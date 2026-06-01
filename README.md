# rust-tts-wrapper

Cross-platform TTS (Text-to-Speech) wrapper with C API. Mirrors [js-tts-wrapper](https://github.com/AACTools/js-tts-wrapper) and [swift-tts-wrapper](https://github.com/AACTools/swift-tts-wrapper).

## Engines (21 total)

| Engine | Type | Credentials | Voice List | Word Boundaries |
|--------|------|-------------|------------|-----------------|
| System (speech-dispatcher) | Local | None | — | Estimated |
| Sherpa-ONNX | Local (191 models) | None | Speakers from registry | Estimated |
| OpenAI | Cloud | API Key | — | Estimated |
| ElevenLabs | Cloud | API Key | API | Estimated |
| Azure | Cloud | Subscription Key + Region | API | Estimated |
| Google Cloud | Cloud | API Key | API | Real (timepoints via v1beta1) |
| Amazon Polly | Cloud | Access Key + Secret + Region | — | Estimated |
| Cartesia | Cloud | API Key | API | Estimated |
| Deepgram | Cloud | API Key | — | Estimated |
| PlayHT | Cloud | API Key + User ID | — | Estimated |
| Fish Audio | Cloud | API Key | — | Estimated |
| Hume AI | Cloud | API Key | — | Estimated |
| Mistral | Cloud | API Key | — | Estimated |
| Murf | Cloud | API Key | — | Estimated |
| Resemble AI | Cloud | API Key | — | Estimated |
| Unreal Speech | Cloud | API Key | — | Estimated |
| UpliftAI | Cloud | API Key | — | Estimated |
| IBM Watson | Cloud | API Key + Region + Instance ID | — | Estimated |
| Wit.ai | Cloud | Token | — | Estimated |
| xAI | Cloud | API Key | — | Estimated |
| ModelsLab | Cloud | API Key | — | Estimated |

## Key Features

- **Unified Voice struct** matching js-tts-wrapper and swift-tts-wrapper with `language_codes` array, `provider` field, and normalized gender
- **Streaming audio** via chunked HTTP reads (8KB chunks) through the `on_audio` callback
- **Word boundary events** via `on_boundary` callback — real API timing for Google (v1beta1 timepoints with SSML marks), estimated boundaries for all other engines
- **Azure SSML support** — proper SSML generation with XML escaping, voice selection, and prosody tags
- **Google REST API** — correct JSON body structure with optional timepoint support
- **Voice enumeration** for Azure, Google, ElevenLabs, Cartesia, and other engines with list APIs
- **Word timing estimation** matching the algorithm in JS and Swift (word-length-adjusted, 150 WPM baseline)
- **C ABI** for bindings to Python, .NET, Swift, and other languages
- **Sherpa-ONNX** offline TTS with 191 models from bundled registry

## Usage (C API)

```c
#include "tts_wrapper.h"
#include <stdio.h>

void on_audio(const uint8_t* chunk, uintptr_t size, void* userdata) {
    // Handle streaming audio chunks
    printf("Received %zu bytes of audio\n", size);
}

void on_boundary(const char* word, float start_time, float end_time, void* userdata) {
    // Handle word boundary events
    printf("Word '%s' from %.2f to %.2f\n", word, start_time, end_time);
}

int main() {
    tts_ctx* ctx = tts_create("elevenlabs", "{\"apiKey\":\"your-api-key\"}");

    tts_set_on_audio(ctx, on_audio, NULL);
    tts_set_on_boundary(ctx, on_boundary, NULL);

    tts_set_voice(ctx, "Rachel");
    tts_set_rate(ctx, 1.0);

    tts_speak_sync(ctx, "Hello world, streaming is supported.");

    tts_destroy(ctx);
    return 0;
}
```

## Usage (Rust)

```rust
use rust_tts_wrapper::factory;

let engine = factory::create_engine("openai", r#"{"apiKey":"your-api-key"}"#).unwrap();

// Standard speaking
engine.speak("Hello world", Some("alloy"), 1.0, 1.0, 1.0, None, None).unwrap();

// Streaming with word boundary callbacks
let mut audio_cb = |chunk: &[u8]| {
    println!("Received audio chunk of size {}", chunk.len());
};

let mut boundary_cb = |word: &str, start: f32, end: f32| {
    println!("Word '{}' from {:.3} to {:.3}", word, start, end);
};

engine.speak_sync(
    "Hello world, streaming is supported.",
    Some("alloy"),
    1.0, 1.0, 1.0,
    Some(&mut audio_cb),
    Some(&mut boundary_cb),
).unwrap();

// List voices
let voices = engine.get_voices().unwrap();
for v in &voices {
    println!("{} ({}) - {}", v.name, v.provider, v.primary_language());
}
```

## Build

```bash
cargo build --all-features
```

### Features

- `system` — speech-dispatcher (Linux system TTS)
- `cloud` — all 19 cloud engines via HTTP
- `sherpaonnx` — Sherpa-ONNX offline TTS (191 models)

## Architecture

```
              TtsEngine (trait)
                    |
      +-------------+-------------+
      |             |             |
  SystemEngine  CloudEngine  SherpaOnnxEngine
  (speech-       (19 cloud      (191 local
  dispatcher)    providers)     models)
```

The `CloudEngine` uses a provider-specific configuration (`CloudConfig`) to handle differences in API structure — Azure sends SSML XML, Google sends JSON with base64 audio, and all others use standard JSON bodies.

### Voice Struct (Unified)

```rust
pub struct Voice {
    pub id: String,
    pub name: String,
    pub gender: String,          // "Male", "Female", "Unknown"
    pub provider: String,        // "azure", "google", etc.
    pub language_codes: Vec<LanguageCode>,
}

pub struct LanguageCode {
    pub bcp47: String,           // "en-US"
    pub iso639_3: String,        // "eng"
    pub display: String,         // "English (United States)"
}
```

### Word Boundaries

```rust
pub struct WordBoundary {
    pub text: String,
    pub offset: u64,    // milliseconds from start
    pub duration: u64,  // milliseconds
}
```

## Sherpa-ONNX Models

191 models from the merged_models.json registry. Models auto-download on first use to `~/.rust-tts-wrapper/sherpaonnx/`.

## Bindings

- `bindings/python/` — Python via ctypes
- `bindings/dotnet/` — .NET via P/Invoke
- `bindings/swift/` — Swift via C interop

## License

MIT
