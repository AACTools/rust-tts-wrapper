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

```xml
<PackageReference Include="RustTtsWrapper.Bindings" Version="1.6.6" />
```

The package ships the Windows native libraries under
`runtimes/win-x64/native/` (and `win-x86`), so a standard `dotnet
publish -r win-x64` resolves `rust_tts_wrapper.dll` — **no manual DLL
placement**. The win-x64 build also bundles the offline engine DLLs
(qwen3tts, omnivoice, ggml) beside it. On Linux/macOS, either build
the native library from source and point `TTS_WRAPPER_LIB` at it, or
set `NativeLibrary` resolution yourself:

```bash
cargo build --release --no-default-features --features system,cloud
TTS_WRAPPER_LIB=target/release/librust_tts_wrapper.so dotnet run
```

> Out-of-process hosts loading your assembly need the app directory on
> `PATH` (or `LOAD_WITH_ALTERED_SEARCH_PATH`) for the engine DLLs to
> resolve — see the root README's .NET section.

A runnable Avalonia app built on the released package lives at
[`examples/dotnet/avalonia-nuget`](../../examples/dotnet/avalonia-nuget),
including a full `AbstractTtsClient` migration guide with the
credential-key mapping table.

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

## Beyond the abstract surface

The adapter exposes the native extras the `AbstractTtsClient` contract
cannot express (GH #68/#69/#70):

### Typed engine setup schema (GH #68)

```csharp
foreach (var setup in TtsClient.GetEngineSetups())
{
    // setup.Fields: Key, Label, Kind (Secret / FilePath / DirectoryPath /
    //   Url / Choice / Number / Text), Required, DefaultValue, Choices
    // setup.Capabilities: rate/pitch/volume/timing/SSML/streaming support,
    //   RequiresInternet, StaticVoiceCatalog, Notes
}
```

Render an Azure key + region combo, a Google credential-file picker, or
an offline model-directory picker from one API — no provider switch, no
key-name guessing. `RustTtsClient.EngineSetup` is the selected engine's
schema, and the client's `Capabilities` are populated from it (the
legacy hard-coded table remains as a fallback for older native builds).

### Actionable errors (GH #69)

```csharp
try { var voices = client.GetVoicesAsync(); }
catch (TtsException ex)
{
    // ex.ErrorCode: MissingConfiguration / CredentialsRejected / NetworkError /
    //   ModelNotFound / MissingNativeDependency / EngineNotAvailable / …
    // ex.FieldKey:  "subscriptionKey" — the settings row to highlight
    // ex.DisplayMessage: a safe, display-ready sentence (secrets redacted)
}
```

`TtsClient.GetLastErrorDetail()` returns the same data as
`TtsErrorDetail` after any failed call.

### Credential verification, tri-state (GH #69)

```csharp
var check = await rustClient.CheckCredentialsDetailedAsync();
// check.Status: Verified | Rejected | Unverified | CheckFailed
```

A static voice catalog (OpenAI-style) lists voices with any key — that
answers `Unverified`, never `Verified`. `CheckFailed` carries the
stable `ErrorCode` + `FieldKey` (e.g. missing `subscriptionKey`).

### Cancellable discovery + request-scoped speech (GH #70)

```csharp
// Voice discovery that cannot overwrite a newer engine's results:
var voices = await rustClient.GetVoicesAsync(cancellationToken);

// Speech with a request id in every event, queue or replace semantics:
var result = await rustClient.SpeakRequestAsync(
    "Preview sentence",
    timing: e => Log($"{e.RequestId[..8]} {e.Word} @ {e.StartTime:F2}s"),
    request: new SpeechRequestOptions { Dispatch = SpeechRequestDispatch.Replace });
// result.Status: Completed | Stopped | Superseded | Cancelled | Failed
```

`Stop()` stops the active request (→ `Stopped`) and cancels everything
queued (→ `Cancelled`). A cancelled or superseded request's late
boundary events are dropped — they can never be attributed to a newer
request. In-flight native enumeration cannot be aborted mid-call
(documented backend limitation); a cancelled token suppresses delivery
of its results instead.

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
