# Engines

28 engines across three families, one API. Engines are feature-gated —
`engine_list()` (and the setup schema) report exactly what the running
build contains.

## Capability matrix

| Engine | Type | Credentials | Streaming | Voice list | Word boundaries | Speech Markdown |
|--------|------|-------------|-----------|------------|-----------------|-----------------|
| System (speech-dispatcher) | Local | None | — (daemon plays) | — | Estimated | — |
| AVSpeech (macOS) | Local | None | — (system plays) | — | Estimated | — |
| SAPI (Windows) | Local | None | — (system plays) | — | Estimated (SAPI events) | — |
| Sherpa-ONNX | Local (1300+ models) | None | Sentence batches | Speakers | Estimated | — |
| Qwen3-TTS local | Local (qwen3-tts.cpp, GGML) | Model dir (+ lib via `QWEN3_TTS_LIB`) | After generation | 10 languages + any reference audio | Estimated | Stripped |
| OmniVoice local | Local (omnivoice.cpp, GGML, 646 languages) | Model + codec GGUFs (+ lib via `OMNIVOICE_LIB`) | After generation | Runtime language table | Estimated | Stripped |
| floravox | Local (piper/MMS/Matcha/Kokoro ONNX — ~1,100 languages) | Model dir | Streamed (per segment) | Filesystem scan | **Measured** (patched) / student / estimated | Native SSML |
| Pocket (Kyutai) | Local (phoneme ONNX bundle + donor wav) | Bundle dir | Frame-by-frame | Reference wav | **Measured** (attention tap) | Native SSML + IPA |
| Azure | Cloud | Key + Region | Real-time (WS) / streamed (REST) | API | **Real** (WS) | Platform-aware |
| Microsoft Edge (Read Aloud) | Cloud | **None** (free) | Real-time (WS) | API | **Real** (WS) | Platform-aware |
| Google Cloud | Cloud | API key | After response | API | **Real** (v1beta1 timepoints) | Platform-aware |
| Google Gemini (3.8 TTS) | Cloud | API key | After response | API | Estimated (scaled) | Platform-aware |
| Qwen (DashScope) | Cloud | API key (+ model/region) | Real-time (WS) | Static (14 voices) | **Real** (voice-dependent) | Stripped |
| OpenAI | Cloud | API key | Streamed | — (static catalog) | Estimated | Platform-aware |
| ElevenLabs | Cloud | API key (+ model/voice) | Streamed | API | Estimated | Model-matched dialect |
| Cartesia | Cloud | API key | Streamed | API | **Real** (alignment) | Platform-aware |
| Deepgram / PlayHT / Fish / Hume / Mistral / Murf / Resemble / Unreal / UpliftAI | Cloud | API key (+userId for PlayHT) | Streamed | varies | Estimated | Platform-aware |
| Amazon Polly | Cloud | Key + secret + region | Streamed | API | Estimated | Platform-aware |
| IBM Watson | Cloud | Key + region + instance | Chunked | — | Estimated | Platform-aware |
| Wit.ai / xAI / ModelsLab | Cloud | token / API key | Chunked | — | Estimated | Platform-aware |

**Streaming** means audio arrives through the `on_audio` callback as it
becomes available (REST bodies stream as bytes arrive; Azure/Edge/Qwen
deliver over WebSockets). Google and ElevenLabs `with-timestamps`
return one JSON document — an API limitation, not buffering.

**Word boundaries**: `estimated` fires progressively during streaming,
anchored to delivered audio; measured timings come from provider data
or model internals. Offsets are UTF-8 bytes into the submitted text
(SAPI: UTF-16 units; Azure: provider units).

