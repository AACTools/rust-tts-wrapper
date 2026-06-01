# rust-tts-wrapper

Cross-platform TTS (Text-to-Speech) wrapper with C API. Mirrors [js-tts-wrapper](https://github.com/user/js-tts-wrapper) and SwiftTTSWrapper.

## Engines (21 total)

| Engine | Type | Credentials |
|--------|------|-------------|
| System (speech-dispatcher) | Local | None |
| Sherpa-ONNX | Local (191 models) | None |
| OpenAI | Cloud | API Key |
| ElevenLabs | Cloud | API Key |
| Azure | Cloud | Subscription Key + Region |
| Google Cloud | Cloud | API Key |
| Amazon Polly | Cloud | Access Key + Secret + Region |
| Cartesia | Cloud | API Key |
| Deepgram | Cloud | API Key |
| PlayHT | Cloud | API Key + User ID |
| Fish Audio | Cloud | API Key |
| Hume AI | Cloud | API Key |
| Mistral | Cloud | API Key |
| Murf | Cloud | API Key |
| Resemble AI | Cloud | API Key |
| Unreal Speech | Cloud | API Key |
| UpliftAI | Cloud | API Key |
| IBM Watson | Cloud | API Key + Region + Instance ID |
| Wit.ai | Cloud | Token |
| xAI | Cloud | API Key |
| ModelsLab | Cloud | API Key |

## Usage (C API)

```c
#include "tts_wrapper.h"
#include <stdio.h>

void on_audio(const uint8_t* chunk, uintptr_t size, void* userdata) {
    // Handle streaming audio chunks here
    printf("Received %zu bytes of audio\n", size);
}

void on_boundary(const char* word, float start_time, float end_time, void* userdata) {
    // Handle word boundary events
    printf("Word '%s' from %.2f to %.2f\n", word, start_time, end_time);
}

int main() {
    // 1. Create engine (e.g., ElevenLabs with API key)
    tts_ctx* ctx = tts_create("elevenlabs", "{\"apiKey\":\"your-api-key\"}");

    // 2. Register callbacks for streaming and word events
    tts_set_on_audio(ctx, on_audio, NULL);
    tts_set_on_boundary(ctx, on_boundary, NULL);

    // 3. Set voice and properties
    tts_set_voice(ctx, "Rachel");
    tts_set_rate(ctx, 1.0);

    // 4. Speak (blocks until complete when using speak_sync)
    tts_speak_sync(ctx, "Hello world, streaming is supported.");

    // 5. Cleanup
    tts_destroy(ctx);
    return 0;
}
```

## Usage (Rust)

```rust
use rust_tts_wrapper::factory;

let engine = factory::create_engine("elevenlabs", r#"{"apiKey":"your-api-key"}"#).unwrap();

// Standard speaking
engine.speak("Hello world", Some("Rachel"), 1.0, 1.0, 1.0, None, None).unwrap();

// Speaking with streaming and boundary callbacks
let mut audio_cb = |chunk: &[u8]| {
    println!("Received audio chunk of size {}", chunk.len());
};

let mut boundary_cb = |word: &str, start: f32, end: f32| {
    println!("Word {} from {} to {}", word, start, end);
};

engine.speak_sync(
    "Hello world, streaming is supported.",
    Some("Rachel"),
    1.0,
    1.0,
    1.0,
    Some(&mut audio_cb),
    Some(&mut boundary_cb),
).unwrap();
```

## Build

```bash
cargo build --all-features
```

### Features

- `system` — speech-dispatcher (Linux system TTS)
- `cloud` — all 19 cloud engines via HTTP
- `sherpaonnx` — Sherpa-ONNX offline TTS (191 models)

## Sherpa-ONNX Models

191 models from the merged_models.json registry. Models auto-download on first use to `~/.rust-tts-wrapper/sherpaonnx/`.

## Bindings

- `bindings/python/` — Python via ctypes
- `bindings/dotnet/` — .NET via P/Invoke
- `bindings/swift/` — Swift via C interop

## License

MIT
