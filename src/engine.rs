//! Core TTS engine trait.

use crate::types::{SpeakOptions, TtsResult, Voice, WordBoundary};
use std::fmt;

/// Callback for streaming audio chunks.
pub type OnAudioCallback<'a> = &'a mut dyn FnMut(&[u8]);

/// Callback for word boundary events.
/// Signature: (word, start_sec, end_sec, char_offset, char_len)
/// char_offset/char_len are -1 when the engine doesn't report them.
pub type OnBoundaryCallback<'a> = &'a mut dyn FnMut(&str, f32, f32, i32, i32);

/// Callback for speech-started events.
pub type OnStartCallback<'a> = &'a mut dyn FnMut();

/// Callback for speech-finished events.
pub type OnEndCallback<'a> = &'a mut dyn FnMut();

/// Callback for error events.
pub type OnErrorCallback<'a> = &'a mut dyn FnMut(&str);

/// Convert SpeechMarkdown to SSML when detected, otherwise return the text
/// unchanged. Returns `(processed_text, is_ssml)`.
///
/// `platform` picks the SSML flavour:
///   - `"azure"` → MicrosoftAzure
///   - `"google"` → GoogleAssistant
///   - `"sapi"` / `"avsynth"` / anything else → AmazonAlexa (the closest
///     generic SSML baseline; SAPI's own parser accepts the subset that
///     speechmarkdown-rust emits for Alexa)
///
/// Available whenever the `speechmarkdown` feature is on (auto-enabled by
/// `cloud`, `sapi`, and `avsynth`). Without it, this is a no-op stub.
#[cfg(feature = "speechmarkdown")]
#[must_use]
pub fn preprocess_speech_markdown(text: &str, platform: &str) -> (String, bool) {
    use speechmarkdown_rust::{Platform, SpeechMarkdownParser};

    // If the input is already SSML (starts with <speak), pass it through
    // unchanged and flag it so the engine knows not to escape/wrap it.
    if text.trim_start().to_ascii_lowercase().starts_with("<speak") {
        return (text.to_string(), true);
    }

    if !SpeechMarkdownParser::is_speech_markdown(text) {
        return (text.to_string(), false);
    }

    let platform = match platform {
        "azure" => Platform::MicrosoftAzure,
        "google" => Platform::GoogleAssistant,
        _ => Platform::AmazonAlexa,
    };

    match SpeechMarkdownParser::to_ssml(text, platform) {
        Ok(ssml) => (ssml, true),
        Err(_) => (text.to_string(), false),
    }
}

#[cfg(not(feature = "speechmarkdown"))]
#[must_use]
pub fn preprocess_speech_markdown(text: &str, _platform: &str) -> (String, bool) {
    (text.to_string(), false)
}

