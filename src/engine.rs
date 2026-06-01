//! Core TTS engine trait.

use crate::types::{TtsResult, Voice};
use std::fmt;

/// Trait that every TTS engine must implement.
///
/// All methods receive voice, rate, pitch, and volume parameters so each
/// engine can apply them as appropriate.
pub type OnAudioCallback<'a> = &'a mut dyn FnMut(&[u8]);
pub type OnBoundaryCallback<'a> = &'a mut dyn FnMut(&str, f32, f32);

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
}
