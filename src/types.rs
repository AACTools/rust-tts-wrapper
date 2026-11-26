//! Shared types used across the crate.

use std::collections::HashMap;
use std::fmt;
use std::os::raw::c_char;

use serde::Serialize;

/// Voice gender, matching Swift's `UnifiedVoice.Gender`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gender {
    /// Male voice.
    Male,
    /// Female voice.
    Female,
    /// Not reported by the provider.
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

/// Map a BCP-47 locale code to a human-readable display name.
///
/// Uses ICU4X (Unicode-maintained, covers ~8000 locales) when the
/// `display_names` feature is enabled, producing consistent output like
/// `"Afrikaans (South Africa)"` from `"af-ZA"` regardless of which engine
/// provided the voice. Falls back to the raw code when the feature is off.
#[cfg(feature = "display_names")]
#[must_use]
pub fn locale_display_name(bcp47: &str) -> String {
    use icu_displaynames::{DisplayNamesOptions, LocaleDisplayNamesFormatter};

    // Parse the BCP-47 code into a Locale (e.g. "af-ZA" → Locale).
    let Ok(locale) = bcp47.parse::<icu_locid::Locale>() else {
        return bcp47.to_string();
    };

    // Create a formatter that produces English display names, then format.
    let Ok(formatter) = LocaleDisplayNamesFormatter::try_new(
        &icu_locid::locale!("en").into(),
        DisplayNamesOptions::default(),
    ) else {
        return bcp47.to_string();
    };

    let name = formatter.of(&locale);
    if name.is_empty() {
        bcp47.to_string()
    } else {
        name.into_owned()
    }
}

/// Fallback without the `display_names` feature: the raw code itself.
#[cfg(not(feature = "display_names"))]
#[must_use]
pub fn locale_display_name(bcp47: &str) -> String {
    bcp47.to_string()
}

/// Audio output format, matching Swift's `AudioFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    /// MPEG audio.
    Mp3,
    /// RIFF/WAVE (PCM).
    Wav,
    /// Ogg container.
    Ogg,
    /// Opus in Ogg.
    Opus,
    /// AAC (ADTS).
    Aac,
    /// Free Lossless Audio Codec.
    Flac,
    /// Raw PCM samples.
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
    /// Half speed (×0.5).
    XSlow,
    /// Slow (×0.75).
    Slow,
    /// Normal (×1.0).
    Medium,
    /// Fast (×1.25).
    Fast,
    /// Fastest (×1.5).
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
    /// Very low (×0.5).
    XLow,
    /// Low (×0.75).
    Low,
    /// Normal (×1.0).
    Medium,
    /// High (×1.25).
    High,
    /// Highest (×1.5).
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
    /// True when the timings are proportional estimates (unpatched
    /// voices, sherpa-onnx's 150-wpm model); false when measured (cloud
    /// provider timings that track the audio).
    pub estimated: bool,
}

/// One synthesis, measured.
///
/// Every engine is measured the same way, from the outside: when its first
/// audio arrived, when all of it had, and how much there was. Nothing here
/// depends on an engine reporting anything about itself, so a local model and
/// a cloud provider can be put side by side.
///
/// Returned by [`TtsEngine::synth_to_bytes_measured`](crate::engine::TtsEngine::synth_to_bytes_measured),
/// and as JSON by [`tts_synth_to_bytes_measured`](crate::tts_synth_to_bytes_measured)
/// (`wallMs`, `firstAudioMs`, `audioBytes`, `chunks`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesisDiagnostics {
    /// From the call until the engine returned, all the audio delivered.
    pub wall_ms: u64,
    /// Until the first audio arrived: how long a listener waits. `None`
    /// when no audio arrived.
    ///
    /// Well below [`Self::wall_ms`] means the engine streamed, and the
    /// audio could be played as it was made. Close to it means the audio
    /// only came at the end, however the engine then split it up.
    pub first_audio_ms: Option<u64>,
    /// The audio delivered, in bytes.
    pub audio_bytes: u64,
    /// How many times audio was delivered.
    pub chunks: u64,
}

impl SynthesisDiagnostics {
    /// The seconds of audio that `audio_bytes` come to as 16-bit PCM at
    /// `sample_rate`, with `channels` channels. Zero for a zero rate or
    /// channel count.
    #[must_use]
    pub fn audio_seconds_pcm16(&self, sample_rate: u32, channels: u16) -> f64 {
        if sample_rate == 0 || channels == 0 {
            return 0.0;
        }
        // Exact up to 2^53 bytes: any utterance there has ever been.
        #[allow(clippy::cast_precision_loss)]
        let bytes = self.audio_bytes as f64;
        bytes / (2.0 * f64::from(channels) * f64::from(sample_rate))
    }

