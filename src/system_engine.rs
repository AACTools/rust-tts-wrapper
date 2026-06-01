//! System TTS engine via speech-dispatcher (Linux).

use crate::engine::TtsEngine;
use crate::types::{TtsError, TtsResult, Voice};
use std::sync::Mutex;

/// TTS engine that uses the system's speech-dispatcher daemon.
///
/// On Linux this connects to speech-dispatcher via its IPC protocol.
/// On creation it attempts to open a connection; if speech-dispatcher
/// is not running, subsequent calls will return errors.
#[derive(Debug)]
pub struct SystemEngine {
    conn: Mutex<Option<speech_dispatcher::Connection>>,
}

impl SystemEngine {
    /// Create a new system engine, connecting to speech-dispatcher.
    ///
    /// If the connection fails (e.g. speech-dispatcher not running),
    /// the engine is still created but speak/stop calls will return errors.
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
    ) -> TtsResult<()> {
        let guard = self.conn.lock().unwrap();
        let conn = guard
            .as_ref()
            .ok_or_else(|| TtsError("Speech dispatcher not connected".into()))?;

        if let Some(v) = voice {
            let _ = conn.set_synthesis_voice_all(v);
        }
        conn.say(speech_dispatcher::Priority::Important, text);
        Ok(())
    }

    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<()> {
        self.speak(text, voice, rate, pitch, volume)
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
