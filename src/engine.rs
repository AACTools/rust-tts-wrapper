//! Core TTS engine trait.

use crate::types::{SpeakOptions, TtsResult, Voice, WordBoundary};
use std::fmt;

/// Callback for streaming audio chunks.
pub type OnAudioCallback<'a> = &'a mut dyn FnMut(&[u8]);

/// Callback for word boundary events.
pub type OnBoundaryCallback<'a> = &'a mut dyn FnMut(&str, f32, f32);

/// Callback for speech-started events.
pub type OnStartCallback<'a> = &'a mut dyn FnMut();

/// Callback for speech-finished events.
pub type OnEndCallback<'a> = &'a mut dyn FnMut();

/// Callback for error events.
pub type OnErrorCallback<'a> = &'a mut dyn FnMut(&str);

/// Convert speech markdown to SSML if detected, otherwise return text as-is.
/// Returns (processed_text, is_ssml).
#[cfg(feature = "cloud")]
#[must_use]
pub fn preprocess_speech_markdown(text: &str, platform: &str) -> (String, bool) {
    use speechmarkdown_rust::{Platform, SpeechMarkdownParser};

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

#[cfg(not(feature = "cloud"))]
#[must_use]
pub fn preprocess_speech_markdown(text: &str, _platform: &str) -> (String, bool) {
    (text.to_string(), false)
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
