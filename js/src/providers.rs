//! google / gemini / polly request paths — mirrors of the native engine's
//! shapes, on global fetch.

use crate::{fetch_bytes, CloudAudio};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use base64::Engine as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

type HmacSha256 = Hmac<Sha256>;

/// Google TTS: key in URL, JSON in, base64 audioContent out.
pub async fn google(req: &crate::CloudRequest, text: &str) -> Result<CloudAudio, JsError> {
    let key = req.credentials["apiKey"].as_str().or_else(|| req.credentials["api_key"].as_str())
        .ok_or_else(|| JsError::new("credentials.apiKey required"))?;
    let voice = req.voice.clone().unwrap_or_else(|| "en-US-Wavenet-D".into());
    let body = crate::build_google_body(req, text, &voice);
    let url = format!("https://texttospeech.googleapis.com/v1/text:synthesize?key={key}");
    let (bytes, status, _) = fetch_bytes("POST", &url, &[("Content-Type", "application/json")], Some(body.to_string())).await?;
    if status != 200 {
        return Err(JsError::new(&format!("google HTTP {status}: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(300)]))));
    }
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| JsError::new(&e.to_string()))?;
    let b64 = v["audioContent"].as_str().ok_or_else(|| JsError::new("no audioContent"))?;
    let audio = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(CloudAudio { audio, mime: "audio/mpeg".into() })
}

/// Gemini 3.8 TTS via the Interactions API (x-goog-api-key).
pub async fn gemini(req: &crate::CloudRequest, text: &str) -> Result<CloudAudio, JsError> {
    let key = req.credentials["apiKey"].as_str().or_else(|| req.credentials["api_key"].as_str())
        .ok_or_else(|| JsError::new("credentials.apiKey required"))?;
    let model = req.model.clone()
        .or_else(|| req.credentials["modelId"].as_str().map(String::from))
        .unwrap_or_else(|| "gemini-3.8-flash-tts".into());
    let voice = req.voice.clone()
        .or_else(|| req.credentials["voice"].as_str().map(String::from))
        .unwrap_or_else(|| "Kore".into());
    let body = serde_json::json!({
        "model": model,
        "input": [{ "type": "user_input", "content": [{ "type": "text", "text": text }] }],
        "response_format": { "type": "audio" },
        "generation_config": { "speech_config": [{ "voice": voice }] },
    });
    let url = "https://generativelanguage.googleapis.com/v1beta/interactions";
    let (bytes, status, _) = fetch_bytes("POST", url,
        &[("x-goog-api-key", key), ("Content-Type", "application/json")], Some(body.to_string())).await?;
    if status != 200 {
        return Err(JsError::new(&format!("gemini HTTP {status}: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(300)]))));
    }
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| JsError::new(&e.to_string()))?;
    // last audio block in steps[*].content[*] (data base64, audio/wav)
    let mut last: Option<&str> = None;
    if let Some(steps) = v["steps"].as_array() {
        for step in steps {
            if let Some(content) = step["content"].as_array() {
                for block in content {
                    if block["type"] == "audio" {
                        if let Some(d) = block["data"].as_str() {
                            last = Some(d);
                        }
                    }
                }
            }
        }
    }
    let b64 = last.ok_or_else(|| JsError::new("no audio block in interaction response"))?;
    let audio = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(CloudAudio { audio, mime: "audio/wav".into() })
}

/// AWS Polly: sigv4-signed POST, binary mp3 out.
pub async fn polly(req: &crate::CloudRequest, text: &str) -> Result<CloudAudio, JsError> {
    let key_id = req.credentials["accessKeyId"].as_str().or_else(|| req.credentials["access_key_id"].as_str())
        .ok_or_else(|| JsError::new("credentials.accessKeyId required"))?;
    let secret = req.credentials["secretAccessKey"].as_str().or_else(|| req.credentials["secret_access_key"].as_str())
        .ok_or_else(|| JsError::new("credentials.secretAccessKey required"))?;
    let region = req.credentials["region"].as_str().unwrap_or("us-east-1");
    let voice = req.voice.clone().unwrap_or_else(|| "Joanna".into());
    let engine = req.credentials["engine"].as_str().unwrap_or("neural");

    // SSML when the text carries markup, else plain
    let is_ssml = text.trim_start().starts_with('<');
    let body = serde_json::json!({
        "Engine": engine,
        "OutputFormat": "mp3",
        "Text": text,
        "TextType": if is_ssml { "ssml" } else { "text" },
        "VoiceId": voice,
    });
    let payload = body.to_string();
    let host = format!("polly.{region}.amazonaws.com");
    let url = format!("https://{host}/v1/speech");

    // sigv4
    let now = js_sys::Date::new_0();
    let iso = now.to_iso_string().as_string().unwrap_or_default(); // 2026-10-02T13:00:00.000Z
    let date = iso[..10].replace('-', "");                          // 20261002
    let amz_date = format!("{}T{}Z", date, &iso[11..19].replace(':', "")); // 20261002THHMMSSZ
    let content_sha = hex(&Sha256::digest(payload.as_bytes()));

    let canonical_headers = format!("content-type:application/json\nhost:{host}\nx-amz-date:{amz_date}\n");
    let signed_headers = "content-type;host;x-amz-date";
    let canonical_request = format!(
        "POST\n/v1/speech\n\n{canonical_headers}\n{signed_headers}\n{content_sha}"
    );
    let scope = format!("{date}/{region}/polly/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical_request.as_bytes()))
    );
    let k_date = hmac_bytes(date.as_bytes(), &[b"AWS4".as_slice(), secret.as_bytes()].concat());
    let k_region = hmac_bytes(region.as_bytes(), &k_date);
    let k_service = hmac_bytes(b"polly", &k_region);
    let k_signing = hmac_bytes(b"aws4_request", &k_service);
    let signature = hex(&hmac_bytes(string_to_sign.as_bytes(), &k_signing));
    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={key_id}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );

    let (bytes, status, _) = fetch_bytes("POST", &url, &[
        ("Content-Type", "application/json"),
        ("X-Amz-Date", &amz_date),
        ("Authorization", &auth),
    ], Some(payload)).await?;
    if status != 200 {
        return Err(JsError::new(&format!("polly HTTP {status}: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(300)]))));
    }
    Ok(CloudAudio { audio: bytes, mime: "audio/mpeg".into() })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hmac_bytes(msg: &[u8], key: &[u8]) -> [u8; 32] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac key");
    mac.update(msg);
    mac.finalize().into_bytes().into()
}

