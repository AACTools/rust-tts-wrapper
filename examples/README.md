# Rust TTS Wrapper Examples

This directory contains comprehensive examples demonstrating the capabilities of the Rust TTS Wrapper library.

## Available Examples

### 1. Word Boundary Events Demo
**File:** `word-boundary-demo.rs`

Demonstrates real-time word boundary events for precise speech synchronization - perfect for applications requiring word highlighting during playback.

**Features:**
- Real-time word highlighting during speech
- Timing information for each word
- Multiple engine support (System, SAPI, SherpaOnnx)
- Cloud provider timing data (Azure, ElevenLabs)

**Run:**
```bash
cargo run --example word-boundary-demo
```

### 2. Streaming Audio Demo
**File:** `streaming-audio-demo.rs`

Shows how to stream audio synthesis and save to files using both real-time streaming and direct synthesis approaches.

**Features:**
- Real-time audio streaming to files
- Direct byte synthesis for batch processing
- Multiple format support (WAV, MP3)
- Cloud provider integration (OpenAI, ElevenLabs, Azure)

**Run:**
```bash
cargo run --example streaming-audio-demo
```

### 3. Advanced Features Demo
**File:** `advanced-features-demo.rs`

Comprehensive demo showing advanced TTS capabilities including voice selection, prosody control, and SSML support.

**Features:**
- Voice listing and selection
- Prosody control (rate, pitch, volume)
- Cloud provider comparison
- SSML support (Azure)
- Credential validation

**Run:**
```bash
cargo run --example advanced-features-demo
```

## Quick Start

### Basic Speech Synthesis

```rust
use rust_tts_wrapper::{create_engine, TtsEngine};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a system TTS engine
    let engine = create_engine("system", "")
        .expect("Failed to create engine");

    // Simple speech synthesis
    engine.speak(
        "Hello, world!",
        None,  // Use default voice
        1.0,   // Normal rate
        1.0,   // Normal pitch
        1.0,   // Normal volume
        None,  // No audio callback
        None,  // No boundary callback
    )?;

    Ok(())
}
```

### Streaming to File

```rust
use std::fs::File;
use std::io::Write;
use rust_tts_wrapper::{create_engine, TtsEngine};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = create_engine("system", "")?;
    let mut output = File::create("output.wav")?;

    engine.speak(
        "Hello, world!",
        None, 1.0, 1.0, 1.0,
        Some(&mut |chunk: &[u8]| {
            // Stream audio chunks to file
            output.write_all(chunk)?;
            Ok(())
        }),
        None,
    )?;

    Ok(())
}
```

### Word Boundary Events

```rust
use std::sync::{Arc, Mutex};
use rust_tts_wrapper::{create_engine, TtsEngine};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = create_engine("system", "")?;
    let words = Arc::new(Mutex::new(Vec::new()));
    let words_clone = words.clone();

    engine.speak(
        "Hello world! This demonstrates word timing.",
        None, 1.0, 1.0, 1.0,
        None,
        Some(&mut |word: &str, start: f32, end: f32, _offset: i32, _len: i32| {
            let mut word_list = words_clone.lock().unwrap();
            word_list.push((word.to_string(), start, end));
            println!("Word: '{}' at {:.2}-{:.2}s", word, start, end);
        }),
    )?;

    Ok(())
}
```

## Cloud Provider Setup

### OpenAI
```bash
export OPENAI_API_KEY="your-key-here"
```

### Azure
```bash
export AZURE_SUBSCRIPTION_KEY="your-key-here"
export AZURE_REGION="eastus"  # or your region
```

### ElevenLabs
```bash
export ELEVENLABS_API_KEY="your-key-here"
```

### Google Cloud
```bash
export GOOGLE_API_KEY="your-key-here"
```

## Engine Support Matrix

| Engine | Requires Credentials | Word Boundaries | Notes |
|--------|---------------------|-----------------|-------|
| system | No | Estimated | Uses eSpeak/Speech Dispatcher |
| sherpaonnx | No | Estimated | Requires local models |
| sapi | No | Estimated | Windows only |
| openai | Yes | No | High quality MP3 output |
| elevenlabs | Yes | Yes | Character-level timing |
| azure | Yes | Yes | Word-level timing, SSML support |
| google | Yes | No | Base64 audio output |

## Building Examples

All examples can be built and run using cargo:

```bash
# Build all examples
cargo build --examples

# Run specific example
cargo run --example word-boundary-demo

# Run with features
cargo run --example streaming-audio-demo --features cloud,sherpaonnx
```

## Output Files

Some examples create output files in `examples/output/`:
- `*_streaming.wav/mp3` - Real-time streaming output
- `*_direct.wav/mp3` - Direct synthesis output

## Thread Safety

All engines are thread-safe and implement `Send + Sync`. You can safely share them across threads:

```rust
use std::sync::Arc;
use rust_tts_wrapper::{create_engine, TtsEngine};

let engine = Arc::new(
    create_engine("system", "")?
);

// Share across multiple threads
let engine_clone = engine.clone();
std::thread::spawn(move || {
    engine_clone.speak("Hello from thread", None, 1.0, 1.0, 1.0, None, None)
}).join().unwrap??;
```

## Error Handling

All operations return `Result<T, Box<dyn Error>>` for comprehensive error handling:

```rust
use rust_tts_wrapper::{create_engine, TtsEngine};

match engine.speak(...) {
    Ok(_) => println!("Success!"),
    Err(e) => eprintln!("Error: {}", e),
}
```

## Platform Notes

### Windows
- SAPI engine available with `--features sapi`
- COM initialization handled automatically
- Default voice selection works best

### Linux
- System engine requires speech-dispatcher
- eSpeak provides good offline TTS
- SherpaOnnx requires model files

### macOS
- System engine uses built-in say command
- AvSynth available with `--features avsynth`

## Performance Tips

1. **Reuse engine instances** - Creating engines is expensive
2. **Use streaming for long texts** - Better memory efficiency
3. **Batch short texts** - Reduces initialization overhead
4. **Consider async** - Use async engines for concurrent operations

## Troubleshooting

### Engine not available
```
Error: Engine "sherpaonnx" not available
```
**Solution:** Ensure required features are enabled: `--features sherpaonnx`

### Credentials invalid
```
Error: Invalid credentials
```
**Solution:** Check your API keys and environment variables

### Missing dependencies
```
Error: COM initialization failed
```
**Solution:** Ensure Windows Speech components are installed

## Additional Resources

- [Main Documentation](../../README.md)
- [API Documentation](https://docs.rs/rust-tts-wrapper)
- [FFI Reference](../../docs/FFI.md)
- [JavaScript Version](../js-tts-wrapper/)