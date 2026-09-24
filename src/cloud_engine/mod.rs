//! Generic cloud TTS engine supporting 19 providers via HTTP APIs.
//!
//! Handles Azure (SSML body), Google (base64 REST), and all other JSON-body
//! providers. Includes voice fetching for engines with list endpoints and
//! word boundary support where APIs provide timing data.

// Thread-local bridge for viseme callbacks. The FFI layer sets this before
// calling speak(); the Azure WS loop reads it when viseme events arrive.
// This avoids a trait-level change to add a viseme callback parameter.
use crate::boundaries::{EstimateFirer, EstimatePlan};
use crate::engine::{estimate_word_boundaries, preprocess_speech_markdown, TtsEngine};
use crate::types::{
    normalize_gender, Gender, LanguageCode, TtsError, TtsResult, Voice, WordBoundary,
};
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(feature = "cloud")]
use {
    tungstenite::client::IntoClientRequest,
    tungstenite::{connect, Message},
    url::Url,
    uuid::Uuid,
};

mod config;
mod decode;
mod edge;
mod elevenlabs;
mod engine;
mod gemini;
mod google;
mod ssml;
mod voices;

// Re-exported so the facade (`crate::cloud_engine::*`) and the tests see
// the moved items with one glob.
pub(crate) use config::*;
pub(crate) use decode::*;
pub(crate) use edge::*;
pub(crate) use engine::*;
pub(crate) use ssml::*;

pub(crate) use engine::create_cloud_engine;
pub use engine::set_viseme_callback;
pub use engine::CloudEngine;
