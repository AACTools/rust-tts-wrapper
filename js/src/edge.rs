//! Microsoft Edge "Read Aloud" over the browser WebSocket API.
//!
//! The same protocol the native engine speaks (speech.config + ssml text
//! frames; binary audio frames with 2-byte header prefixes; WordBoundary
//! metadata in Path:response frames), on web-sys instead of tungstenite.
//!
//! Known caveat: browsers cannot set the Origin/User-Agent headers the
//! native client spoofs; whether the endpoint accepts web origins is
//! exactly what the live test determines.

use sha2::{Digest, Sha256};
use wasm_bindgen::prelude::*;
#[allow(unused_imports)]
use wasm_bindgen::JsCast;

const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const WS_BASE: &str = "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";

/// Sec-MS-GEC: FILETIME ticks rounded to 5min, + token, SHA-256 uppercase hex.
fn sec_ms_gec() -> String {
    // wasm32-unknown-unknown has no SystemTime — use the JS clock
    let nanos = (js_sys::Date::now() * 1_000_000.0) as u128;
    let ticks: u128 = nanos / 100 + 116_444_736_000_000_000;
    let rounded = ticks - (ticks % 3_000_000_000);
    let hash = Sha256::digest(format!("{rounded}{TRUSTED_CLIENT_TOKEN}").as_bytes());
    let mut s = String::with_capacity(hash.len() * 2);
    for b in &hash {
        use std::fmt::Write;
        let _ = write!(s, "{b:02X}");
    }
    s
}

fn date_header() -> String {
    // e.g. "Thu Oct 02 2025 10:00:00 GMT+0000 (Coordinated Universal Time)"
    let js_date = js_sys::Date::new_0();
    js_date.to_string().as_string().unwrap_or_default()
}

#[derive(serde::Serialize)]
pub struct EdgeAudio {
    pub audio: Vec<u8>,
    pub mime: String,
    /// (word, offset_ms, duration_ms) from WordBoundary events
    pub boundaries: Vec<(String, f64, f64)>,
}

fn extract_path(text: &str) -> &str {
    text.lines()
        .find(|l| l.starts_with("Path:"))
        .and_then(|l| l.strip_prefix("Path:"))
        .map_or("", str::trim)
}

fn extract_body(text: &str) -> &str {
    if let Some(i) = text.find("\r\n\r\n") {
        &text[i + 4..]
    } else if let Some(i) = text.find("\n\n") {
        &text[i + 2..]
    } else {
        ""
    }
}

/// Speak through Edge Read Aloud. `voice` = Azure short name
/// (en-US-AriaNeural default); `rate`/`pitch` are multipliers (1.0 =
/// normal); text may be SpeechMarkdown (compiled to Azure SSML).
pub async fn edge_speak(voice: &str, rate: f32, pitch: f32, text: &str) -> Result<EdgeAudio, JsError> {
    edge_speak_ws(&format!("{WS_BASE}?TrustedClientToken={TRUSTED_CLIENT_TOKEN}&Sec-MS-GEC={}&Sec-MS-GEC-Version=1-131.0.2903.112&ConnectionId={}", sec_ms_gec(), uuid::Uuid::new_v4().simple()), voice, rate, pitch, text).await
}

