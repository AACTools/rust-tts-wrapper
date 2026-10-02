//! Generic cloud TTS engine supporting 19 providers via HTTP APIs.
//!
//! Handles Azure (SSML body), Google (base64 REST), and all other JSON-body
//! providers. Includes voice fetching for engines with list endpoints and
//! word boundary support where APIs provide timing data.

// Thread-local bridge for viseme callbacks. The FFI layer sets this before
// calling speak(); the Azure WS loop reads it when viseme events arrive.
// This avoids a trait-level change to add a viseme callback parameter.
#[cfg(feature = "cloud")]
use crate::boundaries::{EstimateFirer, EstimatePlan};
#[cfg(feature = "cloud")]
use crate::engine::{estimate_word_boundaries, preprocess_speech_markdown, TtsEngine};
#[cfg(feature = "cloud")]
use crate::types::{
    normalize_gender, Gender, LanguageCode, TtsError, TtsResult, Voice, WordBoundary,
};
use std::collections::HashMap;
#[cfg(feature = "cloud")]
use std::sync::Arc;

#[cfg(feature = "cloud")]
use {
    tungstenite::client::IntoClientRequest,
    tungstenite::{connect, Message},
    url::Url,
    uuid::Uuid,
};

/// Extract the `Path:` header value from an Azure WS text frame.
///
/// `"Path:turn.end"` → `"turn.end"`. Returns `""` when there is no `Path:`
/// header (defensive — Azure always sends one, but a malformed frame should
/// not panic the loop).
#[must_use]
pub(crate) fn azure_ws_extract_path(text_msg: &str) -> &str {
    text_msg
        .lines()
        .find(|l| l.starts_with("Path:"))
        .and_then(|l| l.strip_prefix("Path:"))
        .map_or("", str::trim)
}

/// Extract the JSON body of an Azure WS text frame.
///
/// Azure separates headers from body with `\r\n\r\n`. Some proxies/servers
/// collapse that to `\n\n`; we accept both. Returns `""` when no separator
/// is present.
#[must_use]
pub(crate) fn azure_ws_extract_body(text_msg: &str) -> &str {
    if let Some(idx) = text_msg.find("\r\n\r\n") {
        &text_msg[idx + 4..]
    } else if let Some(idx) = text_msg.find("\n\n") {
        &text_msg[idx + 2..]
    } else {
        ""
    }
}

/// Pull the synthesis error reason out of a `Path:response` JSON body, if any.
///
/// Azure reports failures as `{"Error": {"Message": "…"}}` (or, rarely, a
/// top-level `reason` string). Returns `None` for non-error responses.
#[must_use]
pub(crate) fn azure_ws_extract_error(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let err = json.get("Error")?;
    let reason = err
        .get("Message")
        .and_then(|v| v.as_str())
        .or_else(|| json.get("reason").and_then(|v| v.as_str()))
        .unwrap_or("Azure synthesis failed");
    Some(reason.to_string())
}

#[cfg(feature = "cloud-core")]
mod config;
#[cfg(feature = "cloud")]
mod decode;
#[cfg(feature = "cloud-core")]
mod edge;
#[cfg(feature = "cloud-core")]
mod elevenlabs;
#[cfg(feature = "cloud")]
mod engine;
#[cfg(feature = "cloud-core")]
mod gemini;
#[cfg(feature = "cloud-core")]
mod google;
#[cfg(feature = "cloud")]
mod qwen;
#[cfg(feature = "cloud-core")]
mod sigv4;
#[cfg(feature = "cloud-core")]
mod ssml;
#[cfg(all(test, feature = "cloud"))]
mod tests;
#[cfg(feature = "cloud")]
mod voices;

// Re-exported so the facade (`crate::cloud_engine::*`) and the tests see
// the moved items with one glob.
#[cfg(feature = "cloud-core")]
pub use config::*;
#[cfg(feature = "cloud")]
pub(crate) use decode::*;
#[cfg(feature = "cloud-core")]
pub use edge::*;
#[cfg(feature = "cloud-core")]
pub use elevenlabs::*;
#[cfg(feature = "cloud-core")]
pub use gemini::*;
#[cfg(feature = "cloud-core")]
pub use google::*;
#[cfg(feature = "cloud")]
pub(crate) use qwen::*;
#[cfg(feature = "cloud-core")]
pub use sigv4::*;
#[cfg(feature = "cloud-core")]
pub use ssml::*;
#[cfg(feature = "cloud")]
pub(crate) use voices::*;

#[cfg(feature = "cloud")]
pub use engine::create_cloud_engine;
#[cfg(feature = "cloud")]
pub use engine::set_viseme_callback;
#[allow(unused_imports)] // external facade: consumers use cloud_engine::CloudEngine
#[cfg(feature = "cloud")]
pub use engine::CloudEngine;
