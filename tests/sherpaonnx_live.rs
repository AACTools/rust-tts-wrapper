//! Live SherpaOnnx synthesis tests across multiple model types.
//!
//! These tests exercise actual model inference — they need real model files
//! downloaded to `~/.rust-tts-wrapper/sherpaonnx/`. They are `#[ignore]`-d
//! by default so the normal CI test job (which doesn't pre-download models)
//! skips them. A separate workflow (`.github/workflows/sherpaonnx-live.yml`)
//! downloads small models for 3 different `model_type` values and runs
//! these tests with `-- --ignored`.
//!
//! Run locally with:
//!   cargo test --test sherpaonnx_live --features sherpaonnx -- --ignored
//!
//! Models used (smallest available per family at the time of writing):
//!   vits       — piper-nl-rdh-low          (~25 MB)  Dutch Piper voice
//!                                          (int8/fp16-free: the fp16
//!                                          variants abort in the CPU-only
//!                                          ONNX runtime with an uncatchable
//!                                          Ort::Exception)
//!   matcha     — icefall-fs-ljspeech      (~73 MB)  Matcha-TTS English (LJSpeech)
//!   kokoro     — kokoro-zh_en-int8  (~140 MB) Kokoro multilingual (Linux CI only)
//!   supertonic — supertonic-3-multilingual (~123 MB) Supertonic 3 (31 langs, Linux CI only)
//!
//! Matcha models need a separate vocoder (`hifigan_v2.onnx`) in the base
//! model dir; the workflow downloads it. See `build_matcha_config` for the
//! fallback search.
//!
//! Override with `SHERPA_VITS_MODEL`, `SHERPA_MATCHA_MODEL`, `SHERPA_KOKORO_MODEL`,
//! `SHERPA_SUPERTONIC_MODEL`.

#![allow(clippy::all, clippy::pedantic, clippy::float_cmp)]

use rust_tts_wrapper::engine::TtsEngine;
use rust_tts_wrapper::factory::create_engine;
use std::sync::{Arc, Mutex};

/// Helper to fetch a model id from env or fall back to the default.
fn model_id(env_var: &str, default: &str) -> String {
    std::env::var(env_var).unwrap_or_else(|_| default.to_string())
}

/// Return true when a model is downloaded locally. Kokoro and Supertonic
/// are only downloaded on the Linux CI matrix (to keep macOS/Windows
/// minutes down), so their tests skip cleanly elsewhere.
fn model_present(id: &str) -> bool {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home)
        .join(".rust-tts-wrapper")
        .join("sherpaonnx")
        .join(id)
        .exists()
}

macro_rules! skip_if_missing {
    ($id:expr) => {
        if !model_present(&$id) {
            eprintln!("skipping: model {} not downloaded", $id);
            return;
        }
    };
}

/// Build an engine for the given model id. Panics with a clear message if
/// the model isn't downloaded so the test failure surfaces the reason.
fn engine_for(model_id: &str) -> Arc<dyn TtsEngine> {
    let creds = format!(r#"{{"modelId":"{model_id}"}}"#);
    let engine = create_engine("sherpaonnx", &creds).expect("sherpaonnx engine");
    // Verify the model is actually present on disk so a missing download
    // surfaces as a clear panic rather than a generic synth failure.
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    let model_dir = std::path::PathBuf::from(home)
        .join(".rust-tts-wrapper")
        .join("sherpaonnx")
        .join(model_id);
    assert!(
        model_dir.exists(),
        "Model '{model_id}' is not downloaded at {}. \
         Pre-download with scripts/download-sherpa-models.sh or pick a model id \
         that's already present.",
        model_dir.display()
    );
    engine
}

/// Capture every audio chunk delivered via the on_audio callback so we can
/// assert on total byte count and chunk sizing.
struct AudioSink {
    chunks: Vec<usize>,
    total_bytes: usize,
}

impl AudioSink {
    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            total_bytes: 0,
        }
    }
}

/// Same idea, but for word boundaries.
struct BoundarySink {
    words: Vec<(String, f32, f32)>,
}

impl BoundarySink {
    fn new() -> Self {
        Self { words: Vec::new() }
    }
}

// ===== VITS family — Piper =====
//
// Piper models are the smallest in the registry and exercise the
// `build_vits_config(... is_piper=true ...)` branch. They speak cleanly
// with no lexicon.txt, relying on espeak-ng-data.

