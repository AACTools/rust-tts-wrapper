#![cfg(feature = "cloning")]

//! Live voice-cloning tests. `#[ignore]`-d by default (billed network
//! calls; Qwen voice-enrollment is free but real). Run locally with:
//!
//! ```text
//! QWEN_API_KEY=sk-... QWEN_PV_ZIP="/path/Will's Personal Voice 1 - Recordings.zip" \
//!     cargo test --features cloning --no-default-features --test cloning_live -- --ignored --nocapture
//! ```
//!
//! `QWEN_PV_ZIP` points at a real Apple Personal Voice "Recordings"
//! export. The test clones, lists, synthesizes through the qwen engine,
//! and DELETES the voice afterwards (quota hygiene).

use rust_tts_wrapper::cloning::{create_cloner, CloneOutcome, VoiceCorpus};
use rust_tts_wrapper::factory::create_engine;
use std::sync::{Arc, Mutex};

fn creds() -> Option<String> {
    let key = std::env::var("QWEN_API_KEY").ok()?;
    Some(serde_json::json!({ "apiKey": key }).to_string())
}

#[test]
#[ignore = "needs QWEN_API_KEY + QWEN_PV_ZIP (real export, real network)"]
fn qwen_clone_from_personal_voice_zip_roundtrip() {
    let Some(creds) = creds() else {
        eprintln!("QWEN_API_KEY not set; skipping");
        return;
    };
    let zip = match std::env::var("QWEN_PV_ZIP") {
        Ok(z) if !z.is_empty() => z,
        _ => {
            eprintln!("QWEN_PV_ZIP not set; skipping");
            return;
        }
    };

    // Import — the real export decodes with at most a couple of bad frames.
    let corpus = VoiceCorpus::from_personal_voice_zip(&zip).expect("import zip");
    assert!(corpus.clips.len() > 100, "expected ~150 clips");
    assert!(
        corpus.total_duration_secs() > 600,
        "expected ~12 min of audio"
    );
    assert!(
        corpus.name.contains("Personal Voice") || !corpus.name.is_empty(),
        "corpus name from zip stem: {}",
        corpus.name
    );
    // Canonical form: 24 kHz mono s16 → 48000 B/s.
    for clip in &corpus.clips {
        assert_eq!(clip.sample_rate, 24_000);
        assert_eq!(clip.pcm.len() % 2, 0);
    }

    let cloner = create_cloner("qwen", &creds).expect("qwen cloner");
    let identity = corpus.to_identity(Some("en"));

    let handle = match cloner.clone_voice(&identity).expect("clone") {
        CloneOutcome::Ready(h) => h,
        other @ CloneOutcome::Pending { .. } => panic!("expected Ready, got {other:?}"),
    };
    assert!(
        handle.voice_id.starts_with("qwen-audio-3.0-tts-flash-"),
        "{}",
        handle.voice_id
    );
    assert_eq!(handle.model.as_deref(), Some("qwen-audio-3.0-tts-flash"));

    // List sees it.
    let listed = cloner.list_cloned().expect("list");
    assert!(listed.iter().any(|h| h.voice_id == handle.voice_id));

    // Synthesize through the engine with the model-bound credential.
    let mut v: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&creds).unwrap();
    v.insert("modelId".into(), handle.model.clone().unwrap().into());
    let engine_creds = serde_json::Value::Object(v).to_string();
    let engine = create_engine("qwen", &engine_creds).expect("engine");
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&audio);
    engine
        .speak(
            "This is my cloned voice speaking through the engine.",
            Some(&handle.voice_id),
            1.0,
            1.0,
            1.0,
            Some(&mut move |b: &[u8]| sink.lock().unwrap().extend_from_slice(b)),
            None,
            None,
        )
        .expect("speak with cloned voice");
    let len = audio.lock().unwrap().len();
    assert!(len > 48_000, "expected >1 s of audio, got {len} bytes");

    // Clean up: delete and confirm it leaves the list.
    cloner.delete_cloned(&handle).expect("delete");
    let listed = cloner.list_cloned().expect("list after delete");
    assert!(!listed.iter().any(|h| h.voice_id == handle.voice_id));
}
