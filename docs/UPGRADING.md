# Upgrading rust-tts-wrapper

Consumer-facing notes for projects embedding the library — notably
VoiceGarden-SAPI (C++ SAPI adapter + .NET `RustTtsWrapper.Bindings`).

## v0.3.17

The C ABI is **unchanged**: `tts_wrapper.h` has the same surface and
signatures, `rust_tts_wrapper.dll` is a drop-in replacement, and the
NuGet package (`RustTtsWrapper.Bindings` 0.3.17) carries the same
P/Invoke layer. What changed is *when* the existing callbacks fire.

### 1. `on_audio` now streams (the big one)

Previously every engine buffered its whole response and delivered audio
only after synthesis completed. Now:

| Path | Delivery |
|---|---|
| Azure / Edge WebSocket | unchanged — real-time per WS message |
| REST engines (OpenAI, Watson, Polly, …) | PCM chunks **as bytes arrive** (MP3 decoded incrementally on a background reader thread) |
| Sherpa-ONNX | **per sentence batch** (each batch re-chunked to 8 KB), so multi-sentence utterances start speaking after the first sentence |
| Google, ElevenLabs `with-timestamps` | still after the response completes — single-JSON-base64 API, not buffering |

What this means for consumer code:

- **Never assume `on_audio` fires once** or that the utterance is
  complete when a callback arrives. First-audio now routinely precedes
  synthesis completion.
- **Per-utterance state must live until `speak`/`speak_sync` returns**,
  not until the first/last `on_audio`.
- Time-to-first-audio improves dramatically for cloud engines on long
  utterances; if you replay/compensate audio timing (the SAPI adapter's
  connection-delay + trailing-silence logic in `OnAudioData`), the math
  is unchanged — it is chunk-based — but chunks now arrive during
  synthesis rather than in one burst.

### 2. Estimated word boundaries fire progressively

For engines without API timing data (everything except Azure/Edge WS,
Google timepoints, ElevenLabs `with-timestamps`), `on_boundary` used to
fire all events in one batch **after** the response completed. Those
estimates now fire **during streaming**, anchored to the audio actually
delivered. Payload signature is unchanged
(`word, start_s, end_s, char_offset, char_len`), and sherpa estimates
still report on the rate-1.0 baseline — callers that compensate for
rate (SAPI adapter, VoiceGarden-SPD) need no changes. Consumers that
collected boundaries and processed them at end-of-speak can keep doing
so (arrival is chronological), but interleaved handling is now
possible and is what makes word highlighting work on long utterances.

### 3. Sample rates: know your engine

`on_audio` delivers PCM16 **mono**; the rate is not signalled and must
be known per engine: Azure, Cartesia, Edge, OpenAI, Google = 24 kHz
(Google is now pinned to 24 kHz server-side — previously it returned
each voice's natural 22.05/24/32 kHz, which broke byte→time math);
ElevenLabs default = 44.1 kHz. Consumers converting bytes to
milliseconds (SAPI's `nWaveBytesPerMSec`) should special-case
ElevenLabs or avoid it where exact timing matters.

### 4. New (non-breaking) Rust API surface

- `SherpaOnnxEngine` is public (module `sherpaonnx_engine`, re-exported
  at the crate root): `available_models()` exposes the 1300-model
  registry, now including per-model `license` / `license_url`.
- Sherpa credentials JSON accepts numeric values (`"numThreads": 2`
  no longer silently discards the whole object).
- New `boundaries` module (`EstimatePlan`, `EstimateFirer`) for
  consumers that want the same progressive-estimate anchoring.

### 5. Packaging / build reproducibility

`sherpa-onnx` is now an **exact pin (=1.13.5)**: crate and sys crate
drift breaks compilation, and a newer crate against older prebuilt
static libs fails at link time (the v0.3.17 i686-windows LNK2019).
Anyone overriding sherpa native libs via `SHERPA_ONNX_LIB_DIR` must
supply the **matching version's** libs. Bump the pin and the download
URL together (see publish.yml's KEEP-IN-SYNC note).

### VoiceGarden-SAPI upgrade checklist

1. `VoiceGarden.UI.csproj`: `RustTtsWrapper.Bindings` 0.3.16 → 0.3.17.
2. MSI payload: replace `rust_tts_wrapper.dll` (x64 + x86) — drop-in,
   same exports.
3. Review `OnAudioData` assumptions: chunk arrival is now progressive;
   the trailing-silence hold/compensation logic is chunk-based and
   stands, but test the Edge engine specifically (incremental MP3
   decode path) and long utterances with rate changes.
4. ElevenLabs voices: silence compensation uses a hardcoded 24 kHz
   byte→ms constant — exclude ElevenLabs from that path or derive the
   rate per engine.
5. No header/binding changes; `tts_set_on_boundary2` /
   `tts_set_on_viseme` remain C-header-only as before.
