#![cfg(feature = "floravox")]

//! Live floravox conformance tests (handoff spec §4).
//!
//! These run real ONNX inference and need a voice on disk. They are
//! `#[ignore]`-d by default; run locally with:
//!
//!   `FLORAVOX_TEST_VOICE=/path/to/voice-dir` \
//!     `cargo test --test floravox_live --features floravox -- --ignored`
//!
//! `FLORAVOX_TEST_VOICE` points at a voice directory or `.onnx` file
//! (piper-family preferred). Optional: `FLORAVOX_TEST_PATCHED=1` asserts
//! the stricter measured-timing contract on that voice (duration-patched
//! voices only — the patch is floravox's `add_durations_output.py`).
//! Without it the unpatched contract (`estimated: true`) is asserted.

use rust_tts_wrapper::engine::TtsEngine;
use rust_tts_wrapper::floravox_engine::FloravoxEngine;

fn test_voice() -> Option<String> {
    std::env::var("FLORAVOX_TEST_VOICE")
        .ok()
        .filter(|v| !v.is_empty())
}

fn engine_for(voice: &str) -> FloravoxEngine {
    // Windows paths carry backslashes — they must be escaped to survive
    // the credentials JSON round-trip.
    let escaped = voice.replace('\\', "\\\\");
    let creds = format!(r#"{{"modelId": "{escaped}"}}"#);
    FloravoxEngine::new(&creds)
}

#[test]
#[ignore = "needs FLORAVOX_TEST_VOICE (real ONNX voice on disk)"]
fn ssml_break_produces_boundaries() {
    let Some(voice) = test_voice() else {
        eprintln!("FLORAVOX_TEST_VOICE not set — skipping");
        return;
    };
    let engine = engine_for(&voice);
    let audio = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let audio2 = std::sync::Arc::clone(&audio);
    let boundaries = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let b2 = std::sync::Arc::clone(&boundaries);

    engine
        .speak(
            "<speak>Hello <break time=\"500ms\"/> world</speak>",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut |chunk: &[u8]| {
                audio2.fetch_add(chunk.len(), std::sync::atomic::Ordering::SeqCst);
            }),
            Some(
                &mut |word: &str, start: f32, end: f32, offset: i32, len: i32, est: bool| {
                    b2.lock()
                        .unwrap()
                        .push((word.to_string(), start, end, offset, len, est));
                },
            ),
            None,
        )
        .expect("SSML synthesis");

    assert!(
        audio.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "audio delivered"
    );
    let b = boundaries.lock().unwrap();
    assert!(!b.is_empty(), "boundaries reported");
    // With a break after the first word, "world" must start later than
    // the first word ended by at least most of the 500 ms pause.
    if b.len() >= 2 {
        let gap = b[1].1 - b[0].2;
        assert!(gap > 0.3, "break honoured between words (gap {gap:.3}s)");
    }
    if std::env::var("FLORAVOX_TEST_PATCHED").is_ok_and(|v| v == "1") {
        // Measured contract: duration-patched voices never estimate.
        assert!(
            b.iter().all(|e| !e.5),
            "patched voice reports measured timings: {b:?}"
        );
    } else {
        // Unpatched contract: proportional estimates flagged.
        assert!(
            b.iter().any(|e| e.5),
            "unpatched voice flags estimates: {b:?}"
        );
    }
}

#[test]
#[ignore = "needs FLORAVOX_TEST_VOICE (real ONNX voice on disk)"]
fn speechmarkdown_matches_ssml_expansion() {
    // Round-trip check: the engine expands SpeechMarkdown to the generic
    // SSML dialect itself, so identical expansions must produce identical
    // audio (ONNX inference is deterministic for identical input).
    let Some(voice) = test_voice() else {
        eprintln!("FLORAVOX_TEST_VOICE not set — skipping");
        return;
    };
    let engine = engine_for(&voice);
    let smd_audio = engine
        .synth_with_boundaries("Hello [500ms] world", None, 1.0, 1.0, 1.0)
        .expect("SMD synthesis");
    let ssml_audio = engine
        .synth_with_boundaries(
            "<speak>Hello <break time=\"500ms\"/> world</speak>",
            None,
            1.0,
            1.0,
            1.0,
        )
        .expect("SSML synthesis");
    assert_eq!(
        smd_audio.0.len(),
        ssml_audio.0.len(),
        "SMD input and its SSML expansion synthesize identically"
    );
    assert_eq!(smd_audio.0, ssml_audio.0, "sample-exact audio match");
}

#[test]
#[ignore = "needs FLORAVOX_TEST_VOICE (real ONNX voice on disk)"]
fn warm_up_loads_session_then_speak_is_fast() {
    let Some(voice) = test_voice() else {
        eprintln!("FLORAVOX_TEST_VOICE not set — skipping");
        return;
    };
    let engine = engine_for(&voice);
    engine
        .warm_up(None)
        .expect("warm_up loads the ONNX session");
    let start = std::time::Instant::now();
    engine
        .speak("Warm.", None, 1.0, 1.0, 1.0, None, None, None)
        .expect("speak after warm_up");
    // First-speak after warm-up skips the model load; generous bound to
    // stay robust on slow CI boxes while still catching a re-load.
    assert!(
        start.elapsed().as_secs() < 30,
        "post-warm-up speak took {:?} — session not cached?",
        start.elapsed()
    );
}

#[test]
#[ignore = "needs FLORAVOX_TEST_VOICE + a sibling .student file"]
fn student_sidecar_timings_are_sane_and_ordered() {
    // The student tier engages automatically when a `.student` file sits
    // beside the voice. What is asserted here: synthesis delivers audio
    // and ordered boundaries; the estimates flag follows floravox's own
    // tier semantics (a student refinement is still an estimate tier,
    // not a measurement). Distinguishing student vs proportional output
    // requires a second voice without the sidecar — not covered here.
    let Some(voice) = test_voice() else {
        eprintln!("FLORAVOX_TEST_VOICE not set — skipping");
        return;
    };
    let engine = engine_for(&voice);
    let (audio_student, boundaries_student) = engine
        .synth_with_boundaries("The quick brown fox jumps", None, 1.0, 1.0, 1.0)
        .expect("synthesis with sidecar present");
    assert!(!audio_student.is_empty());
    assert!(
        !boundaries_student.is_empty(),
        "student tier reports boundaries: {boundaries_student:?}"
    );
    // Word offsets must be non-decreasing (timings ordered).
    let offsets: Vec<u64> = boundaries_student.iter().map(|b| b.offset).collect();
    assert!(
        offsets.windows(2).all(|w| w[0] <= w[1]),
        "offsets must be non-decreasing: {offsets:?}"
    );
}
