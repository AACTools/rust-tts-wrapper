# RustTtsWrapper.Bindings

.NET bindings for [rust-tts-wrapper](https://github.com/AACTools/rust-tts-wrapper).

Two layers are provided:

1. **`TtsClient`** — a standalone, low-level P/Invoke wrapper over the C ABI.
   Use this directly when you want full control and no extra dependencies.
2. **`RustTtsClient`** — a subclass of `DotNetTtsWrapper.Models.AbstractTtsClient`
   that delegates to the Rust backend. Use this when you need drop-in
   compatibility with code written against [DotNetTtsWrapper](https://www.nuget.org/packages/DotNetTtsWrapper/)
   (e.g. [VoiceGarden-SAPI](https://github.com/AACTools/VoiceGarden-SAPI)).

## Install

The native library (`rust_tts_wrapper.dll` / `.so` / `.dylib`) must be on the
loader search path. The simplest pattern is to drop it next to your `.dll`'s
output and set `NativeLibrary` resolver if needed.

The bindings package is `RustTtsWrapper.Bindings`:

```xml
<PackageReference Include="RustTtsWrapper.Bindings" Version="0.1.0" />
```

## Low-level usage (`TtsClient`)

```csharp
using RustTtsWrapper;

using var client = new TtsClient("azure", new() {
    ["subscriptionKey"] = Environment.GetEnvironmentVariable("AZURE_KEY")!,
    ["region"] = "uksouth",
});
client.SetVoice("en-US-AriaNeural");
client.SetRate(1.1f);
byte[] audio = client.SynthToBytes("Hello world");

// Streaming audio + word boundaries
client.SetOnAudio(chunk => Console.Error.WriteLine($"got {chunk.Length} bytes"));
client.SetOnBoundary((word, start, end) =>
    Console.Error.WriteLine($"[{start:F2}-{end:F2}] {word}"));
client.SpeakSync("The quick brown fox.");
```

## Drop-in replacement for DotNetTtsWrapper (`RustTtsClient`)

Anything that already consumes `AbstractTtsClient` can swap the backend with
one line:

```csharp
// Before — DotNetTtsWrapper's pure-C# SherpaOnnx backend
AbstractTtsClient client = new SherpaOnnxTtsClient(credentials);

// After — same AbstractTtsClient surface, Rust backend underneath
AbstractTtsClient client = new RustTtsClient("sherpaonnx", credentials);
```

The adapter implements every abstract member of `AbstractTtsClient`:
`GetVoicesAsync`, `GetVoicesByLanguageAsync`, `SynthToBytesAsync`,
`SynthToStreamAsync`, `SynthToFileAsync`, `SpeakAsync`, `SpeakStreamedAsync`,
`Pause`, `Resume`, `Stop`, `CheckCredentialsAsync`, plus `SetVoice`,
`GetSpeechMarkdownPlatform`, and the capabilities flags.

### Drop-in for VoiceGarden-SAPI

VoiceGarden's `TTSEngine.cs` types its field as `AbstractTtsClient?` and
constructs whichever backend it's configured with. To plug rust-tts-wrapper
into VoiceGarden without touching the rest of the codebase:

1. Add a `PackageReference` to `RustTtsWrapper.Bindings`.
2. Replace the `_ttsClient` construction site:

   ```csharp
   // was
   _ttsClient = new SherpaOnnxTtsClient(creds);
   // becomes
   _ttsClient = new RustTtsClient("sherpaonnx", creds);
   ```

The `SynthToBytesAsync` / `GetVoicesAsync` / `SetVoice` calls in
`TTSEngine.cs` continue to work unchanged because the adapter honours the
same `TtsSynthesisResult.AudioData`, `TtsVoice`, and `TtsOptions` shapes.

## Mapping between DotNetTtsWrapper and Rust APIs

| DotNetTtsWrapper                | rust-tts-wrapper                       |
|---------------------------------|----------------------------------------|
| `SpeechRate.XSlow`..`XFast`     | `f32` rate multiplier `0.5`..`2.0`     |
| `SpeechPitch.XLow`..`XHigh`     | `f32` pitch multiplier `0.5`..`2.0`    |
| `Volume` `int` 0..100           | `f32` `0.0`..`2.0` (1.0 = normal)      |
| `TtsVoice`                      | `tts_voice` struct (P/Invoke)          |
| `AudioFormat.Wav`               | Default; engine may emit MP3/Pcm       |

The adapter performs these conversions in `RateToFloat`, `PitchToFloat`,
`VolumeToFloat`, and `MapVoice`.
