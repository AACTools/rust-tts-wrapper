#![cfg(all(feature = "omnivoice-local", feature = "word-align"))]
//! Live word-align integration tests (#109): the full chain —
//! `omnivoice-local` engine wrapped by the aligner middleware (via the
//! factory's `wordAligner` credential), synthesizing REAL audio and
//! firing REAL (`estimated=false`) word boundaries measured from it.
//!
//! `#[ignore]`-d by default. Run with:
//!
//! ```text
//! OMNIVOICE_LIB=/path/to/omnivoice.cpp \
//! OMNIVOICE_MODEL=/path/omnivoice-base-Q8_0.gguf \
//! OMNIVOICE_CODEC=/path/omnivoice-tokenizer-F32.gguf \
//! WORD_ALIGNER_MODEL=/path/mms-aligner.onnx \
//!     cargo test --features omnivoice-local,word-align \
//!         --test word_align_live -- --ignored --nocapture
//! ```

use rust_tts_wrapper::factory::create_engine;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct Boundary {
    word: String,
    start: f32,
    end: f32,
    estimated: bool,
}

#[test]
#[ignore = "needs OMNIVOICE_LIB build + models + WORD_ALIGNER_MODEL (real synthesis)"]
#[allow(clippy::too_many_lines)]
fn omnivoice_boundaries_become_real_via_aligner() {
    type SharedVec<T> = Arc<Mutex<Vec<T>>>;
    let Ok(model) = std::env::var("OMNIVOICE_MODEL") else {
        eprintln!("OMNIVOICE_MODEL not set; skipping");
        return;
    };
    let Ok(aligner) = std::env::var("WORD_ALIGNER_MODEL") else {
        eprintln!("WORD_ALIGNER_MODEL not set; skipping");
        return;
    };
    let codec = std::env::var("OMNIVOICE_CODEC").expect("OMNIVOICE_CODEC");
    let steps = std::env::var("OMNIVOICE_STEPS").unwrap_or_else(|_| "16".into());
    let creds = serde_json::json!({
        "modelPath": model,
        "codecPath": codec,
        "numSteps": steps,
        "wordAligner": aligner,
    })
    .to_string();

    let engine = create_engine("omnivoice-local", &creds).expect("engine (aligner-wrapped)");
    let audio: SharedVec<u8> = Arc::new(Mutex::new(Vec::new()));
    let boundaries: SharedVec<Boundary> = Arc::new(Mutex::new(Vec::new()));
    let a = Arc::clone(&audio);
    let b = Arc::clone(&boundaries);
    let text = "The quick brown fox jumps over the lazy dog.";
    let t0 = std::time::Instant::now();
    engine
        .speak(
            text,
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut move |pcm: &[u8]| {
                a.lock().map(|mut v| v.extend_from_slice(pcm)).ok();
            }),
            Some(
                &mut move |word: &str, start: f32, end: f32, _o: i32, _l: i32, est: bool| {
                    b.lock()
                        .map(|mut v| {
                            v.push(Boundary {
                                word: word.to_string(),
                                start,
                                end,
                                estimated: est,
                            });
                        })
                        .ok();
                },
            ),
            None,
        )
        .expect("speak");
    let elapsed = t0.elapsed().as_secs_f32();

    let bounds = boundaries.lock().unwrap().clone();
    let audio_bytes = audio.lock().map_or(0, |v| v.len());
    // PCM16 mono 24 kHz — counts are far below f32's mantissa.
    #[allow(clippy::cast_precision_loss)]
    let total_secs = audio_bytes as f32 / 48_000.0;
    eprintln!(
        "synthesized {total_secs:.2}s in {elapsed:.1}s; {} boundaries",
        bounds.len()
    );
    for bnd in &bounds {
        eprintln!(
            "  '{}': {:.3}..{:.3} (estimated={})",
            bnd.word, bnd.start, bnd.end, bnd.estimated
        );
    }

    // The middleware replaced the estimates: every boundary is REAL.
    assert!(!bounds.is_empty(), "expected boundaries");
    assert!(
        bounds.iter().all(|b| !b.estimated),
        "all boundaries must be estimated=false, got {bounds:?}"
    );
    // Words are in the text and timed within the audio.
    for bnd in &bounds {
        assert!(
            text.to_lowercase().contains(&bnd.word),
            "word '{}' not from the spoken text",
            bnd.word
        );
        assert!(
            (0.0..=total_secs).contains(&bnd.start),
            "start {} outside audio ({}s)",
            bnd.start,
            total_secs
        );
        assert!(
            bnd.end > bnd.start && bnd.end <= total_secs + 0.05,
            "end {} invalid",
            bnd.end
        );
    }
    // Monotonic word order.
    for w in bounds.windows(2) {
        assert!(
            w[0].start <= w[1].start,
            "non-monotonic: {} then {}",
            w[0].word,
            w[1].word
        );
    }
    // Sanity: the dog sentence has 9 words — at least most align.
    assert!(
        bounds.len() >= 7,
        "expected ~9 word boundaries, got {}",
        bounds.len()
    );
}
