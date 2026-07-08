use crate::engine::{estimate_word_boundaries, TtsEngine};
use crate::types::{TtsError, TtsResult, Voice};
use std::sync::Mutex;

#[derive(Debug)]
pub struct SystemEngine {
    conn: Mutex<Option<speech_dispatcher::Connection>>,
}

impl SystemEngine {
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

fn rate_to_spd(rate: f32) -> i32 {
    ((rate.clamp(0.1, 10.0) - 1.0) * 100.0).round() as i32
}

fn pitch_to_spd(pitch: f32) -> i32 {
    ((pitch.clamp(0.1, 10.0) - 1.0) * 100.0).round() as i32
}

fn volume_to_spd(volume: f32) -> i32 {
    ((volume.clamp(0.0, 2.0) - 1.0) * 100.0).round() as i32
}

impl TtsEngine for SystemEngine {
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
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

        let _ = conn.set_voice_rate_all(rate_to_spd(rate));
        let _ = conn.set_voice_pitch_all(pitch_to_spd(pitch));
        let _ = conn.set_volume_all(volume_to_spd(volume));

        conn.say(speech_dispatcher::Priority::Important, text);

        if let Some(cb) = on_boundary.as_mut() {
            let estimated = estimate_word_boundaries(text);
            let mut search_from = 0usize;
            for b in &estimated {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let char_offset = text[search_from..]
                    .find(&b.text)
                    .map_or(-1, |pos| (search_from + pos) as i32);

                if char_offset >= 0 {
                    search_from = char_offset as usize + b.text.len();
                }
                let start = b.offset as f32 / 1000.0;
                let end = (b.offset + b.duration) as f32 / 1000.0;
                let char_len = b.text.chars().count() as i32;
                cb(&b.text, start, end, char_offset, char_len);
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

    fn pause(&self) -> TtsResult<()> {
        let guard = self.conn.lock().unwrap();
        if let Some(conn) = guard.as_ref() {
            let _ = conn.pause_all();
        }
        Ok(())
    }

    fn resume(&self) -> TtsResult<()> {
        let guard = self.conn.lock().unwrap();
        if let Some(conn) = guard.as_ref() {
            let _ = conn.resume_all();
        }
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        let guard = self.conn.lock().unwrap();
        let conn = guard
            .as_ref()
            .ok_or_else(|| TtsError("Speech dispatcher not connected".into()))?;

        let spd_voices = conn
            .list_synthesis_voices()
            .map_err(|e| TtsError(format!("Failed to list speech-dispatcher voices: {e}")))?;

        // speech-dispatcher doesn't expose gender; map to Unknown.
        let voices = spd_voices
            .into_iter()
            .map(|v| {
                let lang = v.language.clone();
                let iso639 = lang.split(['-', '_']).next().unwrap_or(&lang).to_string();
                Voice {
                    // Use the name as the id; speech-dispatcher identifies
                    // voices by name when set_synthesis_voice_* is called.
                    id: v.name.clone(),
                    name: v.name,
                    gender: crate::types::Gender::Unknown,
                    provider: "system".to_string(),
                    language_codes: vec![crate::types::LanguageCode {
                        bcp47: lang.clone(),
                        iso639_3: iso639,
                        display: lang,
                    }],
                }
            })
            .collect();
        Ok(voices)
    }

    fn engine_id(&self) -> &'static str {
        "system"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_to_spd_normal_is_zero() {
        assert_eq!(rate_to_spd(1.0), 0);
    }

    #[test]
    fn test_rate_to_spd_positive_when_faster() {
        // rate 1.5 -> +50
        assert_eq!(rate_to_spd(1.5), 50);
        assert!(rate_to_spd(1.1) > 0);
    }

    #[test]
    fn test_rate_to_spd_negative_when_slower() {
        // rate 0.5 -> -50
        assert_eq!(rate_to_spd(0.5), -50);
        assert!(rate_to_spd(0.9) < 0);
    }

    #[test]
    fn test_rate_to_spd_clamps_extremes() {
        // Above 10.0 / below 0.1 must clamp, not panic.
        assert_eq!(rate_to_spd(100.0), rate_to_spd(10.0));
        assert_eq!(rate_to_spd(0.0), rate_to_spd(0.1));
    }

    #[test]
    fn test_pitch_to_spd_matches_rate_shape() {
        assert_eq!(pitch_to_spd(1.0), 0);
        assert_eq!(pitch_to_spd(1.5), 50);
        assert_eq!(pitch_to_spd(0.5), -50);
    }

    #[test]
    fn test_volume_to_spd_normal_is_zero() {
        assert_eq!(volume_to_spd(1.0), 0);
    }

    #[test]
    fn test_volume_to_spd_clamped_at_zero_and_two() {
        // Volume range is [0.0, 2.0]; outside that must clamp.
        assert_eq!(volume_to_spd(2.0), 100);
        assert_eq!(volume_to_spd(0.0), -100);
        assert_eq!(volume_to_spd(5.0), volume_to_spd(2.0));
    }
}
