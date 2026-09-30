# Voice Cloning Plan (DRAFT — intentionally uncommitted)

Status: investigation / design sketch. Nothing here is agreed or scheduled.
Owner notes: written 2026-09-26 after the Qwen engine merge (PR #45); expanded
same day after cross-provider API research + Apple/import research. All provider
API details below were verified against official docs on 2026-09-26 unless
marked otherwise.

## Why we would put this in

The motivating use case is **voice banking**, not "one more provider feature":

> A voice sample is captured **once**. It is cloned to **every** engine that
> supports cloning. The user then tweaks and listens to outputs from all of
> them side by side, and picks the one that sounds most like them (or best
> suits the context). If a provider disappears or reprices, the banked
> sample re-clones elsewhere.

For AAC users (this repo's home turf) that is the difference between "a
voice" and "**my** voice, everywhere." Secondary benefits:

- **Comparison shopping**: identical text → N engines → N audio takes.
- **Provider resilience**: the identity outlives any single vendor.
- **Local/private path**: sherpa-onnx zero-shot cloning works offline —
  for users who cannot or will not upload a voice to a cloud.

## Two cloning models (the core abstraction seam)

| Model | How it works | Engines |
|---|---|---|
| **Zero-shot** (per-request) | Reference audio rides along with (or is cached for) each synthesis call. No enrollment, no server-side stored voice. | sherpa-onnx zipvoice (needs exact transcript), pocket (audio-only) — **already supported in this repo** via `referenceAudio`/`referenceText` credentials; Fish Audio inline `references` (MessagePack-only — likely skip) |
| **Enrollment** (server-side) | Upload reference → provider stores a voice → synthesis references a handle. Instant (sync response) or job (poll until ready). | ElevenLabs IVC, Cartesia, Fish, PlayHT, Qwen (instant); Resemble, Murf (poll); ElevenLabs/Cartesia PVC, Azure (long-running) |

## Verified API matrix (official docs, 2026-09-26)

| | Auth | Input | Audio constraints | Transcript | Name | Handle | Sync? | Gates |
|---|---|---|---|---|---|---|---|---|
| **ElevenLabs IVC** | `xi-api-key` hdr | multipart `files[]` (multi-file) | 30 s usable, 1–2 min rec, >3 min harmful | ❌ | `name` | `voice_id` | sync | most paid tiers; consent = dashboard checkbox, no API field |
| **Cartesia IVC** | `Bearer` + **`Cartesia-Version` hdr** | multipart `clip` (single) | 10–60 s, ≤16 MB | ❌ | `name` + **`language` (req)** | `id` | sync | Pro plan |
| **Fish Audio** | `Bearer` | multipart `voices[]` 1–20 | ≥10 s rec (2–3 × 15–20 s best) | ✅ optional `texts`, **auto-ASR fallback** | `title` | `_id` (model UUID) | sync (`train_mode:"fast"`) | none beyond usage billing |
| **PlayHT** | `Bearer` + `X-USER-ID` | multipart `sample_file` (single) | 2 s–1 h, 5 kb–50 MB | ❌ | `voice_name` | `id` = **`s3://…manifest.json` URL** | sync | paid plan; **4 req/min** |
| **Murf** | `api-key` hdr | multipart `audio` or JSON `audioUrl` | ≤30 s, ≤40 MB, **≥24 kHz** | ❌ | `tag`/`displayName` | `voiceId` (`cln_…`) **via poll** | poll `requestId` | Enterprise workspace |
| **Resemble** | `Bearer` | JSON `dataset_url` or multipart recordings | 10 s–3 min (clips 1–12 s) | **required per clip** (`text`) | `name` | `item.uuid` | **async**: build + poll/webhook | Business plan |
| **Qwen** (DashScope) | `Bearer` | JSON `url` or base64 data-URI | 10–20 s rec (60 s max), ≤10 MB, ≥16/24 kHz | optional (`text`, qwen3 flavor) | `prefix`/`preferred_name` | `voice_id` / `voice`; **locked to `target_model`** | instant (review status on one flavor) | free / $0.01 per voice |
| **Google Chirp 3 ICV** | `Bearer` + user-project | JSON inline `content` | ≤10 s, single channel | ❌ (fixed **consent script** instead) | — | `voiceCloningKey` — **client-stored**, replayed per request | sync | **allow-list**; 10 keys/min |
| **Azure Personal Voice** | `Ocp-Apim-Subscription-Key` | multipart or blob SAS | 5–90 s prompt + consent ≤10 s | ❌ (fixed per-locale consent script) | path id | `speakerProfileId` → SSML `<mstts:ttsembedding>` | LRO poll (~5 s) | recorded consent + **intake form** |
| ModelsLab | — | *unverified (docs unreachable)* | — | — | — | — | — | re-check before designing |
| Deepgram, Unreal Speech, OpenAI, Gemini, Watson, Wit, xAI, Mistral, Hume, UpliftAI, Edge | — | **no cloning** (confirmed for Deepgram/Unreal/Gemini/OpenAI) | — | — | — | — | — | — |

## What the matrix forces into the unified design

1. **`CloneJob`, not just a handle.** Two engines (Murf, Resemble) — plus
   every PVC/Azure flow — only produce the handle after polling. Unify as:
   `clone_voice() -> CloneJob { Ready(handle) | Pending(job_id, poll fn) }`.
2. **Handles are opaque strings, full stop.** PlayHT's handle is an
   `s3://` manifest URL; Murf's is `cln_…`; Fish's is a model `_id`. Never
   validate, parse, or normalize them — store and replay verbatim.
3. **The unified request is lossy by design** — that's fine:
   `CloneRequest { name, clips: Vec<AudioClip>, transcripts?, language?, tag? }`.
   Engines ignore what they lack fields for. Two hard edges:
   - Resemble *requires* per-clip transcripts → transcripts optional in
     the model, but the capture flow should always collect them
     (zipvoice needs exact ones anyway).
   - Cartesia requires `language`; Qwen wants a first-element language
     hint → `language` should default to the corpus's locale.
4. **Audio constraints diverge too much to auto-slice in v1** (2 s–1 h vs
   ≤30 s; 16–50 MB caps; ≥24 kHz Murf). Capture once at a
   common-denominator spec (mono 16-bit, 24–48 kHz, ~10–20 s clips,
   2–3 clips) and validate per engine at clone time; reject with the
   engine's stated requirement. Auto-trim/resample is a later feature.
5. **Auth styles are already solved** by `CloudConfig`
   (header/prefix/extra headers) — cloning configs can reuse the same
   credential maps.
6. **Rate limits bite specifically on clone endpoints** (PlayHT 4/min) —
   the fan-out driver should serialize clone calls per engine with the
   engine's documented limit, and report per-engine failure without
   aborting the batch.

## Prototype results (2026-09-26, end-to-end PROVEN)

`Personal Voice zip → LJSpeech corpus → Qwen clone → speak via this
engine` — all steps verified live:

1. **Import**: 151 CAFs → 150 wavs (one corrupt ALAC frame; skipped),
   ffmpeg `-ar 24000 -ac 1 -sample_fmt s16`. 12.2 min total.
2. **Enrollment select**: 4 clips (~5 s each) concat + 0.4 s pads →
   22.4 s / 1.0 MB WAV — fits Qwen's 10–20 s recommendation, ≤10 MB.
3. **Qwen `voice-enrollment`** (`POST …/api/v1/services/audio/tts/
   customization`, `action: create_voice`, `target_model:
   qwen-audio-3.0-tts-flash`, `prefix: willwade`): **data-URI audio in
   `url` works** (undocumented — official docs say public URL).
   Instant response, `voice_id =
   qwen-audio-3.0-tts-flash-willwade-{uuid}`; `delete_voice` confirmed
   working too. Free for this flavor.
4. **Synthesis**: the merged `qwen` engine spoke with `voice =
   <voice_id>` — 10.3 s PCM + 24 **measured** word boundaries. Output:
   `GitHub/AACTools/will-cloned-qwen.wav`.
5. Known issue observed: boundary timestamps ran ~1.6 s past actual
   audio length — the sentence clock's byte-delta attribution can
   over-advance when a `sentence-end` arrives after the next sentence's
   audio frames started. Needs a fix before the trait lands (attribute
   bytes to the sentence active when they arrived, not at sentence-end).

Corpus leftover: transcripts still missing (prompt-list mapping or ASR
— not needed for Qwen, needed for zipvoice/Resemble).

## Import sources & Apple Personal Voice

**Apple Personal Voice has a user-initiated on-device export** (confirmed
by hands-on user report, 2026-09-26; not yet reflected in Apple's support
docs): a ZIP containing the prompt **text plus the recorded training
audio**. Apps cannot capture Personal Voice speech (Apple is explicit
about that), but a user can export their own training data — which is
exactly what a cloning pipeline needs. That makes this the **best
Apple-side import source**: real recordings with known transcripts,
produced on-device, user-owned.

Import paths, ranked:

1. **Personal Voice export ZIP (primary Apple path) — layout now
   VERIFIED from a real export** (`Will's Personal Voice 1 -
   Recordings.zip`, iOS 17-era, 2023-09):
   - One folder: `TrainingData/`
   - Files: `{5-char-session-id}_{NN}.caf` — 151 clips across 4 session
     prefixes (paused/resumed recording sessions); `NN` is the **prompt
     index** (1–150 era; iOS 26 banks ~10).
   - Audio: **CAF container, ALAC lossless, 48 kHz, mono, 32-bit**
     (verified via ffprobe). ffmpeg decodes trivially
     (`ffmpeg -i x.caf -ar 24000 -sample_fmt s16 x.wav`); in Rust,
     symphonia can too (`symphonia-format-caf` + ALAC codec features).
   - **No transcripts, no manifest** — the ZIP is audio-only. Transcript
     recovery: the `{NN}` index maps onto the (fixed, community-extracted)
     Personal Voice prompt list per iOS version, or ASR fallback for
     exactness (Fish auto-ASRs regardless; zipvoice wants exact text —
     ASR on clean read-aloud speech is near-perfect).
   - 12.2 min total across 151 clips (~4.9 s each) — more than every
     instant cloner needs (10–60 s); per-clip sizes fit Resemble's 1–12 s
     requirement exactly. Importer should pick best-N clips, not send all.
   - *(The reverse pipeline also exists and already has a tool:
   willwade/Convert2ApplePVoice automates training a Personal Voice from
   another TTS — OCR the on-screen prompt via Vision, speak it through
   BlackHole into Personal Voice's "Continuous Recording" mode. It's
   license-caveated (their README says so) and never touches Personal
   Voice's files, so it yields no format details — but it proves both
   directions of the Apple interop gap and gives us a reusable prompt
   corpus idea: the Personal Voice prompt set itself.)*
2. **ModelTalker import (the clean external source).** Gen3 = 298
   sentences; their recorder writes local WAVs and their privacy
   statement promises copies of the user's own recordings on request.
   Recordings are the user's data — least-encumbered external channel.
3. **Authorized capture (legacy fallback).** A host app can speak a
   corpus through the user's Personal Voice via
   `AVSpeechSynthesizer.write(_:toBufferType:)` and save WAVs — gray
   zone (Apple intends AAC use), user-initiated personal backup only.
   Superseded by the export ZIP for new captures.
4. **Do NOT re-clone licensed synthetic voices.** My-Own-Voice (Acapela),
   SpeakUnique, VocaliD deliver *licensed synthetic voices*, not raw
   recordings (VocaliD site was down; treat as restrictive). Feeding
   their output to third-party cloners exceeds their licenses. The plan
   should say this explicitly in docs.
5. **Corpus format: adopt LJSpeech layout** (`metadata.csv`
   `ID|Transcription` + `wav/` mono 16-bit). It's the de facto
   cross-tool convention, it's what the Piper ecosystem eats, and every
   import source above converts to it trivially. Pair it with a
   public-domain, phonetically-balanced prompt list for fresh captures
   (do not copy ModelTalker's sentence texts — licensing unstated).

## Proposed abstractions

```rust
/// The banked identity. Captured once; fanned out everywhere.
struct VoiceIdentity {
    name: String,                      // "Dad's voice"
    clips: Vec<AudioClip>,             // LJSpeech-derived or fresh capture
    transcripts: Vec<String>,          // always collected (zipvoice/Resemble)
    language: Option<String>,          // default from corpus locale
    consent: Vec<ConsentRecording>,    // per gated provider (see below)
    metadata: HashMap<String, String>,
}

enum CloneJob {
    Ready(CloneHandle),
    Pending { engine: String, job_id: String },  // poll() -> Ready | Failed
}

enum CloneHandle {
    ServerVoiceId(String),   // elevenlabs/qwen/cartesia/fish/playht/murf/resemble
    AzureProfile { id: String, speaker_profile_id: String, base_model: String },
    GoogleCloningKey(String),
    ZeroShot,                // sherpa reads the identity directly
}

enum CloningMode { ZeroShot, Instant, Job }

/// Optional trait, like TtsEngine. Engines opt in; capability flags in
/// EngineDescriptor advertise cloning to hosts.
trait VoiceCloning {
    fn cloning_mode(&self) -> CloningMode;
    fn consent_spec(&self) -> Option<ConsentSpec>;       // script + locale, if gated
    fn clone_voice(&self, identity: &VoiceIdentity) -> TtsResult<CloneJob>;
    fn poll_clone(&self, job: &CloneJob::Pending) -> TtsResult<CloneJob>;
    fn list_cloned(&self) -> TtsResult<Vec<CloneHandle>>;
    fn delete_cloned(&self, handle: &CloneHandle) -> TtsResult<()>;
}
```

Registry: `clones.json` (identity name → engine id → handle + job state),
default `~/.rust-tts-wrapper/clones.json`, overridable by the integrator.

## The fan-out + compare workflow

```
1. capture or import identity   (Personal Voice export ZIP / LJSpeech
                                 corpus / ModelTalker folder)
2. clone_voice(identity)        per VoiceCloning engine — collect per-engine
                                results (CloneJob), tolerate individual
                                failures (plans, allow-lists, quotas)
3. poll Pending jobs            (Murf/Resemble/PVCs) in the background
4. speak(same text)             per engine with its handle
5. listen / tweak               rate, pitch, style per engine; A/B in host
```

`examples/compare-clones.rs` demonstrates 1–4; listening stays host-side.

## Hard truths to design around

1. **"Record consent once" is impossible.** Azure and Google mandate
   different fixed verbal scripts (Azure's includes immutable spoken
   name/company). Identity carries N consent recordings, captured at
   clone time per provider, not at capture time.
2. **Transcripts: collect always, require sometimes.** zipvoice needs an
   exact transcript; Resemble requires one per clip; Fish auto-ASRs them;
   the rest ignore them. Capture flow uses "read this paragraph" prompts
   so transcripts exist by construction.
3. **Legal responsibility stays with the integrator.** We transport
   consent audio and document gates; we do not adjudicate. Explicit docs:
   don't clone licensed synthetic voices; Personal Voice exports are the
   user's own training data for their own banking (that's what Apple
   ships them for).
4. **Voice↔model binding** (Qwen `target_model`, Cartesia PVC model
   snapshots, Murf falcon-2-only) → handles carry model context.
5. **ElevenLabs PVC is a non-goal for v1** (captcha-verification
   multi-step); IVC only. Cartesia PVC likewise (datasets/fine-tunes
   pipeline) — IVC only.

## Phasing (revised after research)

- **Phase 0 — done.** sherpa-onnx zero-shot cloning (zipvoice, pocket).
- **Phase 1 — the spine** (~3–4 days): `VoiceCloning` trait +
  `VoiceIdentity`/`CloneJob`/registry + **instant engines**: Qwen,
  ElevenLabs IVC, Fish, Cartesia, PlayHT. `examples/compare-clones.rs`.
  Rust-only (no FFI).
- **Phase 2 — jobs + corpus** (~3 days): `poll_clone` for Murf +
  Resemble; **Personal Voice export ZIP importer** + LJSpeech corpus
  import + ModelTalker folder import + a public-domain prompt pack for
  fresh captures; per-engine constraint validation with actionable
  errors.
- **Phase 3 — consent-gated**: Google Chirp 3 ICV (allow-list),
  Azure Personal Voice (intake), consent-capture UX. Also re-verify
  ModelsLab then.
- **Later / maybe**: auto-trim/resample, ElevenLabs/Cartesia PVC, FFI
  exposure (`tts_clone_voice` etc.) once hosts ask.

## Open questions

- Registry format/location — default `~/.rust-tts-wrapper/clones.json`
  + override? Versioned schema?
- Does the C ABI ever expose cloning, or Rust-only until asked?
- Auto-trim/resample in-crate (adds a resampler dep) vs validate+reject?
- Where does the prompt corpus live — repo, separate crate, or fetched?
- Multi-clip strategies: which 2–3 clips to send Fish (multi-file) vs the
  single best clip for Cartesia/PlayHT/Murf (single-file) — heuristic?
- Cost guardrails: Qwen charges per voice on the qwen3 flavor; fan-out
  driver should surface per-engine cost before running.
