//! The word-align boundary middleware (#109): replaces the 150-wpm
//! proportional boundary estimates with REAL word timings measured
//! from the delivered audio, for engines with no native timing data.
//!
//! The hybrid rule from the issue:
//!
//! 1. **engine-native timings win** — if the wrapped engine fired any
//!    boundary with `estimated=false`, everything it fired is replayed
//!    verbatim;
//! 2. **otherwise the aligner runs** — the synthesized audio is
//!    CTC-aligned against the spoken text (see
//!    [`crate::word_aligner_rt`]) and real (`estimated=false`)
//!    boundaries fire, offsets mapped back into the CALLER's text via
//!    the shared [`crate::word_search::WordSearch`] matcher;
//! 3. **alignment failure falls back** to the engine's estimates —
//!    guessed timings beat no timings.
//!
//! The factory wraps exactly the estimate-only engines (v1:
//! `omnivoice-local` and `qwen3-local` — both fire end-of-speak
//! estimates, so the swallow-and-replay ordering changes nothing for
//! callers; mixed-tier engines like sherpa/floravox are NOT wrapped —
//! delaying their per-voice native events would be a regression).
//! Audio is only accumulated when the caller wants boundaries and an
//! alignment might run — streaming callers without a boundary sink
//! pay nothing.
//!
//! (The module itself is feature-gated: it is useless without the
//! `ort` runner.)
//!
//! Opt-in: the `wordAligner` credential names the aligner ONNX (see
//! `scripts/aligner/README.md`; the MMS weights are CC-BY-NC 4.0 —
//! an optional artifact, nothing embeds it).

use crate::engine::{OnAudioCallback, OnBoundaryCallback, OnMarkCallback, TtsEngine};
use crate::types::{SpeakOptions, TtsResult, Voice};
use crate::word_aligner::WordSpan;
use std::sync::{Arc, Mutex};

/// The alignment seam (mockable in tests; the real implementation is
/// [`crate::word_aligner_rt::WordAligner`]).
pub trait WordAlignSource: Send + Sync {
    /// Align `text` against `waveform` (16 kHz mono f32) → word spans.
    ///
    /// # Errors
    /// When alignment cannot run (model failure, no tokens…).
    fn align_words(&self, waveform: &[f32], text: &str) -> Result<Vec<WordSpan>, String>;
}

#[cfg(feature = "word-align")]
impl WordAlignSource for crate::word_aligner_rt::WordAligner {
    fn align_words(&self, waveform: &[f32], text: &str) -> Result<Vec<WordSpan>, String> {
        self.align_words(waveform, text)
    }
}

/// A recorded boundary event (replayed or replaced after speak).
#[derive(Debug, Clone)]
struct BoundaryEvent {
    word: String,
    start: f32,
    end: f32,
    offset: i32,
    len: i32,
    estimated: bool,
}

/// The sink the middleware records boundary events into.
type Recording = Arc<Mutex<Vec<BoundaryEvent>>>;

/// Wraps an estimate-only engine so its boundaries become real
/// (`estimated=false`) CTC forced alignments of the delivered audio.
pub struct AligningEngine<A: WordAlignSource> {
    inner: Arc<dyn TtsEngine>,
    aligner: A,
    /// The wrapped engine's audio sample rate (for the 16 kHz
    /// resample into the aligner).
    sample_rate: u32,
}

impl<A: WordAlignSource> AligningEngine<A> {
    /// Wrap `inner`; `sample_rate` is the engine's PCM16 output rate.
    #[must_use]
    pub fn new(inner: Arc<dyn TtsEngine>, aligner: A, sample_rate: u32) -> Self {
        Self {
            inner,
            aligner,
            sample_rate,
        }
    }

