//! rust-tts-wrapper for JavaScript: cloud engines on web APIs.
//!
//! The native `cloud` feature is reqwest::blocking + tokio — impossible
//! under wasm. This crate is the async-first twin for the browser: the
//! same provider request shapes and SpeechMarkdown pipeline, on `fetch`.
//! floravox offline synthesis lives in the floravox wasm engine
//! (floravox-web); native-only engines (sapi/avsynth/sherpaonnx) are out
//! of scope by design.

use wasm_bindgen::prelude::*;

#[cfg(feature = "edge")]
mod edge;

/// Speak through Microsoft Edge "Read Aloud" — feature-gated off the
/// browser surface (Microsoft closes browser-origin handshakes; see
/// src/edge.rs). Enable the `edge` feature + a proxy if ever needed.
#[cfg(feature = "edge")]
#[wasm_bindgen]
pub async fn edge_speak(voice: String, rate: f32, pitch: f32, text: String) -> Result<JsValue, JsError> {
    let out = edge::edge_speak(&voice, rate, pitch, &text).await?;
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsError::new(&e.to_string()))
}

/// Yield to the browser event loop once (setTimeout 0).
pub(crate) async fn next_tick() -> Result<(), JsError> {
    let w = web_sys::window().ok_or_else(|| JsError::new("no window"))?;
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let resolve = resolve.clone();
        let cb: js_sys::Function = Closure::once_into_js(move |_: JsValue| {
            let _ = resolve.call0(&JsValue::NULL);
        })
        .unchecked_into();
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&cb, 0);
    });
    wasm_bindgen_futures::JsFuture::from(promise).await.map(|_| ()).map_err(|e| JsError::new(&format!("tick: {e:?}")))
}

#[derive(serde::Deserialize)]
pub struct CloudRequest {
    /// "elevenlabs" | "azure" (google/gemini/polly on the way)
    pub provider: String,
    /// provider credentials: {"api_key": "..."} / azure: {"key": "...", "region": "..."}
    pub credentials: serde_json::Value,
    /// voice id (elevenlabs voice_id, azure short name like en-US-AriaNeural)
    pub voice: Option<String>,
    /// optional model id (elevenlabs eleven_multilingual_v2, ...)
    pub model: Option<String>,
    /// output format hint for azure (default audio-24khz-48kbitrate-mono-mp3)
    pub format: Option<String>,
}

#[derive(serde::Serialize)]
pub struct CloudAudio {
    /// synthesized audio bytes (mp3 for both providers today)
    pub audio: Vec<u8>,
    pub mime: String,
}

fn window() -> Result<web_sys::Window, JsError> {
    web_sys::window().ok_or_else(|| JsError::new("no window — browser only"))
}

async fn fetch_bytes(method: &str, url: &str, headers: &[(&str, &str)], body: Option<String>) -> Result<(Vec<u8>, u16, String), JsError> {
    let w = window()?;
    let mut init = web_sys::RequestInit::new();
    init.method(method);
    if let Some(b) = &body {
        init.body(Some(&JsValue::from_str(b)));
    }
    let req = web_sys::Request::new_with_str_and_init(url, &init)
        .map_err(|e| JsError::new(&format!("request: {e:?}")))?;
    for (k, v) in headers {
        req.headers().set(k, v).map_err(|e| JsError::new(&format!("header {k}: {e:?}")))?;
    }
    let resp = wasm_bindgen_futures::JsFuture::from(w.fetch_with_request(&req))
        .await
        .map_err(|e| JsError::new(&format!("fetch: {}", js_err_str(&e))))?;
    let resp: web_sys::Response = resp.into();
    let status = resp.status();
    let mime = resp.headers().get("content-type").unwrap_or_default().unwrap_or_else(|| "audio/mpeg".into());
    let buf_p = resp.array_buffer().map_err(|e| JsError::new(&format!("body: {e:?}")))?;
    let buf = wasm_bindgen_futures::JsFuture::from(buf_p)
        .await
        .map_err(|e| JsError::new(&format!("body: {}", js_err_str(&e))))?;
    let js_buf = js_sys::ArrayBuffer::from(buf);
    let u8a = js_sys::Uint8Array::new(&js_buf);
    Ok((u8a.to_vec(), status, mime))
}

