# Swift usage

Add the `RustTtsWrapper` product from
[AACTools/rust-tts-wrapper](https://github.com/AACTools/rust-tts-wrapper).
Use a release tag that includes `RustTtsNative.xcframework.zip`.
The Swift package supports macOS 13 and iOS 16 or later. The XCFramework
includes Apple Silicon and Intel slices for macOS and iOS Simulator,
and an arm64 slice for iOS devices. It includes cloud providers and Sherpa-ONNX.

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

## Build from a local checkout

On macOS with Xcode and Rust installed, run these commands from the
repository root.

```bash
scripts/apple/build-xcframework.sh
RUST_TTS_USE_LOCAL_XCFRAMEWORK=1 scripts/apple/smoke-test.sh
```

The script builds `target/apple/RustTtsNative.xcframework`. Add the repository
root as a local Swift package to use it in an app. If the checkout already has
a release pin, set `RUST_TTS_USE_LOCAL_XCFRAMEWORK=1` for the build to select the
local archive.

The standalone `bindings/swift` package can still link against a Cargo build
through `TTS_WRAPPER_LIB_DIR`.

```bash
cargo build --no-default-features --features avsynth,cloud
TTS_WRAPPER_LIB_DIR="$PWD/target/debug" swift test --package-path bindings/swift
```

## Stage a release

1. Build the zip from the source revision intended for the release. Run the
   Apple SwiftPM workflow and download its `apple-swiftpm` artifact, or run
   the local XCFramework build and smoke test.
2. Create a draft GitHub release for the next `vX.Y.Z` tag. Attach the
   `RustTtsNative.xcframework.zip` that passed the smoke test.
3. Run `scripts/apple/pin-release.sh vX.Y.Z PATH_TO_ZIP` and commit the change
   to `Package.swift`. If the native source or ABI changes, build and stage a
   new zip and update the pin before tagging.
4. Push `vX.Y.Z` at the commit with the new pin. The Publish workflow checks
   the staged zip against the pin and publishes the draft with the other
   release assets.

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

## Listing Sherpa-ONNX models

Use `try TtsClient.sherpaModelsJSON()` to read the model registry bundled with
the native build. It returns a JSON object keyed by model ID, with the original
download metadata, languages, and licence fields. The call works offline and
throws `engineNotAvailable` when the native build excludes Sherpa-ONNX.

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