    /// The shared speak driver for all trait entry points.
    #[allow(clippy::too_many_lines, clippy::type_complexity)]
    fn drive(
        &self,
        text: &str,
        speak_inner: impl FnOnce(Option<OnAudioCallback>, Option<OnBoundaryCallback>) -> TtsResult<()>,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
    ) -> TtsResult<()> {
        // Nothing to align without a boundary sink: forward verbatim,
        // accumulate nothing, zero overhead.
        let Some(boundary_sink) = on_boundary else {
            return speak_inner(on_audio, None);
        };

        // Forward the caller's audio live AND accumulate it for the
        // alignment (only when a sink exists — see above).
        let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let acc = Arc::clone(&audio);
        let fwd_audio: OnAudioCallback = if let Some(mut caller) = on_audio {
            &mut move |pcm: &[u8]| {
                acc.lock().map(|mut a| a.extend_from_slice(pcm)).ok();
                caller(pcm);
            }
        } else {
            let acc = Arc::clone(&audio);
            &mut move |pcm: &[u8]| {
                acc.lock().map(|mut a| a.extend_from_slice(pcm)).ok();
            }
        };

        // Swallow the engine's boundaries; decide after speak.
        let recorded: Arc<Mutex<Vec<BoundaryEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let rec = Arc::clone(&recorded);
        let recording: OnBoundaryCallback =
            &mut move |word: &str, start: f32, end: f32, offset: i32, len: i32, estimated: bool| {
                rec.lock()
                    .map(|mut r| {
                        r.push(BoundaryEvent {
                            word: word.to_string(),
                            start,
                            end,
                            offset,
                            len,
                            estimated,
                        });
                    })
                    .ok();
            };

        speak_inner(Some(fwd_audio), Some(recording))?;

        let events = recorded
            .lock()
            .map_or_default(|mut r| std::mem::take(&mut *r));

        // Hybrid rule 1: the engine knows better — any native timing
        // means replay everything verbatim.
        if events.iter().any(|e| !e.estimated) {
            for e in &events {
                boundary_sink(&e.word, e.start, e.end, e.offset, e.len, e.estimated);
            }
            return Ok(());
        }

        // Hybrid rule 2: align the delivered audio against the spoken
        // text. Derive the spoken text the way the WRAPPED engines do
        // (loop-review finding: they speak expand_ssml_to_text output
        // — a plain strip would mis-align <sub>/<say-as> input while
        // still "succeeding", degrading timings without tripping the
        // fallback).
        let plain = derive_spoken_text(text);
        let pcm = audio.lock().map_or_default(|a| a.clone());
        let aligned = if pcm.is_empty() {
            None
        } else {
            let wave = pcm16_to_f32_16k(&pcm, self.sample_rate);
            self.aligner.align_words(&wave, &plain).ok()
        };

        if let Some(spans) = aligned.filter(|s| !s.is_empty()) {
            // The aligner indexes the NORMALIZED word list; map each
            // fired word back into the caller's original text via the
            // shared matcher (exact → accent-insensitive → hold-last),
            // firing the SOURCE-CASED span so events match the
            // estimate path's shape (loop-review finding).
            let normalized = crate::word_aligner_rt::normalize_text(&plain);
            let words: Vec<&str> = normalized.split(' ').collect();
            let mut search = crate::word_search::WordSearch::new(&plain);
            for span in &spans {
                if let Some(word) = words.get(span.word_index) {
                    let (offset, len) = search.find_next(word);
                    // The matched span, source-cased (fall back to the
                    // normalized word when the matcher missed).
                    let display: String = if offset >= 0 && len > 0 {
                        let end = ((offset + len) as usize).min(plain.len());
                        plain[offset as usize..end].to_string()
                    } else {
                        (*word).to_string()
                    };
                    boundary_sink(
                        &display,
                        span.start_secs,
                        span.end_secs,
                        offset.max(0),
                        len,
                        false,
                    );
                }
            }
            return Ok(());
        }

        // Hybrid rule 3: alignment unavailable → the engine's
        // estimates (guessed beats absent).
        for e in &events {
            boundary_sink(&e.word, e.start, e.end, e.offset, e.len, e.estimated);
        }
        Ok(())
    }
}

/// Derive the text the engine will actually SPEAK (loop-review
/// finding: the wrapped engines expand SSML — `say-as`/`sub` — before
/// speaking; aligning against a plainly-stripped text would
/// mis-align while still "succeeding"). Matches their derivation:
/// `<speak` input expands (with a strip fallback); anything else is
/// passed through verbatim.
#[cfg(feature = "ssml-expand")]
fn derive_spoken_text(text: &str) -> String {
    if text.trim_start().to_lowercase().starts_with("<speak") {
        crate::ssml_expand::expand_ssml_to_text(text)
            .unwrap_or_else(|| crate::engine::strip_ssml_to_text(text))
    } else {
        text.to_string()
    }
}