fn js_err_str(v: &JsValue) -> String {
    v.as_string().unwrap_or_else(|| format!("{v:?}"))
}

/// Synthesize through a cloud provider. `request_json` is a serialized
/// [`CloudRequest`]; `text` may be plain text, SSML, or SpeechMarkdown
/// (detected and compiled per provider, mirroring the native pipeline).
#[wasm_bindgen]
pub async fn cloud_speak(request_json: String, text: &str) -> Result<JsValue, JsError> {
    let req: CloudRequest = serde_json::from_str(&request_json).map_err(|e| JsError::new(&e.to_string()))?;
    let audio = match req.provider.as_str() {
        "elevenlabs" => elevenlabs(&req, text).await?,
        "azure" => azure(&req, text).await?,
        other => return Err(JsError::new(&format!("provider '{other}' not yet in the js crate — coming: google/gemini/polly/edge/qwen"))),
    };
    serde_wasm_bindgen::to_value(&audio).map_err(|e| JsError::new(&e.to_string()))
}

/// Text per the native pipeline: SpeechMarkdown -> provider dialect;
/// plain text passes through; SSML is accepted by azure only.
fn prepare_text(req: &CloudRequest, text: &str) -> String {
    let looks_smd = text.contains('[') && text.contains(']');
    let platform = match req.provider.as_str() {
        "elevenlabs" => {
            let m = req.model.as_deref().unwrap_or("");
            if m.starts_with("eleven_v3") || m.starts_with("eleven_v4") { "elevenlabs-v3" } else { "elevenlabs" }
        }
        "azure" => "azure",
        _ => "w3c",
    };
    if looks_smd {
        if let Ok(ssml) = speechmarkdown_rust::SpeechMarkdownParser::to_ssml(text, speechmarkdown_rust::Platform::from_platform_str(platform).unwrap_or(speechmarkdown_rust::Platform::W3c)) {
            return ssml;
        }
    }
    text.to_string()
}