    /// Seconds spent per second of audio: the real-time factor. `1.0` is as
    /// fast as the audio plays, and less is faster. `None` without audio, or
    /// when the call took no measurable time.
    #[must_use]
    pub fn real_time_factor(&self, audio_seconds: f64) -> Option<f64> {
        if self.wall_ms == 0 || audio_seconds <= 0.0 {
            return None;
        }
        // Exact up to 2^53 ms: about 285,000 years.
        #[allow(clippy::cast_precision_loss)]
        let wall_s = self.wall_ms as f64 / 1000.0;
        Some(wall_s / audio_seconds)
    }
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
    /// Model identifier (e.g. `"kokoro-en-v0_19"`).
    pub id: String,
    /// Model type (e.g. `"kokoro"`, `"vits"`).
    pub model_type: String,
    /// Engine that drives this family (always `"sherpa-onnx"` since the
    /// floravox engine moved to its own project).
    pub engines: String,
    /// Human-readable model name.
    pub name: String,
    /// Languages supported by this model.
    pub language: Vec<SherpaLanguage>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of speakers (for multi-speaker models).
    pub num_speakers: u32,
    /// Quality/variant tier from the registry (e.g. `"x_low"`, `"low"`,
    /// `"medium"`, `"high"`; some entries carry variant markers like
    /// `"int8"` or `"fp16"` instead). Empty when the entry omits it.
    pub quality: String,
    /// Download URL for the model archive.
    pub url: String,
    /// Whether the archive is compressed.
    pub compression: bool,
    /// Approximate download size in megabytes.
    pub filesize_mb: f64,
    /// SPDX licence identifier for the model (e.g. `"MIT"`, `"Apache-2.0"`).
    /// Empty string when the registry entry omits it.
    pub license: String,
    /// Human-readable URL for the full licence text. Empty when omitted.
    pub license_url: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "display_names")]
    fn test_locale_display_name_known() {
        let name = locale_display_name("en-US");
        assert!(
            name.to_lowercase().contains("english"),
            "expected 'English' in display name for en-US, got {name}"
        );
    }

    #[test]
    #[cfg(feature = "display_names")]
    fn test_locale_display_name_non_latin() {
        let name = locale_display_name("ar-EG");
        assert!(
            name.contains("Arabic") || name.contains("ar-EG"),
            "expected Arabic in display name, got {name}"
        );
    }

    #[test]
    #[cfg(feature = "display_names")]
    fn test_locale_display_name_unknown_falls_back() {
        let name = locale_display_name("xx-YY");
        assert!(!name.is_empty(), "voice name must not be empty");
    }

    #[test]
    #[cfg(not(feature = "display_names"))]
    fn test_locale_display_name_fallback_is_raw_code() {
        assert_eq!(locale_display_name("en-US"), "en-US");
    }
}

#[cfg(test)]
mod synthesis_diagnostics_tests {
    use super::SynthesisDiagnostics;

    #[test]
    fn pcm16_seconds_follow_the_bytes_the_rate_and_the_channels() {
        let d = SynthesisDiagnostics {
            wall_ms: 1_000,
            first_audio_ms: Some(250),
            audio_bytes: 48_000, // 1 s of mono 16-bit at 24 kHz
            chunks: 1,
        };
        assert!((d.audio_seconds_pcm16(24_000, 1) - 1.0).abs() < 1e-9);
        assert!((d.audio_seconds_pcm16(24_000, 2) - 0.5).abs() < 1e-9);
        assert_eq!(d.audio_seconds_pcm16(0, 1), 0.0);
        assert_eq!(d.audio_seconds_pcm16(24_000, 0), 0.0);
    }

    #[test]
    fn real_time_factor_is_seconds_per_second_of_audio() {
        let d = SynthesisDiagnostics {
            wall_ms: 500,
            first_audio_ms: Some(100),
            audio_bytes: 0,
            chunks: 1,
        };
        // Half a second spent on two seconds of audio: four times faster than real time.
        assert!((d.real_time_factor(2.0).unwrap() - 0.25).abs() < 1e-9);
        assert!(d.real_time_factor(0.0).is_none(), "no audio, no factor");
        let instant = SynthesisDiagnostics { wall_ms: 0, ..d };
        assert!(
            instant.real_time_factor(1.0).is_none(),
            "no time, no factor"
        );
    }

    #[test]
    fn json_is_camel_case_and_says_when_no_audio_arrived() {
        let streamed = SynthesisDiagnostics {
            wall_ms: 1523,
            first_audio_ms: Some(412),
            audio_bytes: 192_048,
            chunks: 37,
        };
        assert_eq!(
            serde_json::to_string(&streamed).unwrap(),
            r#"{"wallMs":1523,"firstAudioMs":412,"audioBytes":192048,"chunks":37}"#
        );
        let silent = SynthesisDiagnostics {
            first_audio_ms: None,
            audio_bytes: 0,
            chunks: 0,
            ..streamed
        };
        assert_eq!(
            serde_json::to_string(&silent).unwrap(),
            r#"{"wallMs":1523,"firstAudioMs":null,"audioBytes":0,"chunks":0}"#
        );
    }
}