/// Test hook: speak against an explicit WS url (mock server).
pub async fn edge_speak_ws(url: &str, voice: &str, rate: f32, pitch: f32, text: &str) -> Result<EdgeAudio, JsError> {
    // SpeechMarkdown -> azure SSML when it looks like markup
    let ssml = if text.contains('[') && text.contains(']') {
        speechmarkdown_rust::SpeechMarkdownParser::to_ssml(
            text,
            speechmarkdown_rust::Platform::MicrosoftAzure,
        )
        .unwrap_or_else(|_| plain_ssml(voice, rate, pitch, text))
    } else {
        plain_ssml(voice, rate, pitch, text)
    };

    let connection_id = uuid::Uuid::new_v4().simple().to_string();

    let ws = web_sys::WebSocket::new(url).map_err(|e| JsError::new(&format!("ws open: {e:?}")))?;
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

    // wait for the handshake: poll readyState, yielding to the event loop
    loop {
        match ws.ready_state() {
            1 => break,                                                        // OPEN
            3 => return Err(JsError::new("ws closed during handshake — the endpoint likely rejects this origin (browsers cannot send the Edge extension Origin header)")),
            _ => crate::next_tick().await?,
        }
    }

    // send speech.config + ssml
    let config = format!(
        "X-Timestamp:{}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"true\"}},\"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\"}}}}}}}}",
        date_header()
    );
    ws.send_with_str(&config).map_err(|e| JsError::new(&format!("config send: {e:?}")))?;
    let request = format!(
        "X-RequestId:{connection_id}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{}Z\r\nPath:ssml\r\n\r\n{ssml}",
        js_sys::Date::now() as i64
    );
    ws.send_with_str(&request).map_err(|e| JsError::new(&format!("ssml send: {e:?}")))?;

    // pump messages until turn.end
    use std::cell::RefCell;
    use std::rc::Rc;
    let audio: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    let boundaries: Rc<RefCell<Vec<(String, f64, f64)>>> = Rc::new(RefCell::new(Vec::new()));
    let done: Rc<RefCell<Option<Result<(), String>>>> = Rc::new(RefCell::new(None));

    let (done_tx, done_rx) = futures_channel::oneshot::channel::<Result<(), String>>();
    let done_tx = Rc::new(RefCell::new(Some(done_tx)));

    {
        let audio = audio.clone();
        let boundaries = boundaries.clone();
        let done = done.clone();
        let done_tx = done_tx.clone();
        let ws_close = ws.clone();
        let onmessage: Closure<dyn FnMut(web_sys::MessageEvent)> = Closure::wrap(Box::new(move |ev: web_sys::MessageEvent| {
            if let Some(buf) = ev.data().dyn_ref::<js_sys::ArrayBuffer>() {
                let u8a = js_sys::Uint8Array::new(buf);
                let bytes = u8a.to_vec();
                if bytes.len() < 2 {
                    return;
                }
                let hdr_len = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
                if bytes.len() < 2 + hdr_len {
                    return;
                }
                audio.borrow_mut().extend_from_slice(&bytes[2 + hdr_len..]);
            } else if let Some(text) = ev.data().as_string() {
                match extract_path(&text) {
                    "turn.end" => {
                        *done.borrow_mut() = Some(Ok(()));
                        let _ = ws_close.close();
                        if let Some(t) = done_tx.borrow_mut().take() {
                            let _ = t.send(Ok(()));
                        }
                    }
                    "response" => {
                        let body = extract_body(&text);
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                            if let Some(err) = v["Error"]["Message"].as_str() {
                                *done.borrow_mut() = Some(Err(err.to_string()));
                                let _ = ws_close.close();
                                if let Some(t) = done_tx.borrow_mut().take() {
                                    let _ = t.send(Err(err.to_string()));
                                }
                                return;
                            }
                            if let Some(metas) = v["Metadata"].as_array() {
                                for m in metas {
                                    if m["Type"].as_str() == Some("WordBoundary") {
                                        let d = &m["Data"];
                                        let word = d["text"]["Text"].as_str().unwrap_or_default().to_string();
                                        let off = d["Offset"].as_f64().unwrap_or(0.0) / 10_000.0;
                                        let dur = d["Duration"].as_f64().unwrap_or(0.0) / 10_000.0;
                                        boundaries.borrow_mut().push((word, off, dur));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }));
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
    }

    {
        let done = done.clone();
        let done_tx = done_tx.clone();
        let onerror: Closure<dyn FnMut(web_sys::ErrorEvent)> = Closure::wrap(Box::new(move |e: web_sys::ErrorEvent| {
            if done.borrow().is_none() {
                let msg = format!("ws error: {}", e.message());
                *done.borrow_mut() = Some(Err(msg.clone()));
                if let Some(t) = done_tx.borrow_mut().take() {
                    let _ = t.send(Err(msg));
                }
            }
        }));
        ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();
    }

    {
        let done = done.clone();
        let done_tx = done_tx.clone();
        let onclose: Closure<dyn FnMut(web_sys::CloseEvent)> = Closure::wrap(Box::new(move |e: web_sys::CloseEvent| {
            if done.borrow().is_none() {
                let msg = format!("ws closed early: {} {}", e.code(), e.reason());
                *done.borrow_mut() = Some(Err(msg.clone()));
                if let Some(t) = done_tx.borrow_mut().take() {
                    let _ = t.send(Err(msg));
                }
            }
        }));
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();
    }

    match done_rx.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(JsError::new(&format!("edge synthesis: {e}"))),
        Err(_) => return Err(JsError::new("edge pump dropped")),
    }

    let audio = audio.borrow().clone();
    let boundaries = boundaries.borrow().clone();
    if audio.is_empty() {
        return Err(JsError::new("edge synthesis produced no audio"));
    }
    Ok(EdgeAudio { audio, mime: "audio/mpeg".into(), boundaries })
}

fn plain_ssml(voice: &str, rate: f32, pitch: f32, text: &str) -> String {
    let esc = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let rate_pct = ((rate - 1.0) * 100.0).round() as i32;
    let pitch_hz = ((pitch - 1.0) * 50.0).round() as i32;
    let rate_s = format!("{rate_pct:+}%");
    let pitch_s = format!("{pitch_hz:+}Hz");
    format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'><voice name='{voice}'><prosody rate='{rate_s}' pitch='{pitch_s}'>{esc}</prosody></voice></speak>"
    )
}