async fn elevenlabs(req: &CloudRequest, text: &str) -> Result<CloudAudio, JsError> {
    let key = req.credentials["api_key"].as_str().ok_or_else(|| JsError::new("credentials.api_key required"))?;
    let voice = req.voice.clone().unwrap_or_else(|| "21m00Tcm4TlvDq8ikWAM".into()); // Rachel
    let model = req.model.clone().unwrap_or_else(|| "eleven_multilingual_v2".into());
    let prompt = prepare_text(req, text);
    // v3/v4 dialect is plain text; other models accept SSML-ish with <break>
    let body = serde_json::json!({
        "text": prompt,
        "model_id": model,
    });
    let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}");
    let (bytes, status, _) = fetch_bytes("POST", &url, &[("xi-api-key", key), ("Content-Type", "application/json")], Some(body.to_string())).await?;
    if status != 200 {
        return Err(JsError::new(&format!("elevenlabs HTTP {status}: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(300)]))));
    }
    Ok(CloudAudio { audio: bytes, mime: "audio/mpeg".into() })
}

async fn azure(req: &CloudRequest, text: &str) -> Result<CloudAudio, JsError> {
    let key = req.credentials["key"].as_str().ok_or_else(|| JsError::new("credentials.key required"))?;
    let region = req.credentials["region"].as_str().ok_or_else(|| JsError::new("credentials.region required"))?;
    let voice = req.voice.clone().unwrap_or_else(|| "en-US-AriaNeural".into());
    let format = req.format.clone().unwrap_or_else(|| "audio-24khz-48kbitrate-mono-mp3".into());
    let ssml = if text.trim_start().starts_with('<') {
        text.to_string()
    } else {
        let inner = prepare_text(req, text);
        let inner = inner.trim().trim_start_matches("<speak>").trim_end_matches("</speak>").to_string();
        let inner = if inner.starts_with('<') { inner } else { crate_escape(&inner) };
        format!(r#"<speak xmlns="http://www.w3.org/2001/10/synthesis" xmlns:mstts="https://www.w3.org/2001/mstts" version="1.0" xml:lang="en-US"><voice name="{voice}">{inner}</voice></speak>"#)
    };
    let url = format!("https://{region}.tts.speech.microsoft.com/cognitiveservices/v1?api-version=2024-11-01&format={format}");
    let (bytes, status, mime) = fetch_bytes("POST", &url, &[("Ocp-Apim-Subscription-Key", key), ("Content-Type", "application/ssml+xml"), ("X-Microsoft-OutputFormat", &format)], Some(ssml)).await?;
    if status != 200 {
        return Err(JsError::new(&format!("azure HTTP {status}: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(300)]))));
    }
    let mime = if mime.starts_with("audio") { mime } else { "audio/mpeg".into() };
    Ok(CloudAudio { audio: bytes, mime })
}

fn crate_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// ---------------------------------------------------------------- floravox
// Re-export the floravox engine surface so JS loads ONE wasm module: the
// wrapper's binary embeds the engine (single registry of voices/langs).
// Loading the standalone floravox_wasm.js alongside would create a second,
// separate instance whose handles the wrapper cannot see.

pub use floravox_wasm::g2p::g2p_load_lang;
pub use floravox_wasm::g2p::g2p_word;
pub use floravox_wasm::onnx::onnx_init;
pub use floravox_wasm::engine::student_stack_load;
pub use floravox_wasm::engine::student_stack_run;
pub use floravox_wasm::engine::teacher_load;
pub use floravox_wasm::engine::teacher_run;
pub use floravox_wasm::orchestrate::voice_load_student;
pub use floravox_wasm::orchestrate::voice_speak;
pub use floravox_wasm::smd::smd_parse;

// ---------------------------------------------------------------- unified
// One speak() for every engine: offline floravox (onnxruntime-web) and the
// cloud providers, dispatched by config.

#[derive(serde::Deserialize)]
struct UnifiedConfig {
    /// "floravox" (offline) | "elevenlabs" | "azure" | ...
    engine: String,
    /// floravox: voice handle from floravox_wasm::voice_load_student
    voice: Option<usize>,
    /// cloud: provider request (credentials etc.)
    credentials: Option<serde_json::Value>,
    #[allow(dead_code)]
    model: Option<String>,
}

/// The one entry point: speak `text` (plain, SSML, or SpeechMarkdown —
/// each engine compiles it per its dialect) through the configured engine.
/// Returns { audio: Float32Array|Uint8Array, sr, spans, marks } for
/// floravox or { audio: Uint8Array, mime } for cloud.
#[wasm_bindgen]
pub async fn speak(config_json: String, text: String) -> Result<JsValue, JsError> {
    let cfg: UnifiedConfig = serde_json::from_str(&config_json).map_err(|e| JsError::new(&e.to_string()))?;
    match cfg.engine.as_str() {
        "floravox" => {
            let voice = cfg.voice.ok_or_else(|| JsError::new("floravox engine needs a voice handle (voice_load_student)"))?;
            // SpeechMarkdown compiles to SSML (the engine's input); plain
            // text and SSML pass through untouched.
            let text = if text.contains('[') && text.contains(']') {
                match floravox_wasm::smd::smd_parse(&text) {
                    Ok(v) => {
                        let ssml = js_sys::Reflect::get(&v, &JsValue::from_str("ssml"))
                            .ok()
                            .and_then(|s| s.as_string())
                            .unwrap_or(text);
                        ssml
                    }
                    Err(_) => text,
                }
            } else {
                text
            };
            floravox_wasm::orchestrate::voice_speak(voice, &text).await
        }
        "elevenlabs" | "azure" => {
            let creds = cfg.credentials.unwrap_or(serde_json::Value::Null);
            let req = serde_json::json!({
                "provider": cfg.engine,
                "credentials": creds,
                "voice": null,
                "model": cfg.model,
            });
            cloud_speak(req.to_string(), &text).await
        }
        other => Err(JsError::new(&format!("unknown engine '{other}'"))),
    }
}
