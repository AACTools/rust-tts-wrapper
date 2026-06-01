//! Shared types used across the crate.

use std::fmt;
use std::os::raw::c_char;

/// A single voice offered by an engine.
#[derive(Debug, Clone)]
pub struct Voice {
    /// Unique voice identifier within the engine.
    pub id: String,
    /// Human-readable voice name.
    pub name: String,
    /// BCP-47 language tag (e.g. `"en-US"`).
    pub language: String,
    /// Gender string (e.g. `"male"`, `"female"`, or empty).
    pub gender: String,
    /// The engine that provides this voice.
    pub engine: String,
}

/// Describes a registered engine for introspection.
#[derive(Debug, Clone)]
pub struct EngineDescriptor {
    /// Unique engine identifier.
    pub id: String,
    /// Human-readable engine name.
    pub name: String,
    /// Whether this engine requires API credentials.
    pub needs_credentials: bool,
    /// JSON array of credential key names, e.g. `r#"["apiKey"]"#`.
    pub credential_keys_json: String,
}

/// Metadata for a Sherpa-ONNX model from the registry.
#[derive(Debug, Clone)]
pub struct SherpaModelInfo {
    /// Model identifier (e.g. `"kokoro-en-en-19"`).
    pub id: String,
    /// Model type (e.g. `"kokoro"`, `"vits"`).
    pub model_type: String,
    /// Human-readable model name.
    pub name: String,
    /// Languages supported by this model.
    pub language: Vec<SherpaLanguage>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of speakers (for multi-speaker models).
    pub num_speakers: u32,
    /// Download URL for the model archive.
    pub url: String,
    /// Whether the archive is compressed.
    pub compression: bool,
    /// Approximate download size in megabytes.
    pub filesize_mb: f64,
}

/// A language entry within a Sherpa-ONNX model.
#[derive(Debug, Clone)]
pub struct SherpaLanguage {
    /// ISO 639 language code.
    pub lang_code: String,
    /// Full language name.
    pub language_name: String,
    /// Country code.
    pub country: String,
}

/// C-compatible voice descriptor returned by [`tts_get_voices`](crate::tts_get_voices).
#[repr(C)]
pub struct tts_voice {
    /// Voice identifier (owned C string).
    pub id: *mut c_char,
    /// Voice name (owned C string).
    pub name: *mut c_char,
    /// Language tag (owned C string).
    pub language: *mut c_char,
    /// Gender (owned C string).
    pub gender: *mut c_char,
    /// Engine identifier (owned C string).
    pub engine: *mut c_char,
}

/// C-compatible engine descriptor returned by [`tts_get_engines`](crate::tts_get_engines).
#[repr(C)]
pub struct tts_engine_info {
    /// Engine identifier (owned C string).
    pub id: *mut c_char,
    /// Engine name (owned C string).
    pub name: *mut c_char,
    /// Whether credentials are required.
    pub needs_credentials: bool,
    /// JSON array of credential key names (owned C string).
    pub credential_keys_json: *mut c_char,
}

/// Error type for TTS operations.
#[derive(Debug)]
pub struct TtsError(pub String);

impl fmt::Display for TtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TtsError {}

impl From<anyhow::Error> for TtsError {
    fn from(e: anyhow::Error) -> Self {
        TtsError(e.to_string())
    }
}

/// Result alias using [`TtsError`].
pub type TtsResult<T> = Result<T, TtsError>;
