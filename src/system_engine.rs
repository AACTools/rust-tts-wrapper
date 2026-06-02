//! System TTS engine via speech-dispatcher (Linux).

use crate::engine::{estimate_word_boundaries, TtsEngine};
use crate::types::{TtsError, TtsResult, Voice};
use std::sync::Mutex;

/// TTS engine that uses the system's speech-dispatcher daemon.
#[derive(Debug)]
pub struct SystemEngine {
    conn: Mutex<Option<speech_dispatcher::Connection>>,
}

impl SystemEngine {
    /// Create a new system engine, connecting to speech-dispatcher.
    pub fn new() -> Self {
        let conn = speech_dispatcher::Connection::open(
            "rust-tts-wrapper",
            "rust-tts-wrapper",
            "rust-tts-wrapper",
            speech_dispatcher::Mode::Threaded,
        )
        .ok();
        SystemEngine {
            conn: Mutex::new(conn),
        }
    }
}

impl TtsEngine for SystemEngine {
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        _rate: f32,
        _pitch: f32,
        _volume: f32,
        _on_audio: Option<crate::engine::OnAudioCallback>,
        mut on_boundary: Option<crate::engine::OnBoundaryCallback>,
    ) -> TtsResult<()> {
        let guard = self.conn.lock().unwrap();
        let conn = guard
            .as_ref()
            .ok_or_else(|| TtsError("Speech dispatcher not connected".into()))?;

        if let Some(v) = voice {
            let _ = conn.set_synthesis_voice_all(v);
        }
        conn.say(speech_dispatcher::Priority::Important, text);

        if let Some(cb) = on_boundary.as_mut() {
            let estimated = estimate_word_boundaries(text);
            for b in &estimated {
                #[allow(clippy::cast_precision_loss)]
                let start = b.offset as f32 / 1000.0;
                #[allow(clippy::cast_precision_loss)]
                let end = (b.offset + b.duration) as f32 / 1000.0;
                cb(&b.text, start, end);
            }
        }

        Ok(())
    }

    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<crate::engine::OnAudioCallback>,
        on_boundary: Option<crate::engine::OnBoundaryCallback>,
    ) -> TtsResult<()> {
        self.speak(text, voice, rate, pitch, volume, on_audio, on_boundary)
    }

    fn stop(&self) -> TtsResult<()> {
        let guard = self.conn.lock().unwrap();
        let conn = guard
            .as_ref()
            .ok_or_else(|| TtsError("Speech dispatcher not connected".into()))?;
        conn.cancel()
            .map_err(|e| TtsError(format!("Stop failed: {e}")))
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        Ok(vec![])
    }

    fn engine_id(&self) -> &'static str {
        "system"
    }
}
