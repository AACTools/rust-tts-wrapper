//! The pronunciation-override middleware (#125): wraps every engine
//! that does not apply overrides itself (sherpaonnx does — it owns
//! both the respell text substitution and the phoneme-form injection
//! at its G2P seam) so text-form overrides apply uniformly for every
//! consumer of [`crate::factory::create_engine`], including the C ABI
//! and every binding on top of it.
//!
//! Boundary events are remapped: engines report offsets against the
//! REWRITTEN text, callers highlighted against the ORIGINAL — an
//! offset inside a replacement maps to the typed word's span (every
//! spoken word of a respell highlights the word the user typed).
//!
//! SSML-looking input is passed through untouched: substituting
//! inside markup could corrupt tags, and SSML callers can embed
//! `<phoneme>` themselves (#125 prior art).

use crate::engine::{OnAudioCallback, OnBoundaryCallback, OnMarkCallback, TtsEngine};
use crate::types::{SpeakOptions, TtsResult, Voice};
use std::fmt;
use std::sync::Arc;

/// Wraps an engine so user pronunciation overrides (#125) apply to
/// its text inputs, with boundary events remapped to the ORIGINAL
/// text. Engines that apply overrides themselves (sherpaonnx) are
/// never wrapped — see [`crate::factory::create_engine`].
pub struct PronouncingEngine {
    inner: Arc<dyn TtsEngine>,
}

impl PronouncingEngine {
    /// Wrap `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn TtsEngine>) -> Self {
        Self { inner }
    }
}

impl fmt::Debug for PronouncingEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PronouncingEngine")
            .field("inner", &self.inner)
            .finish()
    }
}

/// Heuristic: does this look like markup the middleware must not
/// touch? Angle-bracket-open followed by a letter at the very start.
fn looks_like_markup(text: &str) -> bool {
    let t = text.trim_start();
    let mut chars = t.chars();
    matches!(chars.next(), Some('<')) && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
}

impl PronouncingEngine {
    /// Rewrite `text` under the all-respell policy unless it looks
    /// like markup. Returns the text and the span map for boundary
    /// remapping.
    fn prepare(text: &str) -> crate::pronunciation::Applied {
        use crate::pronunciation::{self, Policy};
        match pronunciation::active() {
            Some(p) if !p.is_empty() && !looks_like_markup(text) => {
                p.apply(text, Policy::AllRespell)
            }
            _ => crate::pronunciation::Applied {
                text: text.to_string(),
                spans: Vec::new(),
            },
        }
    }

    /// Wrap the caller's boundary callback with span remapping.
    fn remap_boundary(
        orig: String,
        new: String,
        spans: Vec<crate::pronunciation::SpanMap>,
        on_boundary: Option<OnBoundaryCallback<'_>>,
    ) -> Option<impl FnMut(&str, f32, f32, i32, i32, bool) + use<'_>> {
        // owned captures: the returned closure must borrow only the
        // callback (RPIT would otherwise capture local lifetimes)
        let cb = on_boundary?;
        Some(
            move |word: &str, start: f32, end: f32, offset: i32, len: i32, is_final: bool| {
                // engine offsets are BYTE indices into `new`; map
                // back to byte indices into `orig`
                let (off, l) =
                    crate::pronunciation::remap_boundary_span(&orig, &new, &spans, offset, len);
                cb(word, start, end, off, l, is_final);
            },
        )
    }
}

