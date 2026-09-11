//! Simple test example to verify API usage

// Demo examples are illustrative, not production code — relax pedantic lints.
#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::create_engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Testing Rust TTS Wrapper API...\n");

    // Test 1: Create system engine
    println!("Test 1: Creating system engine...");
    // The native credential-free engine is platform-specific: SAPI on Windows,
    // speech-dispatcher on Linux. Run with `--features sapi` on Windows or
    // `--features system` on Linux.
    #[cfg(target_os = "windows")]
    let native_engine_id = "sapi";
    #[cfg(not(target_os = "windows"))]
    let native_engine_id = "system";
    let engine = create_engine(native_engine_id, "").expect("Failed to create engine");
    println!("✅ Engine created successfully");

    // Test 2: Get voices
    println!("\nTest 2: Getting available voices...");
    let voices = engine.get_voices()?;
    println!("✅ Found {} voices", voices.len());

    // Test 3: Synthesize to bytes
    println!("\nTest 3: Synthesizing audio to bytes...");
    let audio = engine.synth_to_bytes("Hello, World!", None, 1.0, 1.0, 1.0)?;
    println!("✅ Generated {} bytes of audio", audio.len());

    // Test 4: Save to file
    println!("\nTest 4: Saving audio to file...");
    std::fs::create_dir_all("examples/output")?;
    std::fs::write("examples/output/test.wav", &audio)?;
    println!("✅ Saved to examples/output/test.wav");

    println!("\n🎯 All tests passed!");
    Ok(())
}
