//! Core TTS engine trait.

use crate::types::{TtsResult, Voice, WordBoundary};
use std::fmt;

/// Callback for streaming audio chunks.
pub type OnAudioCallback<'a> = &'a mut dyn FnMut(&[u8]);

/// Callback for word boundary events.
pub type OnBoundaryCallback<'a> = &'a mut dyn FnMut(&str, f32, f32);

/// Trait that every TTS engine must implement.
///
/// All methods receive voice, rate, pitch, and volume parameters so each
/// engine can apply them as appropriate.
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

    /// List available voices for this engine.
    fn get_voices(&self) -> TtsResult<Vec<Voice>>;

    /// Return the unique identifier of this engine (e.g. `"system"`, `"sherpaonnx"`).
    fn engine_id(&self) -> &'static str;

    /// Synthesize text to audio bytes (full buffer, no playback).
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

    /// Synthesize text and return word boundary information.
    /// Default implementation estimates boundaries.
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
/// Matches the algorithm used in js-tts-wrapper and swift-tts-wrapper.
#[allow(clippy::cast_precision_loss)]
pub fn estimate_word_boundaries(text: &str) -> Vec<WordBoundary> {
    let words: Vec<&str> = text.split_whitespace().filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return Vec::new();
    }

    let words_per_minute: f64 = 150.0;
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