#[test]
#[ignore]
fn vits_piper_synthesises_nonempty_audio() {
    let id = model_id("SHERPA_VITS_MODEL", "piper-nl-rdh-low");
    let engine = engine_for(&id);
    let sink = Arc::new(Mutex::new(AudioSink::new()));
    let sink_for_cb = sink.clone();
    let mut cb = move |chunk: &[u8]| {
        let mut sink = sink_for_cb.lock().unwrap();
        sink.chunks.push(chunk.len());
        sink.total_bytes += chunk.len();
    };
    engine
        .speak(
            "Hello, this is a test of the Piper VITS model.",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut cb),
            None,
        )
        .expect("speak");

    let sink = sink.lock().unwrap();
    assert!(sink.total_bytes > 0, "expected non-empty audio");
    assert!(!sink.chunks.is_empty(), "expected at least one chunk");
}

#[test]
#[ignore]
fn vits_piper_rate_changes_audio_size() {
    // Faster speech → fewer samples; slower speech → more samples.
    // SherpaOnnx applies rate via GenerationConfig.speed.
    let id = model_id("SHERPA_VITS_MODEL", "piper-nl-rdh-low");
    let engine = engine_for(&id);

    let text = "The quick brown fox jumps over the lazy dog.";

    let fast_bytes = Arc::new(Mutex::new(0usize));
    let fb = fast_bytes.clone();
    let mut cb_fast = |c: &[u8]| {
        *fb.lock().unwrap() += c.len();
    };
    engine
        .speak(text, None, 2.0, 1.0, 1.0, Some(&mut cb_fast), None)
        .unwrap();

    let slow_bytes = Arc::new(Mutex::new(0usize));
    let sb = slow_bytes.clone();
    let mut cb_slow = |c: &[u8]| {
        *sb.lock().unwrap() += c.len();
    };
    engine
        .speak(text, None, 0.5, 1.0, 1.0, Some(&mut cb_slow), None)
        .unwrap();

    let fast = *fast_bytes.lock().unwrap();
    let slow = *slow_bytes.lock().unwrap();
    assert!(
        fast < slow,
        "rate=2.0 ({fast} bytes) must produce fewer bytes than rate=0.5 ({slow} bytes)"
    );
}

#[test]
#[ignore]
fn vits_piper_volume_changes_amplitude() {
    // Volume is applied by apply_volume_and_pitch — verify by checking that
    // the peak sample amplitude scales. We compute peak on the f32 samples
    // reconstructed from the delivered PCM16 bytes.
    let id = model_id("SHERPA_VITS_MODEL", "piper-nl-rdh-low");
    let engine = engine_for(&id);

    fn synth_and_peak(engine: &Arc<dyn TtsEngine>, volume: f32) -> f32 {
        let peak = Arc::new(Mutex::new(0.0f32));
        let peak_clone = peak.clone();
        let mut cb = move |chunk: &[u8]| {
            for pair in chunk.chunks_exact(2) {
                let s = i16::from_le_bytes([pair[0], pair[1]]);
                // PCM i16 normalisation uses 32768 (i16::MAX + 1) so the
                // full negative range [-32768, 0] maps cleanly to [-1.0, 0].
                // Using 32767 leaves -32768 at -1.00003, slightly out of
                // range, which would skew the peak comparison below.
                #[allow(clippy::cast_precision_loss)]
                let abs = (s as f32 / 32768.0).abs();
                let mut p = peak_clone.lock().unwrap();
                if abs > *p {
                    *p = abs;
                }
            }
        };
        engine
            .speak(
                "Loudness check.",
                None,
                1.0,
                1.0,
                volume,
                Some(&mut cb),
                None,
            )
            .unwrap();
        let peak_value = *peak.lock().unwrap();
        peak_value
    }

    let quiet = synth_and_peak(&engine, 0.25);
    let loud = synth_and_peak(&engine, 1.0);
    assert!(
        loud > quiet,
        "volume=1.0 peak ({loud}) must exceed volume=0.25 peak ({quiet})"
    );
}

