//! Streaming Audio Demo
//!
//! This example demonstrates streaming audio synthesis and saving to files.
//! Shows both streaming playback and direct file output.

// Demo examples are illustrative, not production code — relax pedantic lints.
#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::{create_engine, TtsEngine};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎵 Streaming Audio Demo");
    println!("======================\n");
    println!("This demo shows streaming audio synthesis and file output.\n");

    let test_text = "Hello! This is a streaming audio demonstration. The audio is being generated in real-time.";

    // Create output directory
    let output_dir = PathBuf::from("examples/output");
    std::fs::create_dir_all(&output_dir)?;

    // Test different engines. The native engine is platform-specific:
    // SAPI on Windows, speech-dispatcher on Linux.
    #[cfg(target_os = "windows")]
    let engines = vec![("sapi", "Windows SAPI", "")];

    #[cfg(not(target_os = "windows"))]
    let engines = vec![("system", "System TTS", "")];

    for (engine_id, description, credentials) in engines {
        println!("=== {} Demo ===", description);

        match create_engine(engine_id, credentials) {
            Some(engine) => {
                // Example 1: Stream to file
                let output_file = output_dir.join(format!("{}_streaming.wav", engine_id));
                if let Err(e) = stream_to_file(&*engine, test_text, &output_file) {
                    println!("❌ Streaming failed: {}", e);
                } else {
                    println!("✅ Streaming saved to: {}", output_file.display());
                }

                // Example 2: Direct synthesis to bytes
                let output_file = output_dir.join(format!("{}_direct.wav", engine_id));
                if let Err(e) = synthesize_to_file(&*engine, test_text, &output_file) {
                    println!("❌ Direct synthesis failed: {}", e);
                } else {
                    println!("✅ Direct synthesis saved to: {}", output_file.display());
                }

                println!();
            }
            None => {
                println!("⚠️  {} engine not available\n", engine_id);
            }
        }
    }

    // Test cloud engines with credentials
    println!("=== Cloud Engine Streaming Demo ===");

    // Try OpenAI if credentials are available
    if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
        let credentials = format!("{{\"apiKey\":\"{}\"}}", api_key);

        if let Some(openai_engine) = create_engine("openai", &credentials) {
            let output_file = output_dir.join("openai_streaming.mp3");
            println!("🎵 Testing OpenAI streaming...");

            if let Err(e) = stream_to_file(&*openai_engine, test_text, &output_file) {
                println!("❌ OpenAI streaming failed: {}", e);
            } else {
                println!("✅ OpenAI streaming saved to: {}", output_file.display());
            }
        }
    } else {
        println!("⚠️  OpenAI credentials not available");
        println!("   Set OPENAI_API_KEY to test cloud streaming\n");
    }

    println!("\n💡 File Output Options");
    println!("======================\n");
    println!("```rust");
    println!("use rust_tts_wrapper::{{create_engine, tts_engine::TtsEngine}};");
    println!("use std::fs::File;");
    println!("use std::io::Write;");
    println!("");
    println!("// Method 1: Stream to file (real-time chunks)");
    println!("let mut output = File::create(\"output.wav\")?;");
    println!("engine.speak(");
    println!("    text,");
    println!("    None, 1.0, 1.0, 1.0,");
    println!("    Some(&mut |chunk: &[u8]| {{");
    println!("        output.write_all(chunk)?;");
    println!("    }}),");
    println!("    None");
    println!(")?;");
    println!("");
    println!("// Method 2: Direct synthesis to bytes");
    println!("let audio = engine.synth_to_bytes(text, None, 1.0, 1.0, 1.0)?;");
    println!("let mut output = File::create(\"output.wav\")?;");
    println!("output.write_all(&audio)?;");
    println!("```\n");

    println!("🎯 Both methods produce the same audio output!");
    println!("   Streaming is useful for real-time playback and progress tracking.");
    println!("   Direct synthesis is simpler for batch processing.");

    Ok(())
}

/// Stream audio to file in real-time chunks
fn stream_to_file(
    engine: &dyn TtsEngine,
    text: &str,
    output_path: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut output = File::create(output_path)?;
    let mut chunk_count = 0;
    let mut total_bytes = 0;

    println!("   📡 Streaming to file...");

    engine.speak(
        text,
        None, // voice
        1.0,  // rate
        1.0,  // pitch
        1.0,  // volume
        Some(&mut |chunk: &[u8]| {
            // Process audio chunks as they arrive
            chunk_count += 1;
            total_bytes += chunk.len();
            let _ = output.write_all(chunk);

            // Optional: Show progress
            if chunk_count % 10 == 0 {
                println!(
                    "   📦 Received {} chunks ({} bytes)",
                    chunk_count, total_bytes
                );
            }
        }),
        None, // no boundary callback
    )?;

    println!(
        "   ✅ Streaming complete: {} chunks, {} bytes",
        chunk_count, total_bytes
    );
    Ok(())
}

/// Synthesize entire audio to memory, then write to file
fn synthesize_to_file(
    engine: &dyn TtsEngine,
    text: &str,
    output_path: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("   🧪 Synthesizing to bytes...");

    let audio = engine.synth_to_bytes(text, None, 1.0, 1.0, 1.0)?;

    println!("   📊 Generated {} bytes of audio", audio.len());

    let mut output = File::create(output_path)?;
    output.write_all(&audio)?;

    println!("   ✅ File written successfully");
    Ok(())
}
