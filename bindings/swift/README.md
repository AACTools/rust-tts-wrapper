# Swift bindings for rust-tts-wrapper

A high-level Swift wrapper around the C ABI. The native `rust_tts_wrapper`
module must be imported into the same Xcode / SwiftPM target — either via
the staticlib produced by `cargo build` or via the precompiled artifact from
the publish workflow.

## Usage

```swift
import rust_tts_wrapper

do {
    let client = try TtsClient(engineId: "azure", credentials: [
        "subscriptionKey": ProcessInfo.processInfo.environment["AZURE_KEY"]!,
        "region": "uksouth",
    ])
    try client.setVoice("en-US-AriaNeural")
    try client.setRate(1.1)
    let audio = try client.synthToBytes("Hello world")

    try client.setOnAudio { chunk in
        NSLog("got \(chunk.count) bytes")
    }
    try client.setOnBoundary { word, start, end in
        NSLog("[%.2f-%.2f] %@", start, end, word)
    }
    try client.speakSync("The quick brown fox.")
} catch let error as TtsError {
    NSLog("TTS error: \(error)")
}
```

## API surface

- `TtsClient(engineId:credentials:)` — engine constructor; throws `TtsError`
  on failure with the message from `tts_get_last_error`.
- `speak`, `speakSync`, `synthToBytes` — synthesis entry points.
- `stop`, `pause`, `resume` — playback control.
- `setVoice`, `setRate`, `setPitch`, `setVolume` — per-instance settings.
- `setOnAudio`, `setOnBoundary` — typed closures. Pass `nil` to clear. The
  closures are kept alive for as long as the `TtsClient` lives.
- `getVoices()` returns `[TtsVoice]`; `listEngines()` returns
  `[TtsEngineInfo]` (static method).
- `getLastError()` / `getGlobalLastError()` for diagnostics.

## Drop-in compatibility

The Swift wrapper mirrors the API surface of the .NET / Python bindings so
callers can swap backends across the three ecosystems with minimal code
churn. There is no canonical Swift `AbstractTTSClient` package to subclass,
so unlike .NET we don't define an adapter — the `TtsClient` class is the
single entry point.

## Header import

`TtsClient.swift` expects `import rust_tts_wrapper` to bring in the C
header that cbindgen generates at build time. If the module isn't present
(for example, when copying just this file into a project that doesn't yet
link the staticlib), a fallback `@_cdecl` shim block is compiled in so the
file at least type-checks — every function returns a failure value.
