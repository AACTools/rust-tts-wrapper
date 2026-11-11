# Avalonia + NuGet demo (`RustTtsWrapper.Bindings`)

A minimal [Avalonia](https://avaloniaui.net/) app that consumes the
**released `RustTtsWrapper.Bindings` NuGet package** — a tested starting
point for .NET desktop apps (Dasher-style) migrating from
`DotNetTtsWrapper.AbstractTtsClient` to the Rust back end.

It demonstrates the four operations a speech settings screen needs:

- **engine selection** — `TtsClient.ListEngines()` (only the engines
  compiled into the installed native build appear)
- **voice discovery** — `GetVoicesAsync` (cancellable when built with
  local bindings — GH #70)
- **preview** — request-scoped speech with word-timing events (GH #70)
- **stop** — cancels the active request and everything queued

## Run it

```bash
# Windows (default: NuGet package — native assets resolve automatically)
dotnet run -c Release

# Headless smoke (no window): enumeration + preview probe
dotnet run -c Release -- --headless --engine sapi

# Against local in-repo bindings + a locally built native library
# (unlocks the typed setup schema, structured errors, cancellable
# discovery and request-scoped speech demos — GH #68/#69/#70)
cargo build --release --no-default-features --features system,cloud   # or sapi,cloud on Windows
dotnet run -p:UseLocalBindings=true
# Linux/macOS: point the loader at the local library
TTS_WRAPPER_LIB=../../target/release/librust_tts_wrapper.so dotnet run -p:UseLocalBindings=true
```

## Clean `win-x64` publish (no manual DLL copy)

The NuGet package ships the native library under
`runtimes/win-x64/native/` (plus the offline engine DLLs — qwen3tts,
omnivoice, ggml), so a plain publish resolves everything:

```powershell
dotnet publish -c Release -r win-x64 --self-contained false
```

Verify in the output folder:

- `runtimes/win-x64/native/rust_tts_wrapper.dll` is present and loads
  with **no manual DLL placement** (MSBuild's RID-agnostic resolution
  copies it beside the app);
- `dotnet AvaloniaNuGetDemo.dll -- --headless --engine sapi` prints the
  engine list, creates a SAPI client, and enumerates installed voices.

### Expected behaviour with no voice installed

SAPI with zero installed voices is a *normal state*, not an error: the
app creates the client, `GetVoicesAsync` returns an empty list, the UI
shows the empty list with a "no voices installed" hint, and preview is
disabled until a voice is added via Windows Settings → Time & Language →
Speech. The `--headless` run makes the same claim explicit (exit code 0).

> Out-of-process hosts loading the app DLL (e.g. VSTO-style hosts) must
> have the app directory on `PATH` or load with
> `LOAD_WITH_ALTERED_SEARCH_PATH` — see the root README's .NET section.

## Migrating from `AbstractTtsClient`

Swap the backend; keep the rest of your code:

```csharp
// Before (DotNetTtsWrapper backend)
AbstractTtsClient client = new SherpaOnnxTtsClient(credentials);

// After — same surface, Rust back end underneath
AbstractTtsClient client = new RustTtsClient("sherpaonnx", credentials);
```

### Credential key mapping

`RustTtsClient` takes a plain `Dictionary<string, string>` serialised to
the JSON the Rust factory parses. Map your `ITtsCredentials` objects to
the **canonical Rust keys**:

| DotNetTtsWrapper credentials | Rust engine id | Rust credential keys |
| --- | --- | --- |
| `OpenAICredentials` | `openai` | `apiKey` |
| `AzureCredentials` | `azure` | `subscriptionKey`, `region` |
| `GoogleCredentials` | `google` | `apiKey` (or `keyFilePath` — a file whose first line is the key) |
| `ElevenLabsCredentials` | `elevenlabs` | `apiKey`, `modelId`, `voiceId`, `language` |
| `PollyCredentials` | `polly` | `accessKeyId`, `secretAccessKey`, `region` |
| `WatsonCredentials` | `watson` | `apiKey`, `region`, `instanceId` |
| `SherpaOnnxCredentials` | `sherpaonnx` | `modelPath`, `modelId`, `numThreads`, `provider`, … |
| — (local GGUF engines) | `omnivoice-local` | `modelPath`, `codecPath` (GGUF files) |
| — | `qwen3-local` | `modelsDir` (directory with the GGUFs) |

Full, typed field descriptors (secret / file picker / folder picker /
choices with defaults) are available from
`TtsClient.GetEngineSetups()` on bindings builds ≥ the one adding
GH #68 — this demo renders them automatically when built with
`-p:UseLocalBindings=true`.

### Native assets by runtime identifier

| RID | Native content |
| --- | --- |
| `win-x64` | `rust_tts_wrapper.dll` + offline engine DLLs (qwen3tts, omnivoice, ggml) |
| `win-x86` | `rust_tts_wrapper.dll` (cloud + SAPI, no offline engines) |
| `linux-x64` / `osx-x64` / `osx-arm64` | not in the package — build the native library from source and set `TTS_WRAPPER_LIB`, or use `-p:UseLocalBindings=true` |

### Error handling

On bindings builds with GH #70/#69 (local mode in this demo), failures
surface as structured data instead of message strings:

- `TtsException.ErrorCode` — stable `TtsErrorCode`
  (`MissingConfiguration`, `CredentialsRejected`, `NetworkError`,
  `ModelNotFound`, `EngineNotAvailable`, …)
- `TtsException.FieldKey` — the offending settings row (e.g.
  `subscriptionKey`), for highlighting the exact field in the UI
- `TtsException.DisplayMessage` — a safe, display-ready sentence
  (secrets already redacted)
- `CheckCredentialsDetailedAsync()` — Verified / Rejected / Unverified;
  a static voice catalog (OpenAI-style) enumerates with any key and is
  reported `Unverified`, never `Verified`

## CI verification

The `dotnet-example` job in `bindings.yml` (windows-latest) runs on
every PR: it publishes this app for `win-x64` from the released NuGet
package, asserts the packaged native DLL landed in the publish output
without any manual copy, and runs `--headless --engine sapi` to prove
engine enumeration, client creation and voice discovery work in the
published layout.