#[test]
#[ignore]
fn sherpa_streams_audio_per_sentence_batch() {
    // The generate progress callback delivers each sentence batch as it is
    // synthesised (max_num_sentences=1). With on_audio + on_boundary set,
    // boundary events must interleave with audio chunks — i.e. at least one
    // boundary fires BEFORE the final audio chunk, proving delivery is
    // incremental rather than one end-batched buffer. Additionally, the
    // first audio chunk must arrive before synthesis completes.
    let id = model_id("SHERPA_VITS_MODEL", "piper-nl-rdh-low");
    let engine = engine_for(&id);
    let text = "First sentence arrives early. Second sentence synthesises later. Third one closes the stream.";

    #[derive(PartialEq, Debug, Clone, Copy)]
    enum Ev {
        Audio,
        Boundary,
    }
    use Ev::{Audio as A, Boundary as B};

    let seq = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Ev>::new()));
    let t_start = std::time::Instant::now();
    let first_audio_at = std::sync::Arc::new(std::sync::Mutex::new(None::<std::time::Duration>));
    let last_audio_at = std::sync::Arc::new(std::sync::Mutex::new(t_start.elapsed()));

    let seq_a = seq.clone();
    let first_a = first_audio_at.clone();
    let last_a = last_audio_at.clone();
    let seq_b = seq.clone();
    engine
        .speak(
            text,
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut move |_chunk: &[u8]| {
                let mut f = first_a.lock().unwrap();
                if f.is_none() {
                    *f = Some(t_start.elapsed());
                }
                *last_a.lock().unwrap() = t_start.elapsed();
                seq_a.lock().unwrap().push(A);
            }),
            Some(&mut move |_w, _s, _e, _o, _l| seq_b.lock().unwrap().push(B)),
        )
        .expect("speak");

    let seq = seq.lock().unwrap().clone();
    let total = t_start.elapsed();
    assert!(seq.contains(&A), "no audio delivered");
    assert!(seq.contains(&B), "no boundaries delivered");
    let interleaved = seq
        .iter()
        .enumerate()
        .any(|(i, e)| *e == B && seq[i + 1..].contains(&A));
    assert!(
        interleaved,
        "no boundary fired before the final audio chunk; seq = {seq:?}"
    );
    let first = first_audio_at.lock().unwrap().expect("audio delivered");
    let last = *last_audio_at.lock().unwrap();
    assert!(
        first < last,
        "all audio arrived in one batch (first {first:?} == last {last:?})"
    );
    let _ = total;
}

#[test]
#[ignore]
fn vits_piper_word_boundaries_fire_per_word() {
    let id = model_id("SHERPA_VITS_MODEL", "piper-nl-rdh-low");
    let engine = engine_for(&id);
    let sink = Arc::new(Mutex::new(BoundarySink::new()));
    let sink_cb = sink.clone();
    let mut bound_cb = move |word: &str, start: f32, end: f32, _: i32, _: i32| {
        sink_cb
            .lock()
            .unwrap()
            .words
            .push((word.to_string(), start, end));
    };
    let text = "one two three four five";
    engine
        .speak(text, None, 1.0, 1.0, 1.0, None, Some(&mut bound_cb))
        .expect("speak with boundaries");

    let words = sink.lock().unwrap().words.clone();
    let expected: Vec<&str> = text.split_whitespace().collect();
    assert_eq!(
        words.len(),
        expected.len(),
        "expected one boundary per word"
    );
    // Offsets must be monotonic non-decreasing.
    for w in words.windows(2) {
        assert!(
            w[0].1 <= w[1].1,
            "boundary offsets must be monotonic: {:?}",
            w
        );
    }
}

#[test]
#[ignore]
fn vits_piper_streaming_vs_buffered_match() {
    // synth_to_bytes() and speak() with on_audio must deliver the same
    // audio within VITS's stochastic variation: two inferences of the
    // same text legitimately differ by a few percent of samples (the
    // generator noise makes exact byte equality unattainable), so assert
    // a tolerance instead of equality.
    let id = model_id("SHERPA_VITS_MODEL", "piper-nl-rdh-low");
    let engine = engine_for(&id);
    let text = "Same input both ways.";

    let buffered = engine
        .synth_to_bytes(text, None, 1.0, 1.0, 1.0)
        .expect("synth_to_bytes");

    let streamed = Arc::new(Mutex::new(0usize));
    let s = streamed.clone();
    let mut cb = move |c: &[u8]| {
        *s.lock().unwrap() += c.len();
    };
    engine
        .speak(text, None, 1.0, 1.0, 1.0, Some(&mut cb), None)
        .expect("speak");

    let streamed = *streamed.lock().unwrap();
    assert!(buffered.len() > 0, "buffered synthesis produced no audio");
    assert!(streamed > 0, "streamed synthesis produced no audio");
    let diff = streamed.abs_diff(buffered.len());
    let tolerance = buffered.len() / 5; // 20%
    assert!(
        diff <= tolerance,
        "streamed ({streamed}) and buffered ({}) byte counts differ by {diff}, \
         more than the 20% VITS nondeterminism tolerance ({tolerance})",
        buffered.len()
    );
}

