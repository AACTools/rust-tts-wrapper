# floravox integration — status, notes & upstream asks

*Local engineering notes for the floravox engine integration
(`src/floravox_engine.rs`, PR #42). Companion to floravox's own
`docs/handoff-floravox-engine.md`. Last updated 2026-09-24.*

## Status: engine implemented, PR open

**PR #42** (`feat/floravox-engine`, draft). All CI checks green across
Linux/macOS/Windows. Not yet merged/published — waiting on the wasm32
decision (below) and the merge call.

## What the engine uses from floravox

| API | Where used |
|---|---|
| `load_voice(path)` | voice loading (family auto-detect, sibling `.student` pickup) |
| `VoiceBackend::{config, set_speaker, student}` | sample rate, speaker slots, timing tier |
| `Synthesizer::{new, with_document_phonemizer, synthesize_stream, set_speaker}` | synthesis + streaming |
| `MisakiPrePass`, `CharFrontend` | document phonemizers (English / MMS char voices) |
| `SynthesisEvent::{WordBoundary, MarkReached, Finished}` | timing/mark surfacing |
| `WordTiming` | mapped to the wrapper's `WordBoundary` — **`estimated` flag passed through untouched** |
| `floravox_g2p::{MisakiG2p, CachedPhonemizer, LexiconPhonemizer, FstLexicon, MmapLexicon, PhonetisaurusG2p, RuleFallback, ChainedFallback, OovFallback, TokenPhonemizer}` | G2P: misaki default; lexicon+WFST chain via credentials |

Credentials the engine accepts: `modelsDir`, `modelId`, `misaki`
(`us`/`gb`/`off`), `chars`, `speaker`, `lexicon`, `phonetisaurus`,
`lang`, `byt5Encoder` + `byt5Decoder` (opt-in ByT5 neural OOV, default
off).

G2P tiers as shipped:

1. **misaki** (default) — English `us`/`gb`, document pre-pass +
   per-token.
2. **lexicon+Phonetisaurus chain** — `lexicon`/`phonetisaurus`
   credentials (compiled `stem.fst` + `stem.pho`, WFST model).
3. **published bundle** — `floravox-lexicons` cargo feature + `lang`
   credential fetches the voicegarden-lexicons bundle
   (~13 languages, gruut-derived).
4. **ByT5** (opt-in) — `byt5Encoder`/`byt5Decoder` ONNX pair,
   ~130 languages, neural OOV. Chain order: lexicon → Phonetisaurus →
   ByT5 → letter spelling.

MMS character-table voices are auto-detected
(`config().is_char_table`) and use `CharFrontend` regardless.

## What works

- Full SSML: `<break>`, `<prosody rate>`, `<mark>`, `<phoneme>`,
  `<sub>`, `<say-as>` (byte-exact spans). SpeechMarkdown expands to the
  generic dialect via the wrapper's speechmarkdown pipeline.
- Three-tier word timings with the `estimated` flag passed through
  untouched (measured on patched voices; `.student` sidecars engage
  automatically).
- `<mark>` surfaced on the mark callback **and** as a zero-duration
  measured boundary.
- Multi-speaker (`speaker` credential → `set_speaker`).
- Voice discovery (`get_voices`) by scanning `modelsDir`, with a
  raw-id fallback for voices whose config is unreadable.
- 73 offline unit tests (event mapping, voice resolution ladder,
  G2P routing, PCM/volume/rate helpers) + `#[ignore]`-gated live
  conformance suite. All green on CI (3 OS).

## Upstream asks (for when we talk to the floravox crate team)

These are improvements to **floravox-core / floravox-g2p** that would
simplify or unblock the integration. None are blockers — each has a
workaround in place.

### 1. wasm32 / onnxruntime-web bridge — the big one

`ort-sys@2.0.0-rc.13` ships no wasm32 binaries, so the onnx-enabled
wrapper build cannot target `wasm32-unknown-unknown` ("no prebuilt
binaries available for target wasm32-unknown-unknown"). The floravox
web-demo plan already routes around this via onnxruntime-web; the
wrapper needs one of:

- a **`floravox-wasm`** crate (per the plan): `wasm-pack` facade over
  the wrapper, with ort's `disable-linking`/alternative-backend mode
  satisfied by onnxruntime-web exports + JS glue; **or**
- byte-buffer voice constructors on `floravox-core` so a wasm host can
  feed model bytes and drive synthesis itself.

Until then the wrapper documents wasm32 as blocked (see
`src/floravox_engine.rs` module docs). Verified meanwhile:
**floravox-core with `default-features = false` (frontend-only — SSML,
timeline math, estimation) compiles clean for wasm32.**

### 2. Byte-buffer voice constructors

`load_voice(path)` is the only public constructor and it is
path-based (`std::fs`). The wasm surface needs
`load_voice_from_bytes(onnx, config_json, tokens)` (or an equivalent
builder) so browser/embedded hosts can load models from memory. The
wrapper keeps all `std::fs` use confined to `resolve_model` + voice
discovery precisely so this swap is one function.

### 3. Expose `resolve_onnx` (or a path-resolution helper)

The wrapper re-implements the "directory with one non-vocoder `.onnx`
→ that file" probe (`find_onnx`) because `resolve_onnx` is private.
The two implementations can drift (floravox-core's also handles
stems). Either make `resolve_onnx` `pub` or accept a duplicate.

### 4. voicegarden-lexicons: republish against floravox-g2p 0.8.x

`voicegarden-lexicons 0.3.0` on crates.io requires
`floravox-g2p = "0.6.0"`, which pulls a **second, incompatible copy**
of floravox-g2p into any consumer on 0.8.x. The wrapper works around
it by only using paths from the bundle (`bundle.dir`, `entry.lang`)
and opening the lexicon/WFST with its own floravox-g2p 0.8.6. A
republish of voicegarden-lexicons against 0.8.x would let the typed
`LexiconBundle::{lexicon, phonetisaurus}` be used directly.

### 5. Nice-to-haves

- Re-export `floravox_g2p::MisakiG2p` from `floravox-core` (the
  wrapper needs it by name; today it must take a direct
  floravox-g2p dependency for one constructor call).
- `StreamingSynthesis.result`: consider folding the worker outcome
  into the events stream (`SynthesisEvent::Failed { message }`) so
  consumers cannot forget to check it.

## Integration notes (things we learned)

- `VoiceTiming.estimated` is the contract consumers key on; the
  wrapper never re-estimates or upgrades a tier.
- Dropping `StreamingSynthesis` receivers cancels the worker — the
  wrapper's `stop()` relies on this (bounded-channel sends block on a
  live consumer). The `result` channel must still be checked after
  disconnect: a worker panic without a result is surfaced as an error,
  not success (issue #9 semantics).
- `mark` events have no transcript word; consumers should hold the
  last known offset with `byte_len -1` (never a -1 offset).
- Non-English **MMS** voices need no G2P at all (character inventory).
  Non-English **piper** voices need the lexicon bundles — without them
  every word letter-spells. The engine warns once per build.
- `config().is_char_table` auto-detection is what makes MMS voices
  "just work" — keep it.

## Where things live (this repo)

- Engine: `src/floravox_engine.rs` (PR #42)
- Feature flags: `floravox` (base, pulls misaki/uroman via
  floravox-core defaults), `floravox-lexicons` (bundle fetch)
- Tests: inline `#[cfg(test)]` + `tests/floravox_live.rs`
  (`FLORAVOX_TEST_VOICE`, `FLORAVOX_TEST_PATCHED=1`)
- README: "floravox Voices" section (voice families, language
  coverage, timing tiers, credentials)