**Speech Markdown** input is auto-detected and converted to the
engine's dialect via
[speechmarkdown-rust](https://github.com/AACTools/speechmarkdown-rust).

## Credential keys

Canonical keys per engine (also available at runtime via
`tts_get_engine_setups_json` with typed kinds, labels and defaults):

| Engine | Keys |
|---|---|
| openai / cartesia / deepgram / fishaudio / hume / mistral / murf / resemble / unrealspeech / upliftai / xai / modelslab | `apiKey` |
| elevenlabs | `apiKey`, `modelId`, `voiceId`, `language` |
| azure | `subscriptionKey`, `region` |
| edge | — |
| google | `apiKey` or `keyFilePath` (either) |
| gemini | `apiKey`, `modelId`, `voice`, `style` |
| qwen | `apiKey`, `modelId`, `region`, `instruction`, `wsUrl`, `customizationUrl` |
| polly | `accessKeyId`, `secretAccessKey`, `region` |
| playht | `apiKey`, `userId` |
| watson | `apiKey`, `region`, `instanceId` |
| witai | `token` |
| sherpaonnx | `modelPath`, `modelId`, `numThreads`, `numSteps`, `provider`, `referenceAudio`, `referenceText` |
| qwen3-local | `modelsDir` (required), `threads`, `temperature`, `topK`, `maxTokens` |
| omnivoice-local | `modelPath`, `codecPath` (required GGUFs), `numSteps`, `seed`, `instruct`, `language` |
| floravox | `modelsDir`, `modelId`, `misaki`, `chars`, `speaker` |
| pocket | `bundleDir`, `reference` |

## Local engine guides

### qwen3-local

[qwen3-tts.cpp](https://github.com/predict-woo/qwen3-tts.cpp) (MIT;
models Apache-2.0), the only local engine with **zero-shot voice
cloning**. The C++ library is not vendored:

```bash
scripts/build-qwen3-local.sh                 # clones + builds GGML into ~/spikes
cd ~/spikes/qwen3-tts.cpp && python scripts/setup_pipeline_models.py
QWEN3_TTS_LIB=~/spikes/qwen3-tts.cpp cargo build --features qwen3-local
```

Pass a reference WAV path as the voice for zero-shot cloning, a
language id (`en`, `fr`, … — 10 locales), `"default"`, or an
`emb:<base64>` speaker-embedding handle from the local cloner.
PCM16 mono 24 kHz; CPU synthesis is batch-grade (~0.1× realtime on 6
cores — use Metal/CUDA builds for speed); calls are serialized
(upstream is not concurrent-safe). A dev-stub for offline development
lives in [`scripts/dev-stubs/`](../scripts/dev-stubs/).

### omnivoice-local

[omnivoice.cpp](https://github.com/ServeurpersoCom/omnivoice.cpp)
(GGML, GGUF models) — 646 languages, offline, voice-design `instruct`
strings (`"female, child, british accent"`). Build with
`-DOMNIVOICE_SHARED=ON` and point `OMNIVOICE_LIB` at the repo.

The advertised voice list is enumerated from the **runtime's own
language table** (GH #64) — advertised == accepted by construction. A
language the runtime rejects (e.g. a name outside its table) falls
back to auto-detection with a warning instead of failing the
utterance. `en-GB`-style input maps to base `en` with the region
expressed as an accent instruct; `ar` maps to `arb` (standard Arabic).

### floravox

[floravox](https://github.com/AACTools/floravox) (Apache-2.0 OR MIT,
pure Rust) — piper VITS / MMS / Matcha / Kokoro ONNX voices:

| Family | Files on disk | Languages |
|---|---|---|
| piper VITS | `X.onnx` + `X.onnx.json` | ~30 per voice |
| MMS VITS | `X.onnx` + `tokens.txt` | ~1,100 (one per voice) |
| Matcha | acoustic + vocoder | per voice |
| Kokoro | `model.onnx` + `voices.bin` | en + zh (multi-speaker) |

Word-timing tiers, honestly flagged: **measured** (duration-patched
voices), **student** (sidecar model, ~340 languages), **proportional**
(150-wpm). Native SSML: `<break>`/`<prosody>`/`<mark>`/`<phoneme>`/`<sub>`.
Non-English phoneme voices need `--features floravox-lexicons` (lexicon
+ Phonetisaurus chain; `lang` credential auto-fetches bundles). ByT5
neural G2P is opt-in via `byt5Encoder`/`byt5Decoder`.

### pocket (Kyutai)

`pocket-timing` feature: clone a voice from any donor wav and speak
**IPA phonemes** with attention-measured word boundaries. Two bundle
layouts auto-detect (Sherpa 6L, phoneme 24L —
[`willwade/pocket-tts-en-phonemes-24l`](https://huggingface.co/willwade/pocket-tts-en-phonemes-24l)).

Phoneme input marks word groups with `|`:
`"w ˈɛ ɹ| ɪ z| ð ə| b æ θ ɹ u m"` → "Where is the bathroom".

Honest caveats: zero-shot cloning is timbre-approximate (validate for
intelligibility); a ~600-step fine-tune (~$0.50 GPU) embeds a personal
voice in the weights; English (en-US inventory) for now.

### Sherpa-ONNX models

For iOS 15 and later, build the static library with
`scripts/build-ios-sherpa.sh aarch64-apple-ios` for a device or
`scripts/build-ios-sherpa.sh aarch64-apple-ios-sim` for an Apple Silicon
simulator. Use `x86_64-apple-ios` for an Intel simulator. Install the
corresponding target with `rustup target add` first. The script bundles the
pinned Sherpa-ONNX and ONNX Runtime libraries. Link the resulting
`target/<target>/release/librust_tts_wrapper.a` with C++ and the
`Accelerate`, `CoreFoundation`, `CoreML`, and `Foundation` frameworks.
Set `modelPath` to a directory the app can read. Place each extracted
model in a subdirectory named after its registry ID.

1300+ models from the
[sherpa-onnx-models](https://crates.io/crates/sherpa-onnx-models)
registry crate (canonical:
[AACTools/sherpa-onnx-tts-models](https://github.com/AACTools/sherpa-onnx-tts-models)).
Models load from `~/.rust-tts-wrapper/sherpaonnx/`; refresh the
registry with `cargo update -p sherpa-onnx-models`.

## Cloud engine notes

- **Azure**: SSML XML bodies with prosody; real WS timings; viseme
  events.
- **ElevenLabs**: parses no SSML — SpeechMarkdown renders as model-
  matched dialect (audio tags on v3+ models, `<break>` markup on
  pre-v3; `rate` maps to `voice_settings.speed`).
- **Edge**: unauthenticated Read Aloud endpoint — free, no credentials.
- **Voice cloning** across cloud engines (Google ICV, Azure Personal
  Voice, Qwen, ElevenLabs) is covered in
  [voice-cloning.md](voice-cloning.md).