impl TtsEngine for PronouncingEngine {
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
        on_mark: Option<OnMarkCallback>,
    ) -> TtsResult<()> {
        let applied = Self::prepare(text);
        let mut remapped = Self::remap_boundary(
            text.to_string(),
            applied.text.clone(),
            applied.spans.clone(),
            on_boundary,
        );
        self.inner.speak(
            &applied.text,
            voice,
            rate,
            pitch,
            volume,
            on_audio,
            remapped
                .as_mut()
                .map(|f| f as &mut dyn FnMut(&str, f32, f32, i32, i32, bool)),
            on_mark,
        )
    }

    fn speak_with_options(
        &self,
        text: &str,
        options: Option<&SpeakOptions>,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
        on_mark: Option<OnMarkCallback>,
    ) -> TtsResult<()> {
        let applied = Self::prepare(text);
        let mut remapped = Self::remap_boundary(
            text.to_string(),
            applied.text.clone(),
            applied.spans.clone(),
            on_boundary,
        );
        self.inner.speak_with_options(
            &applied.text,
            options,
            on_audio,
            remapped
                .as_mut()
                .map(|f| f as &mut dyn FnMut(&str, f32, f32, i32, i32, bool)),
            on_mark,
        )
    }

    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
        on_mark: Option<OnMarkCallback>,
    ) -> TtsResult<()> {
        let applied = Self::prepare(text);
        let mut remapped = Self::remap_boundary(
            text.to_string(),
            applied.text.clone(),
            applied.spans.clone(),
            on_boundary,
        );
        self.inner.speak_sync(
            &applied.text,
            voice,
            rate,
            pitch,
            volume,
            on_audio,
            remapped
                .as_mut()
                .map(|f| f as &mut dyn FnMut(&str, f32, f32, i32, i32, bool)),
            on_mark,
        )
    }

    // The middleware DELEGATES every non-text method explicitly —
    // inheriting the trait's DEFAULTS here would silently replace the
    // inner engine's own semantics (check_credentials_status would
    // downgrade Rejected to Unverified; pause/resume would become
    // no-ops for engines that implement them). Caught by the
    // integration suite (tests/error_detail_ffi.rs) after the lib-only
    // local gate passed.
    fn check_credentials(&self) -> TtsResult<bool> {
        self.inner.check_credentials()
    }

    fn check_credentials_status(&self) -> TtsResult<crate::engine::CredentialCheck> {
        self.inner.check_credentials_status()
    }

    fn pause(&self) -> TtsResult<()> {
        self.inner.pause()
    }

    fn resume(&self) -> TtsResult<()> {
        self.inner.resume()
    }

    fn stop(&self) -> TtsResult<()> {
        self.inner.stop()
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        self.inner.get_voices()
    }

    fn engine_id(&self) -> &'static str {
        self.inner.engine_id()
    }

    fn synth_to_bytes(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<Vec<u8>> {
        let applied = Self::prepare(text);
        self.inner
            .synth_to_bytes(&applied.text, voice, rate, pitch, volume)
    }

    fn synth_to_wav(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<Vec<u8>> {
        let applied = Self::prepare(text);
        self.inner
            .synth_to_wav(&applied.text, voice, rate, pitch, volume)
    }

    fn synth_to_bytes_with_options(
        &self,
        text: &str,
        options: Option<&SpeakOptions>,
    ) -> TtsResult<Vec<u8>> {
        let applied = Self::prepare(text);
        self.inner
            .synth_to_bytes_with_options(&applied.text, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pronunciation::{self, Policy};
    use std::sync::Mutex;

    /// A recording mock engine: captures the text it was given and
    /// fires one boundary event at a chosen offset.
    #[derive(Debug)]
    struct Mock {
        seen: Mutex<Vec<String>>,
        boundary_offset: i32,
    }

    impl TtsEngine for Mock {
        fn speak(
            &self,
            text: &str,
            _voice: Option<&str>,
            _rate: f32,
            _pitch: f32,
            _volume: f32,
            _on_audio: Option<OnAudioCallback>,
            on_boundary: Option<OnBoundaryCallback>,
            _on_mark: Option<OnMarkCallback>,
        ) -> TtsResult<()> {
            self.seen.lock().unwrap().push(text.to_string());
            if let Some(cb) = on_boundary {
                cb("bow", 0.0, 0.1, self.boundary_offset, 3, false);
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
            on_audio: Option<OnAudioCallback>,
            on_boundary: Option<OnBoundaryCallback>,
            on_mark: Option<OnMarkCallback>,
        ) -> TtsResult<()> {
            self.speak(
                text,
                voice,
                rate,
                pitch,
                volume,
                on_audio,
                on_boundary,
                on_mark,
            )
        }

        fn stop(&self) -> TtsResult<()> {
            Ok(())
        }
        fn get_voices(&self) -> TtsResult<Vec<Voice>> {
            Ok(Vec::new())
        }
        fn engine_id(&self) -> &'static str {
            "mock"
        }
    }

    fn env_locked() -> std::sync::MutexGuard<'static, ()> {
        crate::local_models::test_util::env_lock()
    }

    #[test]
    fn substitutes_text_and_remaps_boundaries() {
        let _env = env_locked();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pron.txt");
        std::fs::write(&file, "botox = bow tocks\n").unwrap();
        pronunciation::set_file(Some(file));

        let mock = Arc::new(Mock {
            seen: Mutex::new(Vec::new()),
            boundary_offset: 3, // inside the replacement in rewritten text
        });
        let mw = PronouncingEngine::new(mock.clone());

        let mut events = Vec::new();
        let mut on_b = |word: &str, _s: f32, _e: f32, off: i32, len: i32, _f: bool| {
            events.push((word.to_string(), off, len));
        };
        mw.speak(
            "um Botox end",
            None,
            1.0,
            1.0,
            1.0,
            None,
            Some(&mut on_b),
            None,
        )
        .unwrap();
        assert_eq!(
            mock.seen.lock().unwrap()[0],
            "um bow tocks end",
            "engine received the rewritten text"
        );
        // the boundary at rewritten offset 3 maps to the ORIGINAL
        // Botox span (3..8) — the highlight lands on the typed word
        assert_eq!(events[0], ("bow".to_string(), 3, 5));

        pronunciation::set_file(None);
    }

    #[test]
    fn ssml_looking_text_passes_through() {
        let _env = env_locked();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pron.txt");
        std::fs::write(&file, "speak = shattered\nbotox = bow tocks\n").unwrap();
        pronunciation::set_file(Some(file));

        let mock = Arc::new(Mock {
            seen: Mutex::new(Vec::new()),
            boundary_offset: 0,
        });
        let mw = PronouncingEngine::new(mock.clone());
        mw.speak(
            "<speak>um Botox end</speak>",
            None,
            1.0,
            1.0,
            1.0,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            mock.seen.lock().unwrap()[0],
            "<speak>um Botox end</speak>",
            "markup must not be rewritten"
        );
        pronunciation::set_file(None);
    }

    #[test]
    fn no_file_means_identity() {
        let _env = env_locked();
        pronunciation::set_file(Some(std::path::PathBuf::from("/nonexistent/pron.txt")));
        let mock = Arc::new(Mock {
            seen: Mutex::new(Vec::new()),
            boundary_offset: 0,
        });
        let mw = PronouncingEngine::new(mock.clone());
        mw.speak("plain Botox text", None, 1.0, 1.0, 1.0, None, None, None)
            .unwrap();
        assert_eq!(mock.seen.lock().unwrap()[0], "plain Botox text");
        pronunciation::set_file(None);
    }

    /// The policy split documented on the module: entries carrying a
    /// phoneme form pass through the middleware untouched (a
    /// phoneme-capable front-end owns them). For middleware engines
    /// that means they stay AS WRITTEN — the file-format docs tell
    /// users to add a respell line for engines without phonemes.
    #[test]
    fn phoneme_only_entries_pass_through() {
        let p = pronunciation::parse("botox = /x/\n").0;
        assert_eq!(p.len(), 1);
        let a = p.apply("Botox", Policy::AllRespell);
        assert_eq!(a.text, "Botox");
    }
}
