# Swift usage

SwiftPM package: `RustTtsWrapper` (in [`bindings/swift/`](../bindings/swift/)) —
wraps the vendored C header with a `TtsClient` class.

```swift
import RustTtsWrapper

let client = try TtsClient(engineId: "openai", credentials: ["apiKey": "your-key"])

client.setOnBoundary { word, offset, len, start, end, estimated in
    print("\(word): \(start)-\(end) \(estimated ? "estimated" : "measured")")
}

client.setVoice("alloy")
client.setRate(1.1)
try client.speakSync("Hello world")
client.stop()
```

## Loading the native library

The package links `rust_tts_wrapper` through the vendored header
(`bindings/swift/Sources/CRustTtsWrapper/include/tts_wrapper.h`, kept
in sync with `include/tts_wrapper.h` by CI). Build the library and
point the test suite / app at it:

```bash
cargo build --release --no-default-features --features avsynth,cloud   # macOS
cd bindings/swift
TTS_WRAPPER_LIB_DIR=../../target/release swift test
```

For app distribution, bundle the dylib (or link the staticlib build)
and disable the embedded-bitcode stripping exceptions per Apple's
guidelines for Rust libraries.

## Engine selection & credentials

```swift
let engines = TtsClient.listEngines()      // ids compiled into this build
let voices  = try client.getVoices()

let azure = try TtsClient(engineId: "azure",
                          credentials: ["subscriptionKey": "…", "region": "eastus"])
```

Credentials are `[String: String]` dictionaries serialised to the JSON
the Rust factory parses (canonical keys — see the
[engines doc](engines.md)).