/// Strip all XML/SSML tags from `ssml`, returning the plain-text content with
/// collapsed whitespace. Used to convert incoming W3C SSML (from
/// `tts_speak_ssml`) to plain text for engines that don't accept SSML
/// (OpenAI, ElevenLabs, SherpaOnnx, etc.).
#[must_use]
pub fn strip_ssml_to_text(ssml: &str) -> String {
    let mut out = String::with_capacity(ssml.len());
    let mut in_tag = false;
    for ch in ssml.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract the `name` attribute from a `<voice name='...'>` (or `"..."`) tag
/// and return the inner SSML content wrapped in a bare `<speak>` element.
///
/// When the C++ adapter calls `tts_speak_ssml` it wraps content in
/// `<speak version='...' xmlns='...'><voice name='...'>INNER</voice></speak>`.
/// Google/Watson/Polly don't use the `<voice>` wrapper (voice is set via API
/// config), so this strips it and returns `(voice_name, "<speak>INNER</speak>")`.
///
/// Returns `(None, original)` when no `<voice>` tag is found.
#[must_use]
pub fn unwrap_voice_tag(ssml: &str) -> (Option<String>, String) {
    // Find <voice ...> opening tag.
    let Some(open_idx) = ssml.find("<voice ") else {
        return (None, ssml.to_string());
    };
    let Some(tag_offset) = ssml[open_idx..].find('>') else {
        return (None, ssml.to_string());
    };
    let tag_end = open_idx + tag_offset;
    let tag_str = &ssml[open_idx..=tag_end];

    let voice_name = extract_name_attr(tag_str);

    // Content between the voice tag's `>` and `</voice>`.
    let content_start = tag_end + 1;
    let content_end = ssml.find("</voice>").unwrap_or(ssml.len());
    let inner = &ssml[content_start..content_end];

    (voice_name, format!("<speak>{inner}</speak>"))
}

/// Pull `name='value'` or `name="value"` out of an XML tag string.
fn extract_name_attr(tag: &str) -> Option<String> {
    for quote in ['\'', '"'] {
        let needle = format!("name={quote}");
        if let Some(start) = tag.find(&needle) {
            let val_start = start + needle.len();
            if let Some(end) = tag[val_start..].find(quote) {
                return Some(tag[val_start..val_start + end].to_string());
            }
        }
    }
    None
}

/// Trait that every TTS engine must implement.
///
/// Mirrors Swift's `TTSClient` protocol.
#[allow(clippy::missing_errors_doc)]
pub trait TtsEngine: Send + Sync + fmt::Debug {
    /// Start speaking `text` asynchronously.
    #[allow(clippy::too_many_arguments)]
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
    ) -> TtsResult<()>;

    /// Speak with full [`SpeakOptions`], matching Swift's `speak(_:options:)`.
    fn speak_with_options(
        &self,
        text: &str,
        options: Option<&SpeakOptions>,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
    ) -> TtsResult<()> {
        let opts = options.cloned().unwrap_or_default();
        self.speak(
            text,
            opts.voice.as_deref(),
            opts.effective_rate(),
            opts.effective_pitch(),
            opts.effective_volume(),
            on_audio,
            on_boundary,
        )
    }

    /// Speak `text` synchronously, blocking until synthesis completes.
    #[allow(clippy::too_many_arguments)]
    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
    ) -> TtsResult<()>;

    /// Stop any in-progress speech.
    fn stop(&self) -> TtsResult<()>;

    /// Pause speech (default: no-op, engines may override).
    fn pause(&self) -> TtsResult<()> {
        Ok(())
    }

    /// Resume speech (default: no-op, engines may override).
    fn resume(&self) -> TtsResult<()> {
        Ok(())
    }

    /// List available voices for this engine.
    fn get_voices(&self) -> TtsResult<Vec<Voice>>;

    /// Return the unique identifier of this engine (e.g. `"system"`, `"sherpaonnx"`).
    fn engine_id(&self) -> &'static str;

    /// Check whether the configured credentials are valid.
    /// Default: attempt to fetch voices as a validation.
    fn check_credentials(&self) -> TtsResult<bool> {
        match self.get_voices() {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Synthesize text to audio bytes (full buffer, no playback).
    /// Mirrors Swift's `synthToBytes(_:options:)`.
    fn synth_to_bytes(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<Vec<u8>> {
        let mut buf = Vec::new();
        self.speak(
            text,
            voice,
            rate,
            pitch,
            volume,
            Some(&mut |chunk: &[u8]| {
                buf.extend_from_slice(chunk);
            }),
            None,
        )?;
        Ok(buf)
    }

    /// Synthesize with [`SpeakOptions`].
    fn synth_to_bytes_with_options(
        &self,
        text: &str,
        options: Option<&SpeakOptions>,
    ) -> TtsResult<Vec<u8>> {
        let opts = options.cloned().unwrap_or_default();
        self.synth_to_bytes(
            text,
            opts.voice.as_deref(),
            opts.effective_rate(),
            opts.effective_pitch(),
            opts.effective_volume(),
        )
    }

    /// Synthesize text and return word boundary information.
    fn synth_with_boundaries(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<(Vec<u8>, Vec<WordBoundary>)> {
        let audio = self.synth_to_bytes(text, voice, rate, pitch, volume)?;
        let boundaries = estimate_word_boundaries(text);
        Ok((audio, boundaries))
    }
}

/// Estimate word boundaries using word-length-adjusted timing.
/// Mirrors Swift's `WordTimingEstimator.estimate(text:wordsPerMinute:)`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn estimate_word_boundaries(text: &str) -> Vec<WordBoundary> {
    estimate_word_boundaries_with_wpm(text, 150.0)
}

/// Estimate word boundaries with configurable words per minute.
/// Matches Swift's `WordTimingEstimator.estimate(text:wordsPerMinute:)`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn estimate_word_boundaries_with_wpm(text: &str, words_per_minute: f64) -> Vec<WordBoundary> {
    let words: Vec<&str> = text.split_whitespace().filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return Vec::new();
    }

    let ms_per_word = 60_000.0 / words_per_minute;

    let mut boundaries = Vec::with_capacity(words.len());
    let mut current_ms: u64 = 0;

    for word in &words {
        let length_factor = (word.len() as f64 / 5.0).clamp(0.5, 2.0);
        let duration = (ms_per_word * length_factor) as u64;
        let duration = duration.max(1);

        boundaries.push(WordBoundary {
            text: (*word).to_string(),
            offset: current_ms,
            duration,
        });
        current_ms += duration;
    }

    boundaries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ssml_to_text_basic() {
        assert_eq!(
            strip_ssml_to_text("<speak>Hello world</speak>"),
            "Hello world"
        );
    }

    #[test]
    fn test_strip_ssml_to_text_with_tags() {
        let ssml = "<speak><voice name='en-US-Aria'>Hello <phoneme alphabet='ipa' ph='wɜːld'>world</phoneme></voice></speak>";
        assert_eq!(strip_ssml_to_text(ssml), "Hello world");
    }

    #[test]
    fn test_strip_ssml_to_text_collapses_whitespace() {
        assert_eq!(
            strip_ssml_to_text("<speak>\n  Hello\n  world\n</speak>"),
            "Hello world"
        );
    }

    #[test]
    fn test_strip_ssml_to_text_plain_passthrough() {
        assert_eq!(strip_ssml_to_text("just plain text"), "just plain text");
    }

    #[test]
    fn test_unwrap_voice_tag_extracts_voice_and_inner() {
        let ssml = "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis'>\
                    <voice name='en-US-Wavenet-D'>Hello <prosody rate='+20%'>fast</prosody></voice>\
                    </speak>";
        let (voice, inner) = unwrap_voice_tag(ssml);
        assert_eq!(voice.as_deref(), Some("en-US-Wavenet-D"));
        assert!(inner.starts_with("<speak>"));
        assert!(inner.contains("Hello"));
        assert!(inner.contains("<prosody"));
        assert!(!inner.contains("<voice"));
    }

    #[test]
    fn test_unwrap_voice_tag_double_quotes() {
        let ssml = "<speak><voice name=\"en-US-AriaNeural\">Hi</voice></speak>";
        let (voice, inner) = unwrap_voice_tag(ssml);
        assert_eq!(voice.as_deref(), Some("en-US-AriaNeural"));
        assert!(inner.contains("Hi"));
    }

    #[test]
    fn test_unwrap_voice_tag_no_voice_returns_original() {
        let ssml = "<speak>Hello world</speak>";
        let (voice, inner) = unwrap_voice_tag(ssml);
        assert!(voice.is_none());
        assert_eq!(inner, ssml);
    }
}