#[test]
#[ignore]
fn vits_piper_multi_speaker_voice_id_selectable() {
    // Multi-speaker VITS models accept a numeric speaker id via the `voice`
    // parameter. Synthesise twice with two different ids and confirm both
    // produce audio (we can't easily assert they differ without a model
    // diff, but a crash or empty result would surface here).
    let id = model_id("SHERPA_VITS_MULTI_MODEL", "coqui-en-vctk");
    // Skip cleanly if the multi-speaker model isn't downloaded.
    skip_if_missing!(id);
    let engine = engine_for(&id);

    for speaker in ["0", "10"] {
        let bytes = Arc::new(Mutex::new(0usize));
        let b = bytes.clone();
        let mut cb = move |c: &[u8]| {
            *b.lock().unwrap() += c.len();
        };
        engine
            .speak(
                "Speaker check.",
                Some(speaker),
                1.0,
                1.0,
                1.0,
                Some(&mut cb),
                None,
            )
            .expect("speak with speaker id");
        assert!(
            *bytes.lock().unwrap() > 0,
            "speaker {speaker} produced no audio"
        );
    }
}

// ===== Matcha family =====

#[test]
#[ignore]
fn matcha_synthesises_nonempty_audio() {
    let id = model_id("SHERPA_MATCHA_MODEL", "icefall-fs-ljspeech");
    let engine = engine_for(&id);
    let total = Arc::new(Mutex::new(0usize));
    let t = total.clone();
    let mut cb = move |c: &[u8]| {
        *t.lock().unwrap() += c.len();
    };
    engine
        .speak(
            "Matcha model synthesis check.",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut cb),
            None,
        )
        .expect("matcha speak");
    assert!(*total.lock().unwrap() > 0, "matcha produced no audio");
}

#[test]
#[ignore]
fn matcha_word_boundaries_fire() {
    let id = model_id("SHERPA_MATCHA_MODEL", "icefall-fs-ljspeech");
    let engine = engine_for(&id);
    let sink = Arc::new(Mutex::new(BoundarySink::new()));
    let s = sink.clone();
    let mut cb = move |w: &str, st: f32, e: f32, _: i32, _: i32| {
        s.lock().unwrap().words.push((w.into(), st, e));
    };
    engine
        .speak("one two three", None, 1.0, 1.0, 1.0, None, Some(&mut cb))
        .expect("matcha speak");
    assert_eq!(sink.lock().unwrap().words.len(), 3);
}

// ===== Kokoro family =====

#[test]
#[ignore]
fn kokoro_synthesises_nonempty_audio() {
    let id = model_id("SHERPA_KOKORO_MODEL", "kokoro-zh_en-int8");
    skip_if_missing!(id);
    let engine = engine_for(&id);
    let total = Arc::new(Mutex::new(0usize));
    let t = total.clone();
    let mut cb = move |c: &[u8]| {
        *t.lock().unwrap() += c.len();
    };
    engine
        .speak(
            "Kokoro synthesis check.",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut cb),
            None,
        )
        .expect("kokoro speak");
    assert!(*total.lock().unwrap() > 0, "kokoro produced no audio");
}

#[test]
#[ignore]
fn kokoro_voices_bin_loaded_from_registry() {
    // Kokoro ships voices.bin — get_voices() should still return the
    // registry-based speaker list (the engine doesn't introspect voices.bin
    // directly). Smoke-test that voice enumeration doesn't panic.
    let id = model_id("SHERPA_KOKORO_MODEL", "kokoro-zh_en-int8");
    skip_if_missing!(id);
    let engine = engine_for(&id);
    let voices = engine.get_voices().expect("voices");
    assert!(!voices.is_empty());
    assert!(voices.iter().all(|v| v.provider == "sherpaonnx"));
}

// ===== Supertonic family =====
//
// Supertonic 3 is the multilingual flow-matching model (31 languages, 10
// preset speakers). Voices are addressed by "sid:lang" (e.g. "0:ja") and the
// language is routed through GenerationConfig.extra. These tests verify the
// full wiring: registry lookup → config build → lang passing → audio.

#[test]
#[ignore]
fn supertonic_synthesises_nonempty_audio() {
    let id = model_id("SHERPA_SUPERTONIC_MODEL", "supertonic-3-multilingual");
    skip_if_missing!(id);
    let engine = engine_for(&id);
    let total = Arc::new(Mutex::new(0usize));
    let t = total.clone();
    let mut cb = move |c: &[u8]| {
        *t.lock().unwrap() += c.len();
    };
    engine
        .speak(
            "Supertonic synthesis check.",
            Some("0:en"),
            1.0,
            1.0,
            1.0,
            Some(&mut cb),
            None,
        )
        .expect("supertonic speak");
    assert!(*total.lock().unwrap() > 0, "supertonic produced no audio");
}