/// Without the ssml-expand feature the engines themselves strip rather
/// than expand — match that (strip on `<speak` input).
#[cfg(not(feature = "ssml-expand"))]
fn derive_spoken_text(text: &str) -> String {
    if text.trim_start().to_lowercase().starts_with("<speak") {
        crate::engine::strip_ssml_to_text(text)
    } else {
        text.to_string()
    }
}

/// PCM16LE bytes → 16 kHz mono f32 in [-1, 1] (linear resample from
/// `from_rate` when needed).
fn pcm16_to_f32_16k(pcm: &[u8], from_rate: u32) -> Vec<f32> {
    let samples: Vec<f32> = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0)
        .collect();
    if from_rate == 16_000 || samples.is_empty() {
        return samples;
    }
    // Resample counts are tiny (utterance-sized); precision allows are
    // scoped to this function.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let ratio = f64::from(16_000) / f64::from(from_rate);
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let n_out = (samples.len() as f64 * ratio) as usize;
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    (0..n_out)
        .map(|i| {
            let src = i as f64 / ratio;
            let i0 = src.floor() as usize;
            let i1 = (i0 + 1).min(samples.len() - 1);
            let frac = (src - src.floor()) as f32;
            samples[i0] * (1.0 - frac) + samples[i1] * frac
        })
        .collect()
}

impl<A: WordAlignSource + 'static> TtsEngine for AligningEngine<A> {
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
        self.drive(
            text,
            |audio, boundary| {
                self.inner
                    .speak(text, voice, rate, pitch, volume, audio, boundary, on_mark)
            },
            on_audio,
            on_boundary,
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
        self.drive(
            text,
            |audio, boundary| {
                self.inner
                    .speak_with_options(text, options, audio, boundary, on_mark)
            },
            on_audio,
            on_boundary,
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
        self.drive(
            text,
            |audio, boundary| {
                self.inner
                    .speak_sync(text, voice, rate, pitch, volume, audio, boundary, on_mark)
            },
            on_audio,
            on_boundary,
        )
    }

    fn stop(&self) -> TtsResult<()> {
        self.inner.stop()
    }

    fn pause(&self) -> TtsResult<()> {
        self.inner.pause()
    }

    fn resume(&self) -> TtsResult<()> {
        self.inner.resume()
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        self.inner.get_voices()
    }

    fn engine_id(&self) -> &'static str {
        self.inner.engine_id()
    }

    fn check_credentials(&self) -> TtsResult<bool> {
        self.inner.check_credentials()
    }

    fn check_credentials_status(&self) -> TtsResult<crate::engine::CredentialCheck> {
        self.inner.check_credentials_status()
    }
}

