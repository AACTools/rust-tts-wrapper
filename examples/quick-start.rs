//! Quick Start Example
//!
//! A simple getting-started example demonstrating basic TTS usage.
//! This is the easiest way to get started with the Rust TTS Wrapper.

// Demo examples are illustrative, not production code — relax pedantic lints
// (matches the convention in tests/sherpaonnx_live.rs).
#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::create_engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎙️  Rust TTS Wrapper - Quick Start");
    println!("==================================\n");

    // Example 1: Simple speech synthesis
    println!("Example 1: Basic Speech Synthesis");
    println!("-----------------------------------\n");

    // The native credential-free engine is platform-specific: SAPI on Windows,
    // speech-dispatcher on Linux. Run with `--features sapi` on Windows or
    // `--features system` on Linux.
    #[cfg(target_os = "windows")]
    let native_engine_id = "sapi";
    #[cfg(not(target_os = "windows"))]
    let native_engine_id = "system";
    let engine = create_engine(native_engine_id, "").expect("Failed to create native TTS engine");

    println!("🔊 Speaking: 'Hello, World!'\n");

    engine.speak(
        "Hello, World!",
        None, // Use default voice
        1.0,  // Normal rate
        1.0,  // Normal pitch
        1.0,  // Normal volume
        None, // No audio callback (direct playback)
        None, // No word boundary callback
    )?;

    std::thread::sleep(std::time::Duration::from_millis(500));

    // Example 2: Custom voice and prosody
    println!("\nExample 2: Voice Selection and Prosody");
    println!("----------------------------------------\n");

    println!("🎙️  Listing available voices...\n");

    match engine.get_voices() {
        Ok(voices) => {
            println!("Found {} voices:\n", voices.len());

            for (i, voice) in voices.iter().take(5).enumerate() {
                println!("  {}. {} ({})", i + 1, voice.name, voice.primary_language());

                if i == 0 {
                    println!("     Testing with this voice...\n");

                    engine.speak(
                        &format!("This is {} speaking.", voice.name),
                        Some(&voice.id),
                        1.1, // Slightly faster
                        1.0, // Normal pitch
                        1.2, // Louder volume
                        None,
                        None,
                    )?;

                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }

            if voices.len() > 5 {
                println!("  ... and {} more voices\n", voices.len() - 5);
            }
        }
        Err(e) => {
            println!("Could not list voices: {}\n", e);
        }
    }

    // Example 3: Rate and pitch control
    println!("Example 3: Rate and Pitch Control");
    println!("----------------------------------\n");

    let test_sentence = "This demonstrates rate and pitch control in text-to-speech.";

    println!("🐢 Slow speech:");
    engine.speak(test_sentence, None, 0.7, 1.0, 1.0, None, None)?;
    std::thread::sleep(std::time::Duration::from_millis(100));

    println!("🎯 Normal speech:");
    engine.speak(test_sentence, None, 1.0, 1.0, 1.0, None, None)?;
    std::thread::sleep(std::time::Duration::from_millis(100));

    println!("🐇 Fast speech:");
    engine.speak(test_sentence, None, 1.5, 1.0, 1.0, None, None)?;
    std::thread::sleep(std::time::Duration::from_millis(100));

    println!("🔈 Low pitch:");
    engine.speak(test_sentence, None, 1.0, 0.8, 1.0, None, None)?;
    std::thread::sleep(std::time::Duration::from_millis(100));

    println!("🔊 High pitch:");
    engine.speak(test_sentence, None, 1.0, 1.2, 1.0, None, None)?;

    // Example 4: Saving to file
    println!("\n\nExample 4: Saving Audio to File");
    println!("--------------------------------\n");

    let audio =
        engine.synth_to_bytes("This audio will be saved to a file.", None, 1.0, 1.0, 1.0)?;

    println!("📁 Generated {} bytes of audio", audio.len());

    // Create output directory
    std::fs::create_dir_all("examples/output")?;

    // Save to file
    let output_path = "examples/output/quick_start_demo.wav";
    std::fs::write(output_path, &audio)?;

    println!("✅ Audio saved to: {}", output_path);

    println!("\n💡 Quick Start Tips");
    println!("====================\n");
    println!("```rust");
    println!("use rust_tts_wrapper::{{create_engine, tts_engine::TtsEngine}};");
    println!("");
    println!("// Create engine");
    println!("let engine = create_engine(\"system\", \"\")?;");
    println!("");
    println!("// Simple speech");
    println!("engine.speak(\"Hello!\", None, 1.0, 1.0, 1.0, None, None)?;");
    println!("");
    println!("// Save to file");
    println!("let audio = engine.synth_to_bytes(\"Hello file!\", None, 1.0, 1.0, 1.0)?;");
    println!("std::fs::write(\"output.wav\", audio)?;");
    println!("");
    println!("// List voices");
    println!("let voices = engine.get_voices()?;");
    println!("println!(\"Available voices: {{}}\", voices.len());");
    println!("```");

    println!("\n🎯 Next Steps:");
    println!("   - Run `word-boundary-demo` for real-time word highlighting");
    println!("   - Run `streaming-audio-demo` for audio file generation");
    println!("   - Run `advanced-features-demo` for cloud providers and SSML");
    println!("   - Set up API keys for cloud TTS providers");

    Ok(())
}
