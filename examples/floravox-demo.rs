//! floravox engine demo: SSML synthesis with measured word boundaries.
//!
//! Usage:
//!   cargo run --features floravox --example floravox-demo -- //!       --model-dir DIR --model NAME [--creds '{"misaki":"us"}']

// Demo examples are illustrative, not production code — relax pedantic lints.
#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::floravox_engine::FloravoxEngine;
use rust_tts_wrapper::TtsEngine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut creds = serde_json::json!({});
    let mut text: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model-dir" => {
                if let Some(d) = args.next() {
                    creds["modelsDir"] = serde_json::Value::String(d);
                }
            }
            "--model" => {
                if let Some(m) = args.next() {
                    creds["modelId"] = serde_json::Value::String(m);
                }
            }
            "--text" => {
                if let Some(t) = args.next() {
                    text = Some(t);
                }
            }
            "--creds" => {
                if let Some(c) = args.next() {
                    creds = serde_json::from_str(&c)?;
                }
            }
            other => return Err(format!("unknown flag {other:?}").into()),
        }
    }
    let engine = FloravoxEngine::new(&creds.to_string());
    println!("voices: {:?}", engine.get_voices()?);
    let (pcm, boundaries) = engine.synth_with_boundaries(
        text.as_deref().unwrap_or(
            "<speak>Hello <mark name=\"m\"/>world, floravox speaks measured timing.</speak>",
        ),
        None,
        1.0,
        1.0,
        1.0,
    )?;
    println!("audio: {} bytes of PCM16", pcm.len());
    println!("word boundaries:");
    for b in &boundaries {
        println!("  {:?} at {} ms ({} ms long)", b.text, b.offset, b.duration);
    }
    Ok(())
}