impl<A: WordAlignSource> std::fmt::Debug for AligningEngine<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AligningEngine")
            .field("inner", &self.inner)
            .field("sample_rate", &self.sample_rate)
            // the aligner itself is opaque (an ort session)
            .field("aligner", &"<model>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock engine that fires canned boundaries and emits canned
    /// PCM16 silence (any nonzero sample count works).
    #[derive(Debug)]
    struct MockEngine {
        events: Vec<BoundaryEvent>,
        pcm_len: usize,
    }

    impl TtsEngine for MockEngine {
        fn speak(
            &self,
            _text: &str,
            _voice: Option<&str>,
            _rate: f32,
            _pitch: f32,
            _volume: f32,
            on_audio: Option<OnAudioCallback>,
            on_boundary: Option<OnBoundaryCallback>,
            _on_mark: Option<OnMarkCallback>,
        ) -> TtsResult<()> {
            if let Some(cb) = on_audio {
                // 0.1 s of near-silence at 24 kHz (nonzero so the
                // middleware tries alignment).
                let pcm: Vec<u8> = vec![0u8, 0, 1, 0]
                    .into_iter()
                    .cycle()
                    .take(self.pcm_len)
                    .collect();
                cb(&pcm);
            }
            if let Some(cb) = on_boundary {
                for e in &self.events {
                    cb(&e.word, e.start, e.end, e.offset, e.len, e.estimated);
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
            Ok(vec![])
        }
        fn engine_id(&self) -> &'static str {
            "mock"
        }
    }

    /// The recorded-boundary shape the test sinks collect.
    type Collected = Vec<(String, f32, f32, bool)>;

    /// A mock aligner with canned spans.
    struct MockAligner {
        spans: Result<Vec<WordSpan>, String>,
    }

    impl WordAlignSource for MockAligner {
        fn align_words(&self, _waveform: &[f32], _text: &str) -> Result<Vec<WordSpan>, String> {
            self.spans.clone()
        }
    }

    fn est(word: &str, start: f32, end: f32) -> BoundaryEvent {
        BoundaryEvent {
            word: word.into(),
            start,
            end,
            offset: -1,
            len: -1,
            estimated: true,
        }
    }

    fn native(word: &str, start: f32, end: f32) -> BoundaryEvent {
        BoundaryEvent {
            word: word.into(),
            start,
            end,
            offset: 1,
            len: 2,
            estimated: false,
        }
    }

    #[test]
    fn estimates_replaced_by_alignment() {
        // Engine fires estimates only + aligner succeeds → real
        // boundaries with estimated=false.
        let mock = Arc::new(MockEngine {
            events: vec![est("hello", 0.0, 0.6), est("world", 0.6, 1.0)],
            pcm_len: 8_000,
        });
        let engine = AligningEngine::new(
            mock,
            MockAligner {
                spans: Ok(vec![
                    WordSpan {
                        word_index: 0,
                        start_secs: 0.05,
                        end_secs: 0.25,
                    },
                    WordSpan {
                        word_index: 1,
                        start_secs: 0.30,
                        end_secs: 0.55,
                    },
                ]),
            },
            24_000,
        );
        let got: Arc<Mutex<Collected>> = Arc::new(Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        let mut sink = |w: &str, s: f32, e: f32, _o: i32, _l: i32, estd: bool| {
            g.lock().map(|mut v| v.push((w.into(), s, e, estd))).ok();
        };
        engine
            .speak(
                "hello world",
                None,
                1.0,
                1.0,
                1.0,
                None,
                Some(&mut sink),
                None,
            )
            .unwrap();
        let got = got.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                ("hello".into(), 0.05, 0.25, false),
                ("world".into(), 0.30, 0.55, false),
            ]
        );
    }

    #[test]
    fn native_boundaries_win() {
        let mock = Arc::new(MockEngine {
            events: vec![est("hello", 0.0, 0.6), native("world", 0.6, 0.9)],
            pcm_len: 8_000,
        });
        let engine = AligningEngine::new(
            mock,
            MockAligner {
                spans: Ok(vec![WordSpan {
                    word_index: 0,
                    start_secs: 9.9,
                    end_secs: 9.9,
                }]),
            },
            24_000,
        );
        let got: Arc<Mutex<Collected>> = Arc::new(Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        let mut sink = |w: &str, s: f32, e: f32, _o: i32, _l: i32, estd: bool| {
            g.lock().map(|mut v| v.push((w.into(), s, e, estd))).ok();
        };
        engine
            .speak(
                "hello world",
                None,
                1.0,
                1.0,
                1.0,
                None,
                Some(&mut sink),
                None,
            )
            .unwrap();
        let got = got.lock().unwrap().clone();
        // Replay verbatim — including the estimate.
        assert_eq!(
            got,
            vec![
                ("hello".into(), 0.0, 0.6, true),
                ("world".into(), 0.6, 0.9, false),
            ]
        );
    }

    #[test]
    fn alignment_failure_falls_back_to_estimates() {
        let mock = Arc::new(MockEngine {
            events: vec![est("hello", 0.0, 0.6)],
            pcm_len: 8_000,
        });
        let engine = AligningEngine::new(
            mock,
            MockAligner {
                spans: Err("model exploded".into()),
            },
            24_000,
        );
        let got: Arc<Mutex<Collected>> = Arc::new(Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        let mut sink = |w: &str, s: f32, e: f32, _o: i32, _l: i32, estd: bool| {
            g.lock().map(|mut v| v.push((w.into(), s, e, estd))).ok();
        };
        engine
            .speak(
                "hello world",
                None,
                1.0,
                1.0,
                1.0,
                None,
                Some(&mut sink),
                None,
            )
            .unwrap();
        assert_eq!(
            got.lock().unwrap().clone(),
            vec![("hello".into(), 0.0, 0.6, true)]
        );
    }

    #[test]
    fn no_boundary_sink_no_accumulation() {
        // OnAudio still forwards verbatim; nothing to assert beyond
        // "it works" — the audio forward path is exercised above.
        let mock = Arc::new(MockEngine {
            events: vec![est("x", 0.0, 0.1)],
            pcm_len: 8_000,
        });
        let engine = AligningEngine::new(
            mock,
            MockAligner {
                spans: Err("never called".into()),
            },
            24_000,
        );
        let mut audio_seen = 0usize;
        let mut sink = |pcm: &[u8]| audio_seen += pcm.len();
        engine
            .speak("hello", None, 1.0, 1.0, 1.0, Some(&mut sink), None, None)
            .unwrap();
        assert!(audio_seen > 0, "audio forwarded");
    }

    #[test]
    fn engine_fires_nothing_aligner_is_the_source() {
        // Empty events + successful alignment -> the aligner's
        // boundaries are the only source (nothing to replay).
        let mock = Arc::new(MockEngine {
            events: vec![],
            pcm_len: 8_000,
        });
        let engine = AligningEngine::new(
            mock,
            MockAligner {
                spans: Ok(vec![WordSpan {
                    word_index: 0,
                    start_secs: 0.10,
                    end_secs: 0.40,
                }]),
            },
            24_000,
        );
        let got: Arc<Mutex<Collected>> = Arc::new(Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        let mut sink = |w: &str, s: f32, e: f32, _o: i32, _l: i32, estd: bool| {
            g.lock().map(|mut v| v.push((w.into(), s, e, estd))).ok();
        };
        engine
            .speak("hello", None, 1.0, 1.0, 1.0, None, Some(&mut sink), None)
            .unwrap();
        // The display is the normalized word (lowercase — the mock
        // text is already lowercase so the source-cased slice matches).
        assert_eq!(
            got.lock().unwrap().clone(),
            vec![("hello".into(), 0.10, 0.40, false)]
        );
    }

    #[test]
    fn ssml_input_expands_like_the_engines() {
        // <say-as cardinal> 42 </say-as> expands the way the wrapped
        // engines expand it, so the aligned words match the SPOKEN
        // text (forty two, not 42).
        let mock = Arc::new(MockEngine {
            events: vec![],
            pcm_len: 8_000,
        });
        let engine = AligningEngine::new(
            mock,
            MockAligner {
                spans: Ok(vec![
                    WordSpan {
                        word_index: 0,
                        start_secs: 0.0,
                        end_secs: 0.2,
                    },
                    WordSpan {
                        word_index: 1,
                        start_secs: 0.2,
                        end_secs: 0.4,
                    },
                ]),
            },
            24_000,
        );
        let got: Arc<Mutex<Collected>> = Arc::new(Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        let mut sink = |w: &str, s: f32, e: f32, _o: i32, _l: i32, estd: bool| {
            g.lock().map(|mut v| v.push((w.into(), s, e, estd))).ok();
        };
        engine
            .speak(
                "<speak>Hello <say-as interpret-as=\"cardinal\">42</say-as></speak>",
                None,
                1.0,
                1.0,
                1.0,
                None,
                Some(&mut sink),
                None,
            )
            .unwrap();
        let got = got.lock().unwrap().clone();
        // With ssml-expand the text becomes "Hello forty two" — both
        // mock spans fire. Without it the strip gives "Hello 42" and
        // the matcher still finds the first word; either way the
        // FIRST boundary is the spoken "Hello" (not the tag).
        assert!(!got.is_empty(), "expected boundaries from SSML input");
        assert_eq!(got[0].0, "Hello");
        assert!(!got[0].3, "real boundaries");
    }

    #[test]
    fn resample_24k_to_16k() {
        // 1 second of 24 kHz → 16 000 samples at 16 kHz.
        let pcm: Vec<u8> = vec![0u8; 48_000];
        let out = pcm16_to_f32_16k(&pcm, 24_000);
        assert_eq!(out.len(), 16_000);
        // Identity at 16k.
        let pcm: Vec<u8> = vec![0u8; 32];
        assert_eq!(pcm16_to_f32_16k(&pcm, 16_000).len(), 16);
    }
}
