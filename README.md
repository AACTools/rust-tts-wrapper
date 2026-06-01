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

tts_ctx* ctx = tts_create("system", NULL);
tts_speak(ctx, "Hello world");
tts_destroy(ctx);
```

## Usage (Rust)

```rust
use rust_tts_wrapper::factory;

let engine = factory::create_engine("system", "").unwrap();
engine.speak("Hello world", None, 1.0, 1.0, 1.0).unwrap();
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
