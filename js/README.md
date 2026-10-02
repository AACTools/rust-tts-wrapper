# rust-tts-wrapper-js

rust-tts-wrapper for JavaScript (wasm32). Cloud TTS engines on web APIs.

## Status
- **elevenlabs**: REST via fetch, SpeechMarkdown pipeline (platform dialect
  per model incl. v3/v4 audio tags), default voice Rachel. Browser-verified
  end-to-end (CORS OK).
- **azure**: REST (region endpoint, SSML body, X-Microsoft-OutputFormat).
  Browser-verified end-to-end.
- **edge: DROPPED from the browser surface** (2026-10-02). The full Read
  Aloud pump is implemented and mock-proven (see src/edge.rs), but
  Microsoft's endpoint closes browser-origin handshakes — it expects the
  Edge extension Origin header, which browsers cannot set. The pump is
  preserved behind the off-by-default `edge` feature for if a proxy ever
  exists. Edge voices on desktop: the native wrapper's Edge engine.
- coming: google, gemini, polly (sigv4 is pure Rust — signs fine in wasm),
  qwen (duplex WS pump — same web-sys pattern as edge)
- out of scope by design: sapi/avsynth (OS synthesizers), sherpaonnx (C++).

## Usage
```js
import init, { cloud_speak } from "./pkg/rust_tts_wrapper_js.js";
await init();
const out = await cloud_speak(JSON.stringify({
  provider: "elevenlabs",
  credentials: { api_key: "..." },
  voice: "21m00Tcm4TlvDq8ikWAM",
}), "Hello [500ms] ++world++");
// out.audio: Uint8Array (mp3), out.mime
```

## Architecture
The native `cloud` feature is reqwest::blocking + tokio (impossible in
wasm). This crate is the async twin: same request shapes, same
speechmarkdown pipeline, `fetch` transport. Follow-up: a `cloud-core`
feature in the parent that shares config/sigv4/alignment parsing natively
instead of the twins. Offline floravox synthesis lives in the floravox
wasm engine (floravox-web/wasm) — unify behind one JS surface next.

## Build
```
cd js && RUSTUP_TOOLCHAIN=stable wasm-pack build --target web --release
```
