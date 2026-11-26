# Rust usage

Everything is available from the crate root:

```rust
use rust_tts_wrapper::{create_engine, TtsEngine};
```

Generate and read the full API docs with rustdoc (doc comments,
examples included — [`#![warn(missing_docs)]`][mdl] keeps them
complete):

```bash
cargo doc --open
```

[mdl]: https://doc.rust-lang.org/rustc/lints/lists.html#missing_docs

## Creating an engine

`create_engine(engine_id, credentials_json)` returns a structured
error (GH #69) instead of a bare `None`:

```rust
use rust_tts_wrapper::factory::create_engine;

let engine = create_engine("openai", r#"{"apiKey":"sk-…"}"#)?;

// Unknown / compiled-out engines:
let err = create_engine("definitely-not-an-engine", "").unwrap_err();
assert_eq!(err.code, rust_tts_wrapper::error_codes::TtsErrorCode::EngineNotAvailable);

// Local engines name the missing setup field:
let err = create_engine("omnivoice-local", "{}").unwrap_err();  // (feature omnivoice-local)
assert_eq!(err.code, rust_tts_wrapper::error_codes::TtsErrorCode::MissingConfiguration);
assert_eq!(err.field_key.as_deref(), Some("modelPath"));
```

Cloud engines construct lazily: creation succeeds offline and the
first synthesis exercises the credentials. `engine_list()` returns
exactly the engines compiled into the running build (feature-gated),
with canonical credential keys.

## Speaking

```rust
// Streamed audio + word timings through callbacks:
let mut audio = |_chunk: &[u8]| { /* PCM16 mono, engine's sample rate */ };
let mut boundary = |word: &str, start: f32, end: f32, off: i32, len: i32, est: bool| {
    println!("{word} @ {start:.2}s–{end:.2}s{} (bytes {off}+{len})",
             if est { " (est.)" } else { "" });
};
engine.speak("Hello world", Some("alloy"), 1.0, 1.0, 1.0,
    Some(&mut audio), Some(&mut boundary), None)?;

// Blocking speak, options object, and plain synthesis:
engine.speak_sync("Hello", None, 1.0, 1.0, 1.0, None, None, None)?;
let opts = SpeakOptions { voice: Some("alloy".into()), ..Default::default() };
engine.speak_with_options("Hello", Some(&opts), None, None, None)?;
let bytes: Vec<u8> = engine.synth_to_bytes("Hello", None, 1.0, 1.0, 1.0)?;
let (bytes, boundaries) = engine.synth_with_boundaries("Hello", None, 1.0, 1.0, 1.0)?;
```

- `rate` / `pitch` / `volume` are float multipliers (1.0 = normal).
- Word-boundary callbacks receive `(word, start_s, end_s, byte_offset,
  byte_len, estimated)` — offsets are UTF-8 bytes into the submitted
  text (`-1` when the engine can't locate the word; SAPI reports UTF-16
  units; see `engine.rs` for the full contract).
- `estimated == true` means proportional timings; measured timings come
  from provider data (Azure/Google WS, floravox duration tensors,
  pocket attention).

## Voices, credentials, introspection

```rust
for v in engine.get_voices()? {
    println!("{} [{}] {}", v.name, v.primary_language(), v.gender);
}

// Tri-state credential check (GH #69): static catalogs enumerate with
// any key — that answers Unverified, never Verified.
match engine.check_credentials_status()? {
    CredentialCheck::Verified => {},
    CredentialCheck::Rejected => {},
    CredentialCheck::Unverified => {},
}
```

## Engine setup schema (GH #68)

Render settings UIs from the registry instead of hand-maintained
tables — typed field descriptors (secret / file / directory / URL /
choice / number / text) plus capability flags:

```rust
for setup in rust_tts_wrapper::engine_schema::engine_setup_schemas() {
    for f in &setup.fields {
        println!("{}: {} ({:?}) required={}", setup.id, f.key, f.kind, f.required);
    }
    println!("  internet: {}", setup.capabilities.requires_internet);
}
```

Every schema field key is also listed in the engine descriptor's
`credential_keys_json` (compat for older callers).

## Errors

Failures are classified into stable [`TtsErrorCode`]s (GH #69) at their
origin; opaque engine strings are classified heuristically at the FFI
boundary. Display-safe messages and secret redaction are included:

```rust
use rust_tts_wrapper::error_codes::{redact_message, classify_error_message};

let detail = rust_tts_wrapper::error_codes::ErrorDetail::from_engine_message(
    Some("azure"), "HTTP status client error (401 Unauthorized)",
);
assert_eq!(detail.code, TtsErrorCode::CredentialsRejected);
assert!(detail.display_message().contains("credentials"));
```

[`TtsErrorCode`]: https://docs.rs/rust-tts-wrapper/latest/rust_tts_wrapper/error_codes/enum.TtsErrorCode.html

## Playback timeline (reader-style highlighting)

The `timeline` module turns word boundaries into a
`(playback_time → byte_offset)` map, with a pause/resume/seek clock:

```rust
use rust_tts_wrapper::timeline::PlaybackTimeline;

let (audio, boundaries) = engine.synth_with_boundaries(text, None, 1.0, 1.0, 1.0)?;
let timeline = PlaybackTimeline::from_boundaries(&boundaries);
let offset = timeline.offset_at(2.5); // byte offset at 2.5 s of playback
```

See `examples/timeline-demo.rs` for the full highlighting pattern.

## What a synthesis cost

Every engine is measured the same way, from the outside: when its first
audio arrived, when all of it had, and how much there was. Nothing depends
on an engine reporting anything about itself, so a local model and a cloud
provider can be put side by side.

```rust
let (audio, d) = engine.synth_to_bytes_measured("Hello", None, 1.0, 1.0, 1.0)?;
let (audio, d) = engine.synth_to_bytes_measured_with_options("Hello", Some(&opts))?;

let first = d.first_audio_ms.map_or("never".into(), |ms| format!("{ms} ms"));
println!("{} ms in all, first audio at {first}: {} bytes in {} deliveries",
         d.wall_ms, d.audio_bytes, d.chunks);

// With the audio's format known (synth_to_bytes keeps the engine's own):
let seconds = d.audio_seconds_pcm16(24_000, 1);
let rtf = d.real_time_factor(seconds); // seconds spent per second of audio
```

- `first_audio_ms` is how long a listener waits, and `None` when no audio
  arrived. Well below `wall_ms`, the engine streamed and the audio could be
  played as it was made; close to it, the audio only came at the end,
  however the engine then split it up.
- The default goes through `speak`'s audio callback, the path streaming
  playback uses. SAPI hands that callback nothing (it plays aloud), so SAPI
  measures its silent synthesis path instead: one delivery, at the end.
  Engines that only play through the operating system (`system`, AVSpeech)
  deliver no audio this way, as with their `synth_to_bytes`, and report
  `None`.

## Feature flags

| Feature | Engines / capability |
|---|---|
| `system` | Linux Speech Dispatcher |
| `sapi` | Windows SAPI |
| `avsynth` | macOS AVSpeechSynthesizer |
| `cloud` | 22 cloud providers (HTTP/WS) |
| `sherpaonnx` | Sherpa-ONNX (1300+ offline models) |
| `qwen3-local` | qwen3-tts.cpp GGML (needs `QWEN3_TTS_LIB`) |
| `omnivoice-local` | omnivoice.cpp GGML (needs `OMNIVOICE_LIB`) |
| `floravox` (+`floravox-lexicons`) | piper/MMS/Matcha/Kokoro ONNX voices |
| `pocket-timing` | Kyutai pocket-tts (attention-measured timings) |
| `cloning` | voice banking / enrollment tooling |
| `display_names` | ICU locale display names |

Cross-platform default: `cargo build --no-default-features --features
system,cloud` (Linux) / `sapi,cloud` (Windows) / `avsynth,cloud` (macOS).
