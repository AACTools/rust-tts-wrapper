# RustTtsWrapper.Bindings

.NET bindings for [rust-tts-wrapper](https://github.com/AACTools/rust-tts-wrapper).

**Full usage guide: [docs/dotnet.md](../../docs/dotnet.md)** — the
`AbstractTtsClient` migration, the typed engine setup schema,
structured errors, tri-state credential checks, cancellable voice
discovery and request-scoped speech. A runnable Avalonia app on the
released package lives at
[`examples/dotnet/avalonia-nuget`](../../examples/dotnet/avalonia-nuget).

Two layers are provided:

1. **`TtsClient`** — a standalone, low-level P/Invoke wrapper over the C ABI.
2. **`RustTtsClient`** — a subclass of `DotNetTtsWrapper.Models.AbstractTtsClient`
   that delegates to the Rust backend (drop-in for
   [DotNetTtsWrapper](https://www.nuget.org/packages/DotNetTtsWrapper/)
   consumers such as VoiceGarden-SAPI).

## Install

```xml
<PackageReference Include="RustTtsWrapper.Bindings" Version="1.6.6" />
```

The package ships the Windows native libraries under
`runtimes/win-x64/native/` (+ `win-x86`) including the offline engine
DLLs — a standard `dotnet publish -r win-x64` resolves everything with
no manual DLL placement. On Linux/macOS, build the native library from
source and set `TTS_WRAPPER_LIB`.

## Minimal example

```csharp
using RustTtsWrapper;

using var client = new TtsClient("openai", new() { ["apiKey"] = "your-key" });
client.SetOnBoundary((word, offset, len, start, end, estimated) =>
    Console.WriteLine($"{word}: {start:F3}-{end:F3} {(estimated ? "estimated" : "measured")}"));
client.SetVoice("alloy");
client.SpeakSync("Hello world");
```

```csharp
// Drop-in AbstractTtsClient swap:
AbstractTtsClient client = new RustTtsClient("sherpaonnx", credentials);
```

## Tests

```bash
cargo build --release --no-default-features --features system,cloud   # or sapi,cloud
TTS_WRAPPER_LIB=$PWD/../../target/release/librust_tts_wrapper.so \
  dotnet test tests/RustTtsWrapper.Bindings.Tests.csproj -p:SkipNativeLibCheck=true
```