#[test]
#[ignore]
fn supertonic_voices_cover_speakers_times_languages() {
    // 10 preset speakers × 31 languages = 310 voices, each id "sid:lang".
    let id = model_id("SHERPA_SUPERTONIC_MODEL", "supertonic-3-multilingual");
    skip_if_missing!(id);
    let engine = engine_for(&id);
    let voices = engine.get_voices().expect("voices");
    assert_eq!(voices.len(), 310);
    assert!(voices.iter().any(|v| v.id == "0:en"));
    assert!(voices.iter().any(|v| v.id == "6:ja"));
}

#[test]
#[ignore]
fn supertonic_switches_language_via_voice_id() {
    // The "sid:lang" encoding must actually reach the model — synth in two
    // languages and confirm neither errors and both emit audio.
    let id = model_id("SHERPA_SUPERTONIC_MODEL", "supertonic-3-multilingual");
    skip_if_missing!(id);
    let engine = engine_for(&id);
    for (voice, text) in [
        ("0:en", "Switching languages."),
        ("0:ja", "言語を切り替えます。"),
    ] {
        let total = Arc::new(Mutex::new(0usize));
        let t = total.clone();
        let mut cb = move |c: &[u8]| {
            *t.lock().unwrap() += c.len();
        };
        engine
            .speak(text, Some(voice), 1.0, 1.0, 1.0, Some(&mut cb), None)
            .unwrap_or_else(|e| panic!("speak {voice} failed: {e}"));
        assert!(
            *total.lock().unwrap() > 0,
            "voice {voice} produced no audio"
        );
    }
}

// ===== Cross-model: speechmarkdown preprocessing feeds through =====

#[test]
#[ignore]
fn speechmarkdown_input_does_not_break_synthesis() {
    // speechmarkdown-rust isn't wired into sherpaonnx (it has no SSML
    // surface), so SpeechMarkdown input must be passed through untouched
    // and synthesis must complete.
    let id = model_id("SHERPA_VITS_MODEL", "piper-nl-rdh-low");
    let engine = engine_for(&id);
    let total = Arc::new(Mutex::new(0usize));
    let t = total.clone();
    let mut cb = move |c: &[u8]| {
        *t.lock().unwrap() += c.len();
    };
    engine
        .speak(
            "Hello (world)[emphasis:\"strong\"]",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut cb),
            None,
        )
        .expect("speak with speechmarkdown input");
    assert!(*total.lock().unwrap() > 0);
}

// ===== Cross-model: stop() is safe even when no synthesis is in flight =====

#[test]
#[ignore]
fn stop_without_active_synthesis_is_safe() {
    let id = model_id("SHERPA_VITS_MODEL", "piper-nl-rdh-low");
    let engine = engine_for(&id);
    engine.stop().expect("stop should not error");
}

// ===== Cross-model: pitch shift changes sample count =====

#[test]
#[ignore]
fn pitch_shift_changes_sample_count() {
    // apply_volume_and_pitch uses linear-interpolation resampling for pitch,
    // so pitch != 1.0 must change the output sample count.
    let id = model_id("SHERPA_VITS_MODEL", "piper-nl-rdh-low");
    let engine = engine_for(&id);
    let text = "Pitch shift measurement.";

    let normal = {
        let n = Arc::new(Mutex::new(0usize));
        let nc = n.clone();
        let mut cb = move |c: &[u8]| {
            *nc.lock().unwrap() += c.len();
        };
        engine
            .speak(text, None, 1.0, 1.0, 1.0, Some(&mut cb), None)
            .unwrap();
        let v = *n.lock().unwrap();
        v
    };

    let shifted = {
        let s = Arc::new(Mutex::new(0usize));
        let sc = s.clone();
        let mut cb = move |c: &[u8]| {
            *sc.lock().unwrap() += c.len();
        };
        engine
            .speak(text, None, 1.0, 2.0, 1.0, Some(&mut cb), None)
            .unwrap();
        let v = *s.lock().unwrap();
        v
    };

    assert_ne!(
        normal, shifted,
        "pitch=2.0 ({shifted} bytes) must differ from pitch=1.0 ({normal} bytes)"
    );
}
