//! Advanced Features Demo
//!
//! This example demonstrates advanced features including:
//! - Voice selection and listing
//! - Prosody control (rate, pitch, volume)

// Demo examples are illustrative, not production code — relax pedantic lints.
#![allow(clippy::all, clippy::pedantic)]
//! - Cloud provider comparison
//! - SSML support
//! - Credential validation

use rust_tts_wrapper::create_engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔧 Advanced Features Demo");
    println!("=========================\n");

    // Demo 1: Voice listing and selection
    println!("=== Voice Listing Demo ===\n");

    // The native engine is platform-specific: SAPI on Windows,
    // speech-dispatcher on Linux.
    #[cfg(target_os = "windows")]
    let engines_to_test = vec![("sapi", "Windows SAPI", "")];

    #[cfg(not(target_os = "windows"))]
    let engines_to_test = vec![("system", "System TTS", "")];

    for (engine_id, description, credentials) in engines_to_test {
        println!("🎙️  {} - {}", engine_id, description);

        match create_engine(engine_id, credentials) {
            Some(engine) => {
                match engine.get_voices() {
                    Ok(voices) => {
                        println!("   Available voices ({}):", voices.len());

                        // Show first 5 voices
                        for (i, voice) in voices.iter().take(5).enumerate() {
                            println!(
                                "   {}. {} ({})",
                                i + 1,
                                voice.name,
                                voice.primary_language()
                            );

                            // Try to select and speak with this voice
                            if i < 1 {
                                // Only test first voice to save time
                                match engine.speak(
                                    &format!("Hello, I am {}", voice.name),
                                    Some(&voice.id),
                                    1.0,
                                    1.0,
                                    1.0,
                                    None,
                                    None,
                                ) {
                                    Ok(_) => println!("   ✅ Voice test successful"),
                                    Err(e) => println!("   ⚠️  Voice test failed: {}", e),
                                }
                            }
                        }

                        if voices.len() > 5 {
                            println!("   ... and {} more", voices.len() - 5);
                        }
                    }
                    Err(e) => {
                        println!("   ⚠️  Failed to get voices: {}", e);
                    }
                }
            }
            None => {
                println!("   ⚠️  Engine not available");
            }
        }
        println!();
    }

    // Demo 2: Prosody control
    println!("=== Prosody Control Demo ===\n");
    println!("Testing rate, pitch, and volume adjustments...\n");

    #[cfg(target_os = "windows")]
    let prosody_engine_id = "sapi";
    #[cfg(not(target_os = "windows"))]
    let prosody_engine_id = "system";

    if let Some(engine) = create_engine(prosody_engine_id, "") {
        let test_text = "This demonstrates prosody control in text-to-speech synthesis.";

        println!("🐢 Slow speech (0.7x rate):");
        engine.speak(test_text, None, 0.7, 1.0, 1.0, None, None)?;
        std::thread::sleep(std::time::Duration::from_millis(100));

        println!("🐇 Fast speech (1.5x rate):");
        engine.speak(test_text, None, 1.5, 1.0, 1.0, None, None)?;
        std::thread::sleep(std::time::Duration::from_millis(100));

        println!("🔉 Low pitch (0.8x):");
        engine.speak(test_text, None, 1.0, 0.8, 1.0, None, None)?;
        std::thread::sleep(std::time::Duration::from_millis(100));

        println!("🔈 High pitch (1.2x):");
        engine.speak(test_text, None, 1.0, 1.2, 1.0, None, None)?;
        std::thread::sleep(std::time::Duration::from_millis(100));

        println!("🔉 Low volume (0.5x):");
        engine.speak(test_text, None, 1.0, 1.0, 0.5, None, None)?;
        std::thread::sleep(std::time::Duration::from_millis(100));

        println!("🔊 High volume (1.5x):");
        engine.speak(test_text, None, 1.0, 1.0, 1.5, None, None)?;

        println!("✅ Prosody control demo complete\n");
    }

    // Demo 3: Cloud provider comparison
    println!("=== Cloud Provider Comparison ===\n");
    println!("Testing different cloud TTS providers...\n");

    let cloud_providers = vec![
        ("openai", "OPENAI_API_KEY", "OpenAI", "alloy"),
        (
            "elevenlabs",
            "ELEVENLABS_API_KEY",
            "ElevenLabs",
            "21m00Tcm4TlvDq8ikWAM",
        ),
        (
            "google",
            "GOOGLE_API_KEY",
            "Google Cloud",
            "en-US-Standard-A",
        ),
    ];

    for (provider_id, env_var, provider_name, default_voice) in cloud_providers {
        if let Ok(api_key) = std::env::var(env_var) {
            let credentials = format!("{{\"apiKey\":\"{}\"}}", api_key);

            println!("🌐 Testing {}...", provider_name);

            if let Some(engine) = create_engine(provider_id, &credentials) {
                // Check credentials first
                match engine.check_credentials() {
                    Ok(true) => {
                        println!("   ✅ Credentials valid");

                        // List available voices
                        match engine.get_voices() {
                            Ok(voices) => {
                                println!("   🎙️  Available voices: {}", voices.len());
                                if !voices.is_empty() {
                                    println!("   Default voice: {}", voices[0].name);
                                }
                            }
                            Err(e) => {
                                println!("   ⚠️  Could not list voices: {}", e);
                            }
                        }

                        // Test synthesis
                        let test_text = "This is a test of cloud text-to-speech synthesis.";
                        match engine.speak(
                            test_text,
                            Some(default_voice),
                            1.0,
                            1.0,
                            1.0,
                            None,
                            None,
                        ) {
                            Ok(_) => println!("   ✅ Synthesis successful"),
                            Err(e) => println!("   ❌ Synthesis failed: {}", e),
                        }
                    }
                    Ok(false) => {
                        println!("   ❌ Invalid credentials");
                    }
                    Err(e) => {
                        println!("   ❌ Credential check failed: {}", e);
                    }
                }
            } else {
                println!("   ⚠️  Could not create engine");
            }
        } else {
            println!(
                "⚠️  {} credentials not available (set {})",
                provider_name, env_var
            );
        }
        println!();
    }

    // Demo 4: SSML support
    println!("=== SSML Support Demo ===\n");

    if let Ok(api_key) = std::env::var("AZURE_SUBSCRIPTION_KEY") {
        let region = std::env::var("AZURE_REGION").unwrap_or_else(|_| "eastus".to_string());
        let credentials = format!(
            "{{\"subscriptionKey\":\"{}\",\"region\":\"{}\"}}",
            api_key, region
        );

        if let Some(engine) = create_engine("azure", &credentials) {
            println!("🎙️  Testing Azure SSML support...\n");

            let ssml_example = r#"
                <speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xml:lang="en-US">
                    <voice name="en-US-JennyNeural">
                        <s>This is a demonstration of SSML support.</s>
                        <break time="500ms"/>
                        <s>You can use SSML for advanced control over speech synthesis.</s>
                        <prosody rate="1.2" pitch="+10%">
                            <s>This sentence is faster and higher in pitch.</s>
                        </prosody>
                    </voice>
                </speak>
            "#;

            println!("Testing SSML input...");
            match engine.speak(ssml_example, None, 1.0, 1.0, 1.0, None, None) {
                Ok(_) => println!("✅ SSML synthesis successful"),
                Err(e) => println!("❌ SSML synthesis failed: {}", e),
            }
        }
    } else {
        println!("⚠️  Azure credentials not available");
        println!("   Set AZURE_SUBSCRIPTION_KEY and AZURE_REGION to test SSML\n");
    }

    println!("\n💡 Advanced Features Usage");
    println!("========================\n");
    println!("```rust");
    println!("use rust_tts_wrapper::{{create_engine, tts_engine::TtsEngine}};");
    println!("");
    println!("// Voice selection");
    println!("let voices = engine.get_voices()?;");
    println!("engine.speak(text, Some(&voices[0].id), 1.0, 1.0, 1.0, None, None)?;");
    println!("");
    println!("// Prosody control");
    println!("engine.speak(");
    println!("    text,");
    println!("    Some(&voice_id),");
    println!("    1.2,  // rate: 20% faster");
    println!("    1.1,  // pitch: 10% higher");
    println!("    0.8,  // volume: 80%");
    println!("    None, None");
    println!(")?;");
    println!("");
    println!("// Credential validation");
    println!("if engine.check_credentials()? {{");
    println!("    println!(\"Credentials valid\");");
    println!("}}");
    println!("```\n");

    println!("🎯 Advanced features enable fine-tuned control over");
    println!("   speech synthesis for professional applications!");

    Ok(())
}
