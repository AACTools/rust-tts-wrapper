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

## Playing audio in an Apple app

For cloud and Sherpa-ONNX voices, use `synthToWav` to get a complete
PCM WAV file at the synthesized sample rate. Run the blocking synthesis
call off the main actor. Keep the player alive while audio plays.

```swift
import AVFoundation

let data = try await Task.detached { try client.synthToWav("Hello world") }.value
let player = try AVAudioPlayer(data: data)
player.play()
```

`synthToBytes` keeps the engine's original byte format. Cloud and Sherpa-ONNX
deliver raw PCM through that method.

## Raw SSML, errors, credentials and the setup schema (GH #68/#69/#73)

```swift
// Raw SSML (prosody wrapping bypassed; no-SSML engines get their
// model-matched dialect):
try client.speakSsml("<speak version=\"1.0\" xml:lang=\"en-US\">…</speak>")

// Structured errors: TtsError carries the stable code, the offending
// field key and a display-ready hint (messages secret-redacted).
do {
    _ = try client.synthToBytes("hello")
} catch let e as TtsError {
    e.code            // TtsErrorCode.networkError.rawValue
    e.fieldKey        // "subscriptionKey" for missing-config failures
    e.displayMessage  // safe hint
}
client.getLastErrorCode()    // TtsErrorCode after any failure
client.getLastErrorDetail()  // TtsErrorDetail?

// Tri-state credential check: static catalogs answer .unverified.
switch client.checkCredentials() {
case .verified: break
case .rejected: break
case .unverified: break
case .checkFailed(let detail): break   // e.g. MissingConfiguration + fieldKey
}

// Typed engine setup schema — render settings UIs without a provider
// switch (kinds: secret / filePath / directoryPath / url / choice /
// number / text; either/or groups; capabilities).
let setups = try TtsClient.engineSetups()
let azure = try TtsClient.engineSetup("azure")
```
