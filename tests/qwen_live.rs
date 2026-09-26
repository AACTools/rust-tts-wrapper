#![cfg(feature = "cloud")]

//! Live Qwen cloud TTS tests (Alibaba Cloud Model Studio / `DashScope`).
//!
//! These tests hit the real WebSocket synthesis endpoint and are
//! `#[ignore]`-d by default so CI (no key) skips them. Run locally with:
//!
//! ```text
//! QWEN_API_KEY=sk-... cargo test --test qwen_live -- --ignored
//! ```
//!
//! `QWEN_MODEL` (default `qwen-audio-3.0-tts-flash`), `QWEN_VOICE`
//! (default `longanhuan_v3.6`), and `QWEN_REGION` (default empty — the
//! Qwen Cloud endpoint; `intl`/`beijing` select the `DashScope` endpoints)
//! override the request.

use rust_tts_wrapper::engine::TtsEngine;
use rust_tts_wrapper::factory::create_engine;
use std::sync::{Arc, Mutex};

fn creds() -> Option<String> {
    let key = std::env::var("QWEN_API_KEY").ok()?;
    let mut map = serde_json::Map::new();
    map.insert("apiKey".into(), key.into());
    for (env_key, cred_key) in [
        ("QWEN_MODEL", "modelId"),
        ("QWEN_VOICE", "voiceId"),
        ("QWEN_REGION", "region"),
        ("QWEN_INSTRUCTION", "instruction"),
    ] {
        if let Ok(v) = std::env::var(env_key) {
            if !v.is_empty() {
                map.insert(cred_key.into(), v.into());
            }
        }
    }
    Some(serde_json::Value::Object(map).to_string())
}

fn make_engine() -> Option<Arc<dyn TtsEngine>> {
    create_engine("qwen", &creds()?)
}

#[test]
#[ignore = "needs QWEN_API_KEY (billed network call)"]
fn qwen_live_speaks_and_returns_voices() {
    let Some(engine) = make_engine() else {
        eprintln!("QWEN_API_KEY not set; skipping");
        return;
    };

    let voices = engine.get_voices().expect("voice list");
    assert!(!voices.is_empty(), "static voice table must not be empty");
    assert!(voices.iter().any(|v| v.id == "longanhuan_v3.6"));

    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let boundaries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let a = Arc::clone(&audio);
    let b = Arc::clone(&boundaries);
    engine
        .speak(
            "Hello, this is a live test of the Qwen engine.",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut move |bytes: &[u8]| {
                a.lock().expect("audio lock").extend_from_slice(bytes);
            }),
            Some(&mut move |word: &str, _s, _e, _off, _len, _est| {
                b.lock().expect("boundary lock").push(word.to_string());
            }),
            None,
        )
        .expect("live synthesis");

    let audio = audio.lock().expect("audio lock").clone();
    assert!(
        !audio.is_empty(),
        "synthesis must deliver PCM bytes (got 0)"
    );
    // PCM16 mono: every sample is 2 bytes.
    assert_eq!(audio.len() % 2, 0, "PCM16 byte stream must be even-length");
    eprintln!(
        "delivered {} PCM bytes ({} s at 24 kHz)",
        audio.len(),
        audio.len() / 48_000
    );

    let boundaries = boundaries.lock().expect("boundary lock").clone();
    eprintln!("word boundaries: {boundaries:?}");
    // Timestamp support varies by voice — do not assert non-empty, but a
    // supported voice must report words derived from the spoken text. The
    // server segments/normalizes aggressively (e.g. "， this" carries a
    // normalised comma prefix; "Qwen" may come back hanfied as "困"), so
    // match on substrings rather than exact equality.
    if !boundaries.is_empty() {
        assert_eq!(
            boundaries.first().map(String::as_str),
            Some("Hello"),
            "first boundary word should be the sentence start, got {boundaries:?}"
        );
        assert!(
            boundaries.iter().any(|w| w.contains("this")),
            "boundary words should come from the spoken text, got {boundaries:?}"
        );
    }
}
