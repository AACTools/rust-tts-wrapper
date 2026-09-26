#![cfg(feature = "cloud")]

//! Live AWS Polly tests (SigV4-signed REST). `#[ignore]`-d by default.
//!
//! ```text
//! POLLY_AWS_KEY_ID=... POLLY_AWS_ACCESS_KEY=... POLLY_REGION=us-east-1 \
//!     cargo test --test polly_live -- --ignored --nocapture
//! ```

use rust_tts_wrapper::factory::create_engine;
use std::sync::{Arc, Mutex};

fn creds() -> Option<String> {
    let id = std::env::var("POLLY_AWS_KEY_ID").ok()?;
    let secret = std::env::var("POLLY_AWS_ACCESS_KEY").ok()?;
    let region = std::env::var("POLLY_REGION").unwrap_or_else(|_| "us-east-1".into());
    Some(
        serde_json::json!({
            "accessKeyId": id,
            "secretAccessKey": secret,
            "region": region,
        })
        .to_string(),
    )
}

#[test]
#[ignore = "needs POLLY_AWS_KEY_ID/POLLY_AWS_ACCESS_KEY (billed network call)"]
fn polly_live_synthesizes_and_lists_voices() {
    let Some(creds) = creds() else {
        eprintln!("POLLY_* env vars not set; skipping");
        return;
    };
    let engine = create_engine("polly", &creds).expect("polly engine");
    assert!(engine.check_credentials().unwrap(), "credentials rejected");

    let voices = engine.get_voices().expect("voice list");
    eprintln!("polly voices: {}", voices.len());
    assert!(!voices.is_empty(), "voices list should not be empty");
    assert!(
        voices.iter().any(|v| v.id == "Joanna"),
        "Joanna should be listed"
    );

    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&audio);
    engine
        .speak(
            "Hello! Polly is speaking through rust tts wrapper with signed requests.",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut move |b: &[u8]| sink.lock().unwrap().extend_from_slice(b)),
            None,
            None,
        )
        .expect("live synthesis");
    let mp3 = audio.lock().unwrap().clone();
    assert!(!mp3.is_empty(), "synthesis delivered no MP3 bytes");
    eprintln!("delivered {} MP3 bytes", mp3.len());
}
