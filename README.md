# rust-tts-wrapper

Cross-platform text-to-speech for Rust — 28 engines behind one API,
plus a flat C ABI with bindings for .NET, Python, Node and Swift.

- **One API, three families**: OS voices (SAPI, AVSpeech,
  speech-dispatcher), offline engines (Sherpa-ONNX, floravox, qwen3,
  OmniVoice, Kyutai pocket), and 22 cloud providers (Azure, Google,
  OpenAI, ElevenLabs, Polly, …).
- **Real word timings** where the engine provides them (Azure/Edge
  WebSockets, Google timepoints, floravox duration tensors, pocket
  attention), honest estimates everywhere else.
- **Streaming audio** through callbacks as it arrives, SpeechMarkdown
  input, SSML, voice cloning (experimental).
- **A stable C ABI** (`include/tts_wrapper.h`) with stable error
  codes, a typed engine-setup schema and a tri-state credential check.

Sibling projects: [js-tts-wrapper](https://github.com/AACTools/js-tts-wrapper) ·
[swift-tts-wrapper](https://github.com/AACTools/swift-tts-wrapper) ·
[dotnet-tts-wrapper](https://github.com/AACTools/dotnet-tts-wrapper)

## Quick start

```rust
use rust_tts_wrapper::{create_engine, TtsEngine};

let engine = create_engine("openai", r#"{"apiKey":"sk-…"}"#)?;

let mut boundary = |word: &str, start: f32, end: f32, _: i32, _: i32, est: bool| {
    println!("{word} @ {start:.2}s–{end:.2}s{}", if est { " (est.)" } else { "" });
};
engine.speak("Hello world", Some("alloy"), 1.0, 1.0, 1.0,
    Some(&mut |_chunk: &[u8]| {}), Some(&mut boundary), None)?;
```

```bash
cargo build --no-default-features --features system,cloud   # Linux
cargo build --no-default-features --features sapi,cloud     # Windows
cargo build --no-default-features --features avsynth,cloud  # macOS
```

## Documentation

| Doc | Contents |
|---|---|
| [Rust usage](docs/rust.md) | creating engines, callbacks, voices, errors, setup schema, timeline, feature flags |
| [.NET usage](docs/dotnet.md) | NuGet package, `TtsClient`, the `AbstractTtsClient` adapter, migration guide |
| [Python usage](docs/python.md) | ctypes client, loading, examples |
| [Node usage](docs/node.md) | npm package, events API |
| [Swift usage](docs/swift.md) | SwiftPM package |
| [C ABI reference](docs/c-abi.md) | every symbol, ownership rules, error codes |
| [Engines](docs/engines.md) | capability matrix, credential keys, local-engine guides |
| [Voice cloning](docs/voice-cloning.md) | banking, enrollment, consent-gated providers |

Full API docs are generated from source (`#![warn(missing_docs)]`
keeps every public item documented):

```bash
cargo doc --open
```

Runnable examples live in [`examples/`](examples/README.md) — including
a complete [Avalonia app on the released NuGet package](examples/dotnet/avalonia-nuget).

## Language bindings

| Binding | Package | Status |
|---|---|---|
| .NET | [`RustTtsWrapper.Bindings`](https://www.nuget.org/packages/RustTtsWrapper.Bindings/) (NuGet, native assets included) | CI-tested on Windows + Linux |
| Node | [`@aactools/tts-wrapper`](https://www.npmjs.com/package/@aactools/tts-wrapper) (npm) | CI-tested on 3 OSes |
| Swift | `RustTtsWrapper` (SwiftPM, in-repo) | CI-tested on macOS |
| Python | [`bindings/python/`](bindings/python/) (single file, vendor it) | ABI conformance-tested |
| C | [`bindings/c/`](bindings/c/) reference harness | compiled `-Wall -Wextra -Werror` |

All five suites run in CI on every push (`.github/workflows/bindings.yml`).

## Feature flags

`system` `sapi` `avsynth` — OS voices · `cloud` — 22 providers ·
`sherpaonnx` `qwen3-local` `omnivoice-local` `floravox` `pocket-timing`
— offline engines · `cloning` — voice banking · `display_names` — ICU
locale names. Details in the [engines doc](docs/engines.md); local
GGML engines need their C++ runtime built first
([`scripts/dev-stubs/`](scripts/dev-stubs/) has dev stubs).

## Development

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cargo test --no-default-features --features system,cloud
cargo doc --no-deps --document-private-items   # docs build is CI-enforced
```

CI runs the full matrix: lint + tests on 3 OSes, every binding suite,
the offline engines against their real C++ runtimes, and release
builds for ~20 targets. Live cloud calls are exercised locally via
`tests/live_cloud.rs.template` + `.env.example`.

## Status & license

Active development; the C ABI surface is additive-only in practice
(see [docs/c-abi.md](docs/c-abi.md)). MIT — see [LICENSE](LICENSE).
