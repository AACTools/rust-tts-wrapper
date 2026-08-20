//! floravox engine demo: SSML synthesis with measured word boundaries.
//!
//! Usage:
//!   cargo run --features floravox --example floravox-demo -- \
//!       --model-dir ~/.rust-tts-wrapper/floravox --model en_US-lessac-low
//!
//! Falls back to the default models dir when no arguments are given.

// Demo examples are illustrative, not production code — relax pedantic lints.
#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::floravox_engine::FloravoxEngine;
use rust_tts_wrapper::TtsEngine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut models_dir = None;
    let mut model = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model-dir" => models_dir = args.next(),
            "--model" => model = args.next(),
            other => return Err(format!("unknown flag {other:?}").into()),
        }
    }
    let creds = serde_json::json!({
        "modelsDir": models_dir,
        "modelId": model,
    })
    .to_string();
    let engine = FloravoxEngine::new(&creds);

    println!("voices: {:?}", engine.get_voices()?);

    let (pcm, boundaries) = engine.synth_with_boundaries(
        "<speak>Hello <mark name=\"m\"/>world, <break time=\"200ms\"/>fluoravox speaks SSML.</speak>",
        None,
        1.0,
        1.0,
        1.0,
    )?;

    println!("audio: {} bytes of PCM16", pcm.len());
    println!("word boundaries (from the model's duration tensor):");
    for b in &boundaries {
        println!("  {:?} at {} ms ({} ms long)", b.text, b.offset, b.duration);
    }
    Ok(())
}
