//! google / gemini / polly request paths — thin transports over the shared
//! `cloud-core` request builders and parsers from the parent crate.

use crate::{fetch_bytes, CloudAudio};
use base64::Engine as _;
use wasm_bindgen::prelude::*;

/// Google TTS: key in URL, JSON in (shared `build_google_request` body),
/// base64 audioContent out.
pub async fn google(req: &crate::CloudRequest, text: &str) -> Result<CloudAudio, JsError> {
    let key = req.credentials["apiKey"].as_str().or_else(|| req.credentials["api_key"].as_str())
        .ok_or_else(|| JsError::new("credentials.apiKey required"))?;
    let voice = req.voice.clone().unwrap_or_else(|| "en-US-Wavenet-D".into());
    let input_ssml = if text.trim_start().starts_with('<') { Some(text.to_string()) } else { None };
    let prepared = match &input_ssml {
        None if text.contains('[') && text.contains(']') => crate::prepare_text(req, text),
        _ => text.to_string(),
    };
    let (body, _words) =
        rust_tts_wrapper::cloud_core::build_google_request(&prepared, &voice, false, input_ssml.as_deref());
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

/// Gemini 3.8 TTS via the Interactions API (x-goog-api-key). Shared
/// `build_gemini_request` body + `parse_gemini_interaction_audio` parser.
pub async fn gemini(req: &crate::CloudRequest, text: &str) -> Result<CloudAudio, JsError> {
    let key = req.credentials["apiKey"].as_str().or_else(|| req.credentials["api_key"].as_str())
        .ok_or_else(|| JsError::new("credentials.apiKey required"))?;
    let model = req.model.clone()
        .or_else(|| req.credentials["modelId"].as_str().map(String::from))
        .unwrap_or_else(|| "gemini-3.8-flash-tts".into());
    let voice = req.voice.clone()
        .or_else(|| req.credentials["voice"].as_str().map(String::from))
        .unwrap_or_else(|| "Kore".into());
    let body = rust_tts_wrapper::cloud_core::build_gemini_request(text, &voice, 1.0, 1.0, 1.0, Some(&model), None);
    let url = "https://generativelanguage.googleapis.com/v1beta/interactions";
    let (bytes, status, _) = fetch_bytes("POST", url,
        &[("x-goog-api-key", key), ("Content-Type", "application/json")], Some(body.to_string())).await?;
    if status != 200 {
        return Err(JsError::new(&format!("gemini HTTP {status}: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(300)]))));
    }
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| JsError::new(&e.to_string()))?;
    let audio = match rust_tts_wrapper::cloud_core::parse_gemini_interaction_audio(&v) {
        rust_tts_wrapper::cloud_core::GeminiAudioBlock::Present(a) => a,
        _ => return Err(JsError::new("no audio block in interaction response")),
    };
    Ok(CloudAudio { audio, mime: "audio/wav".into() })
}

/// AWS Polly: sigv4-signed POST via the shared cloud-core signer, binary
/// mp3 out.
pub async fn polly(req: &crate::CloudRequest, text: &str) -> Result<CloudAudio, JsError> {
    let key_id = req.credentials["accessKeyId"].as_str().or_else(|| req.credentials["access_key_id"].as_str())
        .ok_or_else(|| JsError::new("credentials.accessKeyId required"))?;
    let secret = req.credentials["secretAccessKey"].as_str().or_else(|| req.credentials["secret_access_key"].as_str())
        .ok_or_else(|| JsError::new("credentials.secretAccessKey required"))?;
    let region = req.credentials["region"].as_str().unwrap_or("us-east-1");
    let voice = req.voice.clone().unwrap_or_else(|| "Joanna".into());
    let engine = req.credentials["engine"].as_str().unwrap_or("neural");

    let is_ssml = text.trim_start().starts_with('<');
    let payload = serde_json::json!({
        "Engine": engine,
        "OutputFormat": "mp3",
        "Text": text,
        "TextType": if is_ssml { "ssml" } else { "text" },
        "VoiceId": voice,
    })
    .to_string();
    let host = format!("polly.{region}.amazonaws.com");
    let url = format!("https://{host}/v1/speech");

    // amz-date from the JS clock (wasm has no SystemTime)
    let iso = js_sys::Date::new_0().to_iso_string().as_string().unwrap_or_default();
    let amz_date = format!("{}T{}Z", iso[..10].replace('-', ""), &iso[11..19].replace(':', ""));
    let content_sha = rust_tts_wrapper::cloud_core::sha256_hex(payload.as_bytes());
    let auth = rust_tts_wrapper::cloud_core::authorization_header(
        "POST",
        &rust_tts_wrapper::cloud_core::SignedUrl { host: host.clone(), path: "/v1/speech".into(), query: Vec::new() },
        &rust_tts_wrapper::cloud_core::SigV4Credentials { access_key: key_id, secret_key: secret, region, service: "polly" },
        &amz_date,
        Some("application/json"),
        &content_sha,
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
