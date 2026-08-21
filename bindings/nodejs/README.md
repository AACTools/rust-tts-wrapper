# @aactools/tts-wrapper

Node.js bindings for the rust-tts-wrapper C ABI. Loads the shared library
at runtime via [koffi](https://koffi.dev) — no node-gyp, no native
compilation of this package.

## Install & setup

Build (or download) the Rust library once:

```sh
cargo build --release
```

produces `target/release/librust_tts_wrapper.so` (Linux),
`librust_tts_wrapper.dylib` (macOS) or `rust_tts_wrapper.dll` (Windows).

The client finds it via `TTS_WRAPPER_LIB=/path/to/lib`, a
`runtimes/<platform>-<arch>/` directory next to the package, or the
repo's `target/{release,debug}`. See `resolveLibraryPath()` in
`src/index.js`.

## Usage

```js
const { TtsClient } = require("@aactools/tts-wrapper");

const client = new TtsClient({ engineId: "openai", credentials: { apiKey: "..." } });
client.on("audio", (chunk) => stream.write(chunk));
client.on("boundary", ({ word, startSec, endSec, estimated }) => {
  console.log(`${word}: ${startSec}-${endSec} ${estimated ? "estimated" : "measured"}`);
});
client.setVoice("alloy");
client.speak("Hello world");
client.close();
```

Events: `audio` (Buffer), `boundary`, `mark`, `viseme`, `start`, `end`,
`error` (string). See `src/index.d.ts` for the full API.

## Tests

```sh
npm install
npm test
```
