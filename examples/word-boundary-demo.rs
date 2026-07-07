//! Word Boundary Events Demo
//!
//! This example demonstrates the word boundary event system that enables
//! real-time word highlighting during speech synthesis - perfect for
//! applications like Grid3 that need precise speech synchronization.

// Demo examples are illustrative, not production code — relax pedantic lints.
#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::{create_engine, TtsEngine};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎯 Word Boundary Events Demo");
    println!("============================\n");
    println!("This demo shows how to use word boundary events for real-time");
    println!("word highlighting during speech synthesis.\n");

    let test_text = "Hello world! This demonstrates precise timing events.";

    // Test with credential-free engines first. The native engine is
    // platform-specific: SAPI on Windows, speech-dispatcher on Linux.
    #[cfg(target_os = "windows")]
    let engines = vec![("sapi", "Windows SAPI", true)];

    #[cfg(not(target_os = "windows"))]
    let engines = vec![("system", "System TTS (eSpeak/Speech Dispatcher)", true)];

    for (engine_id, description, is_available) in engines {
        if !is_available {
            continue;
        }

        println!("=== {} Demo ===", engine_id);
        println!("{}\n", description);

        match create_engine(engine_id, "") {
            Some(engine) => {
                if let Err(e) = demonstrate_word_boundaries(&*engine, test_text) {
                    println!("❌ {} failed: {}\n", engine_id, e);
                }
            }
            None => {
                println!("⚠️  {} engine not available\n", engine_id);
            }
        }
    }

    // Demonstrate cloud engines with credentials
    println!("=== Cloud Engine Word Boundary Demo ===");
    println!("Testing with real timing data from cloud providers\n");

    // Try Azure if credentials are available
    if let Ok(api_key) = std::env::var("AZURE_SUBSCRIPTION_KEY") {
        let region = std::env::var("AZURE_REGION").unwrap_or_else(|_| "eastus".to_string());
        let credentials = format!(
            "{{\"subscriptionKey\":\"{}\",\"region\":\"{}\"}}",
            api_key, region
        );

        if let Some(azure_engine) = create_engine("azure", &credentials) {
            println!("🎵 Testing Azure with real word boundary timing...\n");
            if let Err(e) = demonstrate_word_boundaries(&*azure_engine, test_text) {
                println!("❌ Azure failed: {}\n", e);
            }
        }
    } else {
        println!("⚠️  Azure credentials not available");
        println!("   Set AZURE_SUBSCRIPTION_KEY and AZURE_REGION to test\n");
    }

    // Usage instructions
    println!("💡 Usage in Your Application");
    println!("============================\n");
    println!("```rust");
    println!("use rust_tts_wrapper::{{create_engine, tts_engine::TtsEngine}};");
    println!("use std::sync::{{Arc, Mutex}};");
    println!("");
    println!("// Create engine");
    println!("let engine = create_engine(\"system\", \"\")?;");
    println!("");
    println!("// Set up word highlighting");
    println!("let words = Arc::new(Mutex::new(Vec::new()));");
    println!("let words_clone = words.clone();");
    println!("");
    println!("engine.speak(");
    println!("    \"Hello world\",");
    println!("    None,");
    println!("    1.0, 1.0, 1.0,");
    println!("    None,  // no audio callback");
    println!("    Some(&mut |word, start, end, offset, len| {{");
    println!("        words_clone.lock().unwrap().push((word.to_string(), start, end));");
    println!("        println!(\"Word: {{}}\", word);");
    println!("    }})");
    println!(")?;");
    println!("```\n");

    println!("🎯 Perfect for applications requiring precise speech");
    println!("   synchronization and word highlighting!");

    Ok(())
}

fn demonstrate_word_boundaries(
    engine: &dyn TtsEngine,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let words = Arc::new(Mutex::new(Vec::new()));
    let words_clone = words.clone();
    let word_count = Arc::new(Mutex::new(0));
    let count_clone = word_count.clone();

    println!("📝 Text to speak:");
    println!("   \"{}\"\n", text);

    println!("🎵 Starting speech synthesis with word boundary events...\n");

    // Set up word boundary callback
    let mut boundary_callback = move |word: &str, start: f32, end: f32, _offset: i32, _len: i32| {
        let mut word_list = words_clone.lock().unwrap();
        word_list.push((word.to_string(), start, end));

        // Simulate word highlighting in a UI
        let current_word = word;
        let highlighted = word_list
            .iter()
            .map(|(w, _, _)| {
                if w.to_lowercase() == current_word.to_lowercase() {
                    format!("[{}]", w)
                } else {
                    w.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        println!(
            "📍 Word: \"{}\" | Time: {:.3}s - {:.3}s",
            current_word, start, end
        );
        println!("   Highlighted: {}", highlighted);

        *count_clone.lock().unwrap() += 1;
    };

    // Start speaking with word boundary events
    engine.speak(
        text,
        None, // voice
        1.0,  // rate
        1.0,  // pitch
        1.0,  // volume
        None, // no audio callback for this demo
        Some(&mut boundary_callback),
    )?;

    // Give time for all events to complete
    std::thread::sleep(Duration::from_millis(500));

    let final_count = word_count.lock().unwrap();
    println!(
        "🏁 Speech completed - processed {} word events\n",
        *final_count
    );

    Ok(())
}
