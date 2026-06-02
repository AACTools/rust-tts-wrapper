//! Shared types used across the crate.

use std::collections::HashMap;
use std::fmt;
use std::os::raw::c_char;

/// Voice gender, matching Swift's `UnifiedVoice.Gender`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gender {
    Male,
    Female,
    Unknown,
}

impl fmt::Display for Gender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Male => write!(f, "Male"),
            Self::Female => write!(f, "Female"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Normalize a raw gender string to a typed [`Gender`].
#[must_use]
pub fn normalize_gender(value: &str) -> Gender {
    match value.to_lowercase().as_str() {
        "female" => Gender::Female,
        "male" => Gender::Male,
        _ => Gender::Unknown,
    }
}

/// A language code entry with BCP-47, ISO 639-3, and display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageCode {
    /// BCP-47 language tag (e.g. `"en-US"`).
    pub bcp47: String,
    /// ISO 639-3 language code (e.g. `"eng"`).
    pub iso639_3: String,
    /// Human-readable language name (e.g. `"English (United States)"`).
    pub display: String,
}

/// A single voice offered by an engine, unified across all providers.
/// Mirrors Swift's `UnifiedVoice`.
#[derive(Debug, Clone)]
pub struct Voice {
    /// Unique voice identifier within the engine.
    pub id: String,
    /// Human-readable voice name.
    pub name: String,
    /// Gender of the voice.
    pub gender: Gender,
    /// The engine/provider that provides this voice (e.g. `"azure"`, `"google"`).
    pub provider: String,
    /// Language codes supported by this voice.
    pub language_codes: Vec<LanguageCode>,
}

impl Voice {
    /// Convenience: return the primary (first) BCP-47 language code, or empty string.
    #[must_use]
    pub fn primary_language(&self) -> &str {
        self.language_codes.first().map_or("", |l| l.bcp47.as_str())
    }
}

/// Audio output format, matching Swift's `AudioFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    Wav,
    Ogg,
    Opus,
    Aac,
    Flac,
    Pcm,
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mp3 => write!(f, "mp3"),
            Self::Wav => write!(f, "wav"),
            Self::Ogg => write!(f, "ogg"),
            Self::Opus => write!(f, "opus"),
            Self::Aac => write!(f, "aac"),
            Self::Flac => write!(f, "flac"),
            Self::Pcm => write!(f, "pcm"),
        }
    }
}

/// Named speech rate presets, matching Swift's `SpeechRate`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpeechRate {
    XSlow,
    Slow,
    Medium,
    Fast,
    XFast,
}

impl SpeechRate {
    /// Convert to a float multiplier (1.0 = normal).
    #[must_use]
    pub fn rate_value(self) -> f32 {
        match self {
            Self::XSlow => 0.5,
            Self::Slow => 0.75,
            Self::Medium => 1.0,
            Self::Fast => 1.25,
            Self::XFast => 1.5,
        }
    }
}

/// Named speech pitch presets, matching Swift's `SpeechPitch`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpeechPitch {
    XLow,
    Low,
    Medium,
    High,
    XHigh,
}

impl SpeechPitch {
    /// Convert to a float multiplier (1.0 = normal).
    #[must_use]
    pub fn pitch_value(self) -> f32 {
        match self {
            Self::XLow => 0.5,
            Self::Low => 0.75,
            Self::Medium => 1.0,
            Self::High => 1.25,
            Self::XHigh => 1.5,
        }
    }
}

/// Options for speak/synth calls, matching Swift's `SpeakOptions`.
#[derive(Debug, Clone, Default)]
pub struct SpeakOptions {
    /// Speech rate as a float multiplier (1.0 = normal). Overrides `speech_rate`.
    pub rate: Option<f32>,
    /// Speech rate as a named preset.
    pub speech_rate: Option<SpeechRate>,
    /// Speech pitch as a float multiplier (1.0 = normal). Overrides `speech_pitch`.
    pub pitch: Option<f32>,
    /// Speech pitch as a named preset.
    pub speech_pitch: Option<SpeechPitch>,
    /// Volume (0.0–1.0).
    pub volume: Option<f32>,
    /// Voice identifier.
    pub voice: Option<String>,
    /// Desired audio output format.
    pub format: Option<AudioFormat>,
    /// Whether to preprocess SpeechMarkdown to SSML.
    pub use_speech_markdown: bool,
    /// Whether to request real word boundary events from the API.
    pub use_word_boundary: bool,
    /// If true, pass SSML directly to the engine without wrapping.
    pub raw_ssml: bool,
    /// Engine-specific extra options.
    pub extra: HashMap<String, String>,
}

impl SpeakOptions {
    /// Resolve the effective rate value.
    #[must_use]
    pub fn effective_rate(&self) -> f32 {
        self.rate
            .or_else(|| self.speech_rate.map(SpeechRate::rate_value))
            .unwrap_or(1.0)
    }

    /// Resolve the effective pitch value.
    #[must_use]
    pub fn effective_pitch(&self) -> f32 {
        self.pitch
            .or_else(|| self.speech_pitch.map(SpeechPitch::pitch_value))
            .unwrap_or(1.0)
    }

    /// Resolve the effective volume value.
    #[must_use]
    pub fn effective_volume(&self) -> f32 {
        self.volume.unwrap_or(1.0)
    }
}

/// A word boundary event with timing information.
/// Mirrors Swift's `WordBoundary`.
#[derive(Debug, Clone, PartialEq)]
pub struct WordBoundary {
    /// The spoken word text.
    pub text: String,
    /// Offset from start of audio in milliseconds.
    pub offset: u64,
    /// Duration of the word in milliseconds.
    pub duration: u64,
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
