#![cfg(feature = "omnivoice-local")]
//! Live OmniVoice-local tests — no network, but need the built
//! omnivoice.cpp library (`OMNIVOICE_LIB` at build time) and the
//! GGUF model pair on disk. `#[ignore]`-d by default so feature
//! builds without models still test-compile. Run with:
//!
//! ```text
//! OMNIVOICE_LIB=/path/to/omnivoice.cpp \
//! OMNIVOICE_MODEL=/path/to/omnivoice-base-Q8_0.gguf \
//! OMNIVOICE_CODEC=/path/to/omnivoice-tokenizer-F32.gguf \
//! OMNIVOICE_REF_WAV=/path/to/reference.wav \
//!     cargo test --features omnivoice-local --test omnivoice_local_live \
//!         -- --ignored --nocapture
//! ```
//!
//! The reference WAV is optional: without it the plain-TTS tests
//! still run; with it the cloning (codes-path) tests run too.

use rust_tts_wrapper::factory::create_engine;
use rust_tts_wrapper::TtsEngine;
use std::sync::{Arc, Mutex};

fn creds() -> Option<String> {
    let model = std::env::var("OMNIVOICE_MODEL").ok()?;
    let codec = std::env::var("OMNIVOICE_CODEC").ok()?;
    let steps = std::env::var("OMNIVOICE_STEPS").unwrap_or_else(|_| "32".into());
    Some(
        serde_json::json!({
            "modelPath": model,
            "codecPath": codec,
            "numSteps": steps,
        })
        .to_string(),
    )
}

fn ref_pair() -> Option<(String, String)> {
    let wav = std::env::var("OMNIVOICE_REF_WAV").ok()?;
    let text = std::env::var("OMNIVOICE_REF_TEXT").ok()?;
    Some((wav, text))
}

/// Plain synthesis through the factory path: non-empty audio at the
/// runtime's sample rate (24 kHz).
#[test]
#[ignore = "needs OMNIVOICE_LIB-linked build + OMNIVOICE_MODEL/CODEC"]
fn omnivoice_local_synthesizes() {
    let Some(creds) = creds() else {
        eprintln!("OMNIVOICE_MODEL/CODEC not set; skipping");
        return;
    };
    let engine = create_engine("omnivoice-local", &creds).expect("engine");
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let a = Arc::clone(&audio);
    engine
        .speak(
            "Hello from the local OmniVoice engine.",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut move |pcm: &[u8]| {
                a.lock().map(|mut b| b.extend_from_slice(pcm)).ok();
            }),
            None,
            None,
        )
        .expect("synthesis");
    let samples = audio.lock().expect("audio lock");
    assert!(
        samples.len() > 4_800,
        "expected > 0.1 s of 16-bit 24 kHz audio, got {} bytes",
        samples.len()
    );
}

/// Issue #151 P2 — the codes path, end to end against the REAL
/// runtime: `set_reference` extracts codes once; repeated synthesis
/// with the same voice must keep working (the engine passes
/// `ref_audio_tokens`, never raw samples). Also prints the per-call
/// wall time so the codec-encode savings are visible in --nocapture
/// output.
#[test]
#[ignore = "needs OMNIVOICE_LIB-linked build + model/codec + OMNIVOICE_REF_WAV/REF_TEXT"]
fn omnivoice_local_cloning_codes_path_repeats() {
    if creds().is_none() {
        eprintln!("OMNIVOICE_MODEL/CODEC not set; skipping");
        return;
    }
    let Some((ref_wav, ref_text)) = ref_pair() else {
        eprintln!("OMNIVOICE_REF_WAV/REF_TEXT not set; skipping");
        return;
    };
    // set_reference needs &mut self, so build the concrete engine
    // directly rather than going through the factory's Arc<dyn _>.
    let params = rust_tts_wrapper::omnivoice_local_engine::OmniVoiceLocalParams {
        model_path: std::env::var("OMNIVOICE_MODEL").expect("OMNIVOICE_MODEL"),
        codec_path: std::env::var("OMNIVOICE_CODEC").expect("OMNIVOICE_CODEC"),
        ..Default::default()
    };
    let mut eng = rust_tts_wrapper::omnivoice_local_engine::OmniVoiceLocalEngine::new(params)
        .expect("engine");
    let t0 = std::time::Instant::now();
    eng.set_reference(&ref_wav, &ref_text)
        .expect("reference extraction (codes path)");
    eprintln!("set_reference (extract codes): {:.2?}", t0.elapsed());
    for i in 0..2 {
        let t = std::time::Instant::now();
        let out = eng.speak(
            "This voice was cloned from the reference clip.",
            None,
            1.0,
            1.0,
            1.0,
            None,
            None,
            None,
        );
        let dt = t.elapsed();
        assert!(out.is_ok(), "synthesis {i} via cached codes: {out:?}");
        eprintln!("synthesis {i} via cached codes: {dt:.2?}");
    }
}
