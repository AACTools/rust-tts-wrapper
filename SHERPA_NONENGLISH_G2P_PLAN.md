# Non-English Piper G2P from VoiceGarden lexicons — implementation plan

Tracking issue: [#82](https://github.com/AACTools/rust-tts-wrapper/issues/82).
Review with the decision record: [issue comment](https://github.com/AACTools/rust-tts-wrapper/issues/82#issuecomment-6829299350).

One PR per phase, loop-review on each, CI-clean (fmt + clippy -D warnings +
tests across feature combos) — same rules as `SHERPA2_TODO.md`.

## Goal

Make non-English Piper voices speak under sherpa-onnx 2.x, starting with
`piper-de_DE-thorsten-medium`, using the published VoiceGarden pronunciation
bundles (MIT/BSD, no GPL) as the G2P source. Sherpa-ONNX stays the synthesis
engine. Out-of-vocabulary words must be spoken (Phonetisaurus guess), not
silently dropped.

Non-goals (follow-ups, not this effort):

- migrating the **English** lexicon path (#66/#67) to the phonemize path
- languages without a validated bundle (pl, da, hu, …)
- the ByT5 neural G2P tier (too heavy for mobile)
- text normalization beyond German digits (see Phase 3 decision)

## Prior art and the decision

sherpa-onnx 2.0 removed espeak-ng/piper-phonemize (GPL):
[#3731](https://github.com/k2-fsa/sherpa-onnx/issues/3731) →
[PR #4055](https://github.com/k2-fsa/sherpa-onnx/pull/4055). Two front-ends
remain: the built-in lexicon (`word || p1 p2 p3`, what we already generate
for English) and caller-side phonemization passed as
`GenerationConfig::phoneme_codepoints` (one Unicode-codepoint vector per
sentence). Upstream's reference phonemizer is a GPL fork of piper-phonemize —
unusable here. The promised "lexicon.txt inside every model archive" has not
happened for German (the de_DE-thorsten HF repo is unchanged since 2023).

The built-in lexicon front-end after
[#4075](https://github.com/k2-fsa/sherpa-onnx/pull/4075) (in our pinned
2.0.1): punctuation-adjacent words are rescued, NFC + Unicode lowercasing
fixed — but genuinely OOV words are still dropped, and numbers are still
dropped ([#4139](https://github.com/k2-fsa/sherpa-onnx/issues/4139), open).
Upstream plans per-language rule-based C++ G2Ps; nothing for German.

**Decision — two paths, Path B primary:**

| | Path A: generate `lexicon.txt` into the model dir | Path B: wrapper phonemizes → `phoneme_codepoints` |
|---|---|---|
| OOV words | dropped (post-#4075: only punctuation-adjacent rescued) | Phonetisaurus guess, mapped, spoken |
| numbers | dropped | wrapper decides (minimal German digits in scope) |
| runtime writes | yes (model dir must be writable) | none |
| bare-sherpa interop artifact | yes | no |
| extra deps | none beyond the converter | `voicegarden-lexicons` + `floravox-g2p` (FST only) |

Path B subsumes Path A. Path A stays as the fallback when the `sherpa-g2p`
feature is off and as the interop artifact (a converted `lexicon.txt` beside
the model helps every other sherpa client too). Both paths share one
model-aware phone converter (Phase 2).

The bundle fetch/cache is **not** reimplemented: the `voicegarden-lexicons`
crate (already an optional dep) does manifest sha256 verification and caches
under `~/.voicegarden/lexicons`; `src/floravox_engine.rs`
`fetch_lexicon_bundle` is the in-repo pattern. v0.3.1 fixed the manifest
sha mismatches, so checksums verify against the uploaded bytes.

## Architecture

```
                    ┌─ Phase 2: model-aware converter (shared) ─┐
                    │  bundle phones --(tokens.txt-driven)-->   │
                    │  model token sequence (single codepoints) │
                    └────────────────────┬──────────────────────┘
                                         │
  Path A (fallback/interop)              │         Path B (primary)
  convert bundle lexicon.txt             │         speak() front-end
  → model_dir/lexicon.txt                │         ┌───────────────────────────┐
  (sherpa's lexicon front-end)           │         │ sentence-split            │
                                        │         │ word lookup (bundle .fst) │
                                        │         │ OOV → Phonetisaurus WFST  │
                                        │         │ map phones (shared conv.) │
                                        │         │ validate single-codepoint │
                                        │         │ gen_config.phoneme_codepoints
                                        └────────►│ generate_with_config      │
                                                  └───────────────────────────┘
```

Insertion point: `SherpaOnnxEngine::synthesize`
(`src/sherpaonnx_engine.rs`), just before the two
`tts.generate_with_config(text, &gen_config, …)` calls (streaming and
file paths). When the front-end is active for the loaded model, it produces
the codepoints and the `text` argument becomes `""`. Word-boundary estimates
(`EstimatePlan::build(text)`) keep using the ORIGINAL text — they are WPM
estimates over words and do not depend on the phoneme stream.

## Phases

### Phase 0 — codepoint-contract spike (gates Phase 3)

Before building the front-end, pin down the exact contract of
`phoneme_codepoints` on a real model, because getting it wrong produces
garbage audio that still "works":

- Probe on macOS against `piper-de_DE-thorsten-medium`:
  1. generate audio via the converted-lexicon path (Path A) — the reference;
  2. generate audio via hand-built `phoneme_codepoints` for the same
     sentence, varying: leading/trailing pause tokens, punctuation codepoints
     (`,.!?` are in tokens.txt), inter-word space token, BOS/EOS markers
     (`^`/`$` if present in tokens.txt);
  3. A/B compare waveforms/spectrograms and by ear.
- Deliverable: a written contract (which symbols, which order, sentence
  boundaries) + a tiny example binary under `examples/` kept as the probe.
- If `phoneme_codepoints` cannot reproduce the lexicon-path audio on this
  model, fall back to making Path A primary for German and file upstream.

### Phase 1 — language resolution + validated-language allowlist

New `src/sherpa_lang.rs`:

- Resolve a model's language from registry metadata
  (`SherpaModelInfo::language[].lang_code`, e.g. `de_DE`), NOT id parsing.
  Normalize to BCP-47; bundle keys resolve via the crate's exact/prefix
  matching (`de-DE` → `de`). Keep excluding bilingual `<lang>_en`
  compounds (`is_bilingual_en_compound`).
- A compile-time allowlist `VALIDATED_LANG_BUNDLES: &[(&str /*bcp47*/, &str
  /*bundle key*/)]` — the single gate for everything downstream. Starts as
  `[("de", "deu" /*expanded*/, fallback "de")]`. Every later phase keys off
  it; "enable download for validated languages" (issue bullet 4) becomes
  "add a row here after Phase 4 passes".
- Unit tests: registry language → bundle key; compounds rejected; unknown
  language → None.

### Phase 2 — generalized model-aware converter + Path A for German

Generalize the English-only machinery in `src/sherpa_lexicon.rs` /
`src/sherpa_convention.rs` into a vocabulary-driven converter
(`PhonemeMap::for_language`-shaped). Per bundle phone symbol, in order:

1. exact match in the model's `tokens.txt` → identity;
2. strip tie bars (U+0361/U+035C) → if the result matches, use it; else
   split the components (`p͡f` → `p` `f`) when all are in vocab;
3. split modifier letters (length `ː` U+02D0, `ˑ`) off the base when the
   composite is not a token (`uː` → `u` `ː`) and both parts are in vocab;
4. ASCII→IPA homoglyphs (`g` → `ɡ` U+0261; NFC does NOT do this) then retry 1;
5. stress marks (`ˈ` `ˌ`) deleted when absent from the vocab (German) /
   kept when present (English, unchanged);
6. else unmappable → Path A: skip entry (existing behaviour); Path B: word
   is dropped with a logged warning + counter.

The German probe validated exactly these rules (`uː`, `p͡f`, `g`) with all
277,912 entries converting — the test asserts that coverage against the
bundle file when present (`#[ignore]`-gated), and against a synthetic
thorsten-like tokens.txt in offline unit tests.

Path A wiring (fallback + interop):

- Extend `discover_lexicon_assets`/`GRUUT_CACHE_LAYOUTS` to per-language
  layouts (`<key>/lexicon.txt` under the crate's cache root), replacing the
  eng-US/en hardcoding for allowlisted languages (English layouts stay for
  English).
- Fetch behind the new `sherpa-g2p` feature via `LexiconArchive::fetch`
  (expanded preferred, base fallback — same order as `floravox_engine`).
  Without the feature: discovery only, no download (today's behaviour).
- Extend `should_auto_generate_lexicon`: allowlisted non-English VITS/piper
  models qualify for generation (English rules unchanged, still
  pairs-gated). Note the German path needs NO pairs table — gruut-de IS the
  model's training convention; the pairs requirement stays English-only.
- Read-only model dirs (iOS bundles): when the model dir is not writable,
  generate into `<cache_root>/lexicons-gen/<model_id>/lexicon.txt` and add
  that path to `collect_lexicons` for the config. New credentials key
  `lexiconCachePath` overrides the root. (Mostly moot under Path B, but
  fixes Path A for sandboxed clients — issue bullet 1's "writable model
  directory".)

### Phase 3 — Path B: the phonemize front-end

New `src/sherpa_g2p.rs` (feature `sherpa-g2p`), mirroring
`floravox_engine`'s proven chain:

- **Chain**: `MmapLexicon::open(<bundle>/<lang>.fst)` first, then
  `ChainedFallback(PhonetisaurusG2p::open(<bundle>/phonetisaurus.fst), …)`
  for OOV, wrapped in the existing `CachedPhonemizer` (1024 entries).
  WFST opens LAZILY on first OOV — it costs ~70 MB resident and ~650 ms
  cold (README numbers), which we don't pay for dictionary-only sentences
  on iOS.
- **Lookup normalization** mirrors sherpa's post-#4075 `NormalizeLexiconKey`:
  NFC → Unicode lowercase → NFC, so `Haus` matches `haus`-keyed entries and
  decomposed input composes. Gruut keys are stored lowercase for German
  (verify in Phase 4 corpus run; exact-then-lowercase if not).
- **Sentence splitting** on `. ! ? ; :` and newlines (#4075's set) —
  each sentence becomes one `Vec<i32>`.
- **Validation**: every mapped symbol must be a single Unicode codepoint
  present in `tokens.txt` (the codepoint path cannot address multi-char
  tokens). Any failure → whole model falls back to Path A with a clear
  one-line reason, not silent garbling.
- **Unmappable/OOV-after-WFST words**: dropped with an `eprintln!` + a
  counter surfaced in stats; never abort the sentence.
- **Hook**: in `synthesize`, when the loaded model is allowlisted and the
  feature is on and the chain loaded: phonemize → set
  `gen_config.phoneme_codepoints` → pass `""` as text to both
  `generate_with_config` call sites. Everything downstream (streaming
  delivery, prosody, boundary estimates, cancellation) is untouched.
- **German digits**: minimal normalizer (`3` → `drei` …) applied to tokens
  that are all digits, before lookup. Numbers otherwise vanish (upstream
  #4139). Dates/ordinals out of scope, documented as such.

### Phase 4 — German validation (the issue's acceptance gate)

- `#[ignore]` live tests (`tests/sherpaonnx_live.rs` pattern):
  - thorsten + expanded `deu` bundle: common words, proper names,
    inflections (`Häuser`, `liefen`, `des Kindes`), a compound
    (`Donaudampfschifffahrt`), OOV nonce words (must be SPOKEN via WFST,
    not dropped), digits.
  - assert: non-empty audio for every sentence incl. the OOV one; OOV
    skip counter == 0 on the in-dictionary sentences; sample rate 22050.
  - Path A regression: generated `lexicon.txt` loads and speaks (fallback
    path when the feature is off).
- CI (`.github/workflows/sherpaonnx-live.yml`): add the thorsten model +
  `deu` bundle download steps beside the eng-US ones; cache
  `~/.voicegarden/lexicons/deu`.
- iOS: extend `tests/ios_sherpa_smoke.c` — measure peak RSS with and
  without the WFST having loaded (budget: ≤ +80 MB).
- Measure OOV rate on a German corpus (e.g. 50k Wikipedia words) with the
  expanded bundle → recorded in this file; drives the allowlist decision.

### Phase 5 — enable download + docs

- Add `("de", …)` to the allowlist as validated → voices surface/download
  guidance updated (`get_voices` annotations or registry metadata, whatever
  the download-enablement mechanism consumes).
- Update `docs/engines.md`, `README.md` (non-English piper section), and
  the FFI/Swift docs for the new `lexiconCachePath` credentials key.
- Leave a pointer in `voicegarden-lexicons` to reconcile the README's
  "symbols all resolve" claim with the sherpa-runtime conversion need.

## Feature and dependency changes

```toml
# Cargo.toml
[features]
sherpa-g2p = ["dep:voicegarden-lexicons", "dep:floravox-g2p"]

[dependencies]
voicegarden-lexicons = { version = "0.3", optional = true }  # already present
floravox-g2p = { version = "=0.8.8", optional = true, default-features = false }  # already present
```

- NOT default: keeps default builds lean, mirrors `floravox-lexicons`.
- `floravox-g2p` WITHOUT the `onnx` feature — FST lexicon + Phonetisaurus
  only, no second ONNX runtime in the binary (the macOS CoreML/BNNS clash
  noted in Cargo.toml).
- `voicegarden-lexicons` vendors its own older `floravox-g2p`; only paths
  cross that boundary (same note as `floravox_engine.rs`).

## Risks

| Risk | Mitigation |
|---|---|
| `phoneme_codepoints` contract guessed wrong (pauses/punct/BOS-EOS) | Phase 0 spike gates Phase 3; A/B against lexicon-path audio |
| multi-codepoint tokens in some models break the codepoint path | per-model validation refuses → automatic Path A fallback |
| iOS memory (WFST ~70 MB) / cold start (~650 ms) | lazy load on first OOV; expanded bundle shrinks OOV rate; tier optional |
| sentence splitting diverges from sherpa's (abbreviations etc.) | use #4075's exact delimiter set; document; worst case odd pauses |
| bundle fetch at first speak (network on a speech path) | fetch at engine construction, fail soft to Path A; `VOICEGARDEN_LEXICON_DIR` for pre-seeded offline caches (iOS bundles) |
| German noun capitalization vs lexicon keys | normalize NFC+lowercase on both sides (matches #4075); verify gruut key casing in Phase 4 |
| writing lexicons into read-only model dirs | writable fallback dir + `lexiconCachePath`; Path B needs no writes at all |

## Acceptance criteria (from the issue)

| Issue bullet | Where |
|---|---|
| bundle resolved from model language, phones validated against tokens.txt | Phase 1 + Phase 2 validation |
| model-aware conversion + checksum-verified per-language cache | Phase 2 (crate does checksums) |
| clients can provide a writable model dir | Phase 2 (`lexiconCachePath` + fallback dir); moot under Path B |
| Phonetisaurus for OOV, phones passed to sherpa | Phase 3 (primary path) |
| common words / names / inflections verified on macOS + iOS | Phase 4 |
| enable download for validated Piper languages | Phase 5 (allowlist) |
