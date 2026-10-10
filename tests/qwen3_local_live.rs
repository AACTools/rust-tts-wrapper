#![cfg(feature = "qwen3-local")]

//! Live local Qwen3-TTS tests — no network, but need the built C++
//! library and converted GGUF models. `#[ignore]`-d by default so
//! feature builds without models still test-compile. Run with:
//!
//! ```text
//! QWEN3_TTS_LIB=/path/to/qwen3-tts.cpp \
//! QWEN3_TTS_MODELS=/path/to/qwen3-tts.cpp/models \
//! QWEN3_TTS_REF=/path/to/reference.wav \
//!     cargo test --features qwen3-local,cloning --test qwen3_local_live -- --ignored --nocapture
//! ```

use rust_tts_wrapper::cloning::{create_cloner, AudioClip, CloneOutcome, VoiceIdentityBuilder};
use rust_tts_wrapper::factory::create_engine;
use std::sync::{Arc, Mutex};

fn creds() -> Option<String> {
    let models = std::env::var("QWEN3_TTS_MODELS").ok()?;
    Some(
        serde_json::json!({
            "modelsDir": models,
            "threads": std::env::var("QWEN3_TTS_THREADS").unwrap_or_else(|_| "6".into()),
        })
        .to_string(),
    )
}

#[test]
#[ignore = "needs QWEN3_TTS_LIB-linked build + QWEN3_TTS_MODELS (GGUFs on disk)"]
fn qwen3_local_synthesizes_and_estimates_boundaries() {
    let Some(creds) = creds() else {
        eprintln!("QWEN3_TTS_MODELS not set; skipping");
        return;
    };
    let engine = create_engine("qwen3-local", &creds).expect("engine");
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let words: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (a, w) = (Arc::clone(&audio), Arc::clone(&words));
    engine
        .speak(
            "Hello from the local Qwen three engine.",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut move |b: &[u8]| a.lock().unwrap().extend_from_slice(b)),
            Some(&mut move |word: &str, _s, _e, _o, _l, est| {
                w.lock()
                    .unwrap()
                    .push(format!("{word}{}", if est { "*" } else { "" }));
            }),
            None,
        )
        .expect("speak");
    let pcm = audio.lock().unwrap().clone();
    assert!(pcm.len() > 48_000, "expected >1 s of audio");
    assert_eq!(pcm.len() % 2, 0);
    eprintln!(
        "{} PCM bytes; boundaries: {:?}",
        pcm.len(),
        words.lock().unwrap()
    );
}

#[test]
#[ignore = "needs QWEN3_TTS_LIB + QWEN3_TTS_MODELS + QWEN3_TTS_REF (reference wav)"]
fn qwen3_local_zero_shot_clone_roundtrip() {
    let Some(creds) = creds() else {
        eprintln!("QWEN3_TTS_MODELS not set; skipping");
        return;
    };
    let reference = match std::env::var("QWEN3_TTS_REF") {
        Ok(r) if !r.is_empty() => r,
        _ => {
            eprintln!("QWEN3_TTS_REF not set; skipping");
            return;
        }
    };
    // Cloner: identity → embedding handle.
    let cloner = create_cloner("qwen3-local", &creds).expect("cloner");
    let identity = VoiceIdentityBuilder::new("banked")
        .add_clip(AudioClip::from_audio_file(&reference).expect("reference clip"))
        .finish()
        .expect("identity");
    let handle = match cloner.clone_voice(&identity).expect("clone") {
        CloneOutcome::Ready(h) => h,
        other @ CloneOutcome::Pending { .. } => panic!("expected Ready, got {other:?}"),
    };
    assert!(
        handle.voice_id.starts_with("emb:"),
        "{}",
        &handle.voice_id[..8.min(handle.voice_id.len())]
    );

    // Engine speaks with the embedding handle.
    let engine = create_engine("qwen3-local", &creds).expect("engine");
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&audio);
    engine
        .speak(
            "This is my cloned voice, running fully offline.",
            Some(&handle.voice_id),
            1.0,
            1.0,
            1.0,
            Some(&mut move |b: &[u8]| sink.lock().unwrap().extend_from_slice(b)),
            None,
            None,
        )
        .expect("speak with embedding");
    let pcm = audio.lock().unwrap().len();
    assert!(pcm > 48_000, "expected >1 s of cloned audio, got {pcm}");
}

#[test]
#[ignore = "needs QWEN3_TTS_LIB + QWEN3_TTS_MODELS"]
fn qwen3_local_language_voice_ssml_and_volume() {
    let Some(creds) = creds() else {
        eprintln!("QWEN3_TTS_MODELS not set; skipping");
        return;
    };
    let engine = create_engine("qwen3-local", &creds).expect("engine");
    assert!(
        engine.check_credentials().unwrap(),
        "models should be loaded"
    );
    let voices = engine.get_voices().expect("voices");
    assert_eq!(voices.len(), 11, "default + 10 languages");
    assert!(voices
        .iter()
        .any(|v| v.id == "fr" && v.language_codes[0].bcp47 == "fr"));

    // Language voice + SSML-in (tags must be stripped, not spoken) +
    // volume gain path.
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&audio);
    engine
        .speak(
            "<speak>Bonjour, ceci est un test <break time=\"200ms\"/> en français.</speak>",
            Some("fr"),
            1.0,
            1.0,
            0.5,
            Some(&mut move |b: &[u8]| sink.lock().unwrap().extend_from_slice(b)),
            None,
            None,
        )
        .expect("speak fr");
    let pcm = audio.lock().unwrap().len();
    assert!(pcm > 16_000, "expected some audio, got {pcm}");
}
