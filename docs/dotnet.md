# .NET usage

NuGet package: [`RustTtsWrapper.Bindings`](https://www.nuget.org/packages/RustTtsWrapper.Bindings/)
· Runnable example: [`examples/dotnet/avalonia-nuget`](../examples/dotnet/avalonia-nuget)

Two layers:

1. **`TtsClient`** — a direct P/Invoke wrapper over the C ABI. Use it
   when you want full control and no extra dependencies.
2. **`RustTtsClient`** — a subclass of
   `DotNetTtsWrapper.Models.AbstractTtsClient`, for drop-in
   compatibility with code written against
   [DotNetTtsWrapper](https://www.nuget.org/packages/DotNetTtsWrapper/)
   (e.g. VoiceGarden-SAPI, Dasher-style apps).

## Install

```xml
<PackageReference Include="RustTtsWrapper.Bindings" Version="1.6.6" />
```

The package ships Windows native libraries under
`runtimes/win-x64/native/` (+ `win-x86`), so a standard
`dotnet publish -r win-x64` resolves `rust_tts_wrapper.dll` with **no
manual DLL placement**. The win-x64 build also bundles the offline
engine DLLs (qwen3tts, omnivoice, ggml). On Linux/macOS, build the
native library from source and point `TTS_WRAPPER_LIB` at it.

> Out-of-process hosts loading your assembly need the app directory on
> `PATH` (or `LOAD_WITH_ALTERED_SEARCH_PATH`) for the engine DLLs to
> resolve.

## Low-level client

```csharp
using RustTtsWrapper;

using var client = new TtsClient("openai", new() { ["apiKey"] = "your-key" });
client.SetOnBoundary((word, offset, len, start, end, estimated) =>
    Console.WriteLine($"{word}: {start:F3}-{end:F3} {(estimated ? "estimated" : "measured")}"));
client.SetVoice("alloy");
client.SpeakSync("Hello world");
```

## Drop-in AbstractTtsClient replacement

```csharp
// Before — DotNetTtsWrapper's pure-C# backend
AbstractTtsClient client = new SherpaOnnxTtsClient(credentials);

// After — same surface, Rust backend underneath
AbstractTtsClient client = new RustTtsClient("sherpaonnx", credentials);
```

Credential keys map to the Rust factory's canonical names:

| DotNetTtsWrapper credentials | Rust engine id | Rust credential keys |
|---|---|---|
| `OpenAICredentials` | `openai` | `apiKey` |
| `AzureCredentials` | `azure` | `subscriptionKey`, `region` |
| `GoogleCredentials` | `google` | `apiKey` (or `keyFilePath`) |
| `ElevenLabsCredentials` | `elevenlabs` | `apiKey`, `modelId`, `voiceId`, `language` |
| `PollyCredentials` | `polly` | `accessKeyId`, `secretAccessKey`, `region` |
| `WatsonCredentials` | `watson` | `apiKey`, `region`, `instanceId` |
| `SherpaOnnxCredentials` | `sherpaonnx` | `modelPath`, `modelId`, `numThreads`, … |

(Full table for every engine: `TtsClient.GetEngineSetups()` — below.)

## Beyond the abstract surface

### Typed engine setup schema (GH #68)

Render an Azure key + region combo, a Google credential-file picker or
an offline model-directory picker from one API — no provider switch,
no key-name guessing:

```csharp
foreach (var setup in TtsClient.GetEngineSetups())
{
    // setup.Fields: Key, Label, Kind (Secret / FilePath / DirectoryPath /
    //   Url / Choice / Number / Text), Required, DefaultValue, Choices,
    //   Placeholder, HelpText
    // setup.Capabilities: rate/pitch/volume/timing/SSML/streaming support,
    //   RequiresInternet, StaticVoiceCatalog, Notes
    // setup.RequiredAny: either/or groups (google: apiKey OR keyFilePath)
}
```

`RustTtsClient.EngineSetup` is the selected engine's schema, and the
client's `Capabilities` are populated from it (legacy fallback for
older native builds included).

### Actionable errors (GH #69)

```csharp
try { var voices = await client.GetVoicesAsync(); }
catch (TtsException ex)
{
    // ex.ErrorCode: MissingConfiguration / CredentialsRejected / NetworkError /
    //   ModelNotFound / MissingNativeDependency / EngineNotAvailable / …
    // ex.FieldKey:  "subscriptionKey" — the settings row to highlight
    // ex.DisplayMessage: a safe, display-ready sentence (secrets redacted)
}
```

`TtsClient.GetLastErrorDetail()` returns the same data after any
failed call.

### Credential verification, tri-state (GH #69)

```csharp
var check = await rustClient.CheckCredentialsDetailedAsync();
// check.Status: Verified | Rejected | Unverified | CheckFailed
// CheckFailed carries ErrorCode + FieldKey (e.g. missing subscriptionKey)
```

A static voice catalog (OpenAI-style) lists voices with any key — that
answers `Unverified`, never `Verified`.

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

Contract highlights (see `SpeechRequests.cs` docs for the full
specification):

- `Queue` serializes requests in order; `Replace` supersedes anything
  outstanding (settings-preview semantics).
- `Stop()` stops the active request (→ `Stopped`) and cancels
  everything queued (→ `Cancelled`).
- A cancelled or superseded request's late boundary events are dropped
  — they can never be attributed to a newer request.
- In-flight native enumeration cannot be aborted mid-call; a cancelled
  token suppresses delivery of its results instead.

## API mapping

| DotNetTtsWrapper | rust-tts-wrapper |
|---|---|
| `SpeechRate.XSlow`..`XFast` | `f32` rate multiplier `0.5`..`2.0` |
| `SpeechPitch.XLow`..`XHigh` | `f32` pitch multiplier `0.5`..`2.0` |
| `Volume` `int` 0..100 | `f32` `0.0`..`2.0` (1.0 = normal) |
| `TtsVoice` | `tts_voice` struct (P/Invoke) |
| `AudioFormat.Wav` | default; engine may emit MP3/PCM |

The conversions live in `RustTtsClient` (`RateToFloat`,
`PitchToFloat`, `VolumeToFloat`, `MapVoice`).

## Tests

```bash
cargo build --release --no-default-features --features system,cloud  # or sapi,cloud
cd bindings/dotnet
TTS_WRAPPER_LIB=$PWD/../../target/release/librust_tts_wrapper.so \
  dotnet test tests/RustTtsWrapper.Bindings.Tests.csproj -p:SkipNativeLibCheck=true
```
