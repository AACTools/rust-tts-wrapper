# Non-English Piper G2P from VoiceGarden lexicons — implementation plan

Tracking issue: [#82](https://github.com/AACTools/rust-tts-wrapper/issues/82).
Decision record: [review comment](https://github.com/AACTools/rust-tts-wrapper/issues/82#issuecomment-6829299350).
On-device probe + build/quality notes (2026-11-12): see the later comment
on the issue.

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

Path B subsumes Path A. When the `sherpa-g2p` front-end is available it is
ALWAYS used; the generated `lexicon.txt` (Path A) becomes an explicit
compatibility option for interop — a converted lexicon beside the model
helps every other sherpa client of the same model dir (Phase 2). Both
paths share one model-aware phone converter (Phase 2).

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
(`src/sherpaonnx_engine.rs`), at the two
`tts.generate_with_config(text, &gen_config, …)` call sites (lines ~692
streaming and ~728 file paths). When the front-end is active for the loaded
model, it produces the codepoints and the call sites pass `""` as the text
argument — do NOT rebind `text` itself: both `EstimatePlan::build(text)`
sites (streaming ~608, and non-streaming ~744, which runs AFTER generate)
must keep the ORIGINAL text. They are WPM estimates over words and do not
depend on the phoneme stream.

## Phases

### Phase 0 — codepoint-contract verification (mostly retired)

~~Gating spike~~ → verification task. The risky half is already proven: the
2026-11-12 on-device probe ran Thorsten under sherpa 2.0.1 (v1.8.0 Apple
libraries) with hand-mapped German phones via `phoneme_codepoints` and got
22.05 kHz audio on arm64 macOS AND the iPhone simulator, with only the ONNX
model + `tokens.txt` present — no lexicon. The input path works on-device.

What remains, folded into Phase 3 (an afternoon, not a gate):

- Pin the exact symbol contract the same way, A/B against the lexicon-path
  audio: leading/trailing pause tokens, punctuation codepoints (`,.!?` are
  in tokens.txt), the inter-word space token, BOS/EOS markers (`^`/`$` if
  present). Wrong choices here produce plausible-but-odd prosody, which
  non-empty audio does NOT catch.
- Keep the probe as a tiny `examples/` binary for future models.

### Phase 1 — language resolution + validated-language allowlist

New `src/sherpa_lang.rs`:

- Resolve a model's language from registry metadata
  (`SherpaModelInfo::language[].lang_code`, e.g. `de_DE`), NOT id parsing.
  Normalize to BCP-47; bundle keys resolve via the crate's exact/prefix
  matching (`de-DE` → `de`). Keep excluding bilingual `<lang>_en`
  compounds (`is_bilingual_en_compound`).
- A compile-time allowlist — the single gate for everything downstream:

  ```rust
  struct LangBundle {
      bcp47: &'static str, // "de"
      archive: Archive,    // Base (lexicons.json) | Expanded (expanded.json)
      key: &'static str,   // "de" | "deu" — keys differ between archives
      validated: bool,     // flipped in Phase 5 once Phase 4 passes
  }
  ```

  The archive dimension is load-bearing: `de` exists only in the base
  `lexicons.json`, `deu` only in `expanded.json`, and the crate's
  `entry()` matches on `bcp47` — asking the expanded archive for "de"
  silently resolves `deu`. Fetch constructs EXACTLY the archive named in
  the row (`LexiconArchive::default_archive()` /
  `default_expanded()`), never the `expanded.or_else(base)` order
  `floravox_engine` uses. Lifecycle: both German rows ship from Phase 1
  behind the `sherpa-g2p` feature (`validated: false`); Phase 4's bake-off
  picks the default row; Phase 5 flips `validated` → download enablement.
- Unit tests: registry language → (key, archive); compounds rejected;
  unknown language → None. Registry `lang_code`s are bare codes
  (`"de"` with `country: "DE"` as a separate field — no underscore forms
  exist in the pinned registry), so fixtures must use real shapes, and
  normalization maps `de` → bundle lookup, not `de_DE` → `de`.

### Phase 2 — generalized model-aware converter + Path A for German

Generalize the English-only machinery in `src/sherpa_lexicon.rs` /
`src/sherpa_convention.rs` into a vocabulary-driven converter
(`PhonemeMap::for_language`-shaped). Per bundle phone symbol, in order:

1. exact match in the model's `tokens.txt` → identity;
2. strip tie bars (U+0361/U+035C) → if the result matches, use it; else
   split the components (`p͡f` → `p` `f`) when all are in vocab;
3. split modifier letters (length `ː` U+02D0, `ˑ` U+02D1) off the base when
   the composite is not a token (`uː` → `u` `ː`) and both parts are in vocab;
4. strip the offglide breve (U+032F): diphthongs are written `aʊ̯`/`aɪ̯`
   (base + offglide + U+032F) — try the composite, then base + plain
   offglide (`a ʊ`), per vocab;
5. strip remaining rare diacritics (see inventory below) and retry;
6. ASCII→IPA homoglyphs (`g` → `ɡ` U+0261; NFC does NOT do this) then retry 1;
7. stress marks (`ˈ` `ˌ`) deleted when absent from the vocab (German) /
   kept when present (English, unchanged);
8. else unmappable → Path A: skip entry (existing behaviour); Path B: word
   is dropped with a logged warning + counter.

Measured diacritic inventory across the v0.3.1 German lexicons (occurrence
counts; the converter must handle at least these):

| mark | codepoint | deu | de | rule |
|---|---|---|---|---|
| `ː` length | U+02D0 | 346,964 | 281,608 | split off base |
| `̯` offglide breve | U+032F | 11,911 | 10,781 | strip (try composite first) |
| `͡` tie bar | U+0361 | 7,910 | 7,014 | strip / split |
| `̩` syllabic | U+0329 | 60 | — | strip |
| `ˑ` half-long | U+02D1 | 28 | — | split off base |
| `̃ ̥ ͜ ̞ ˀ ̪ ̝` | assorted | ≤20 | ≤2 | strip |

Counted as standalone space-separated phone tokens: ASCII `g` appears
92,996 times in deu (82,705 in de); IPA `ɡ` appears zero times as a
standalone token — its single deu occurrence is inside the composite
`ɡ̥ː` (`gacki`), which chains rules 3+5+6 (IPA base + U+0325 + U+02D0)
and belongs in the converter unit tests. The homoglyph map carries real
weight. All lexicon keys are lowercase (0 capitalized keys in deu; `Haus`
is stored `haus`), so query normalization NFC → lowercase is correct, not
a guess.

Coverage test: the original probe converted all 277,912 base entries with
the first three rules; until Phase 4 selects the default, the test re-runs
conversion over BOTH bundles (`#[ignore]`-gated against the real files)
and asserts ≥ the probe's rate on each, plus offline unit tests against a
synthetic thorsten-like tokens.txt covering every mark in the table above
and the `ɡ̥ː` composite.

Path A wiring (explicit compatibility option — not an automatic fallback;
when the `sherpa-g2p` front-end is available it is always used):

- Opt-in via credentials (`"lexiconCompat": true`): generates a converted
  `lexicon.txt` beside the model (or in the fallback dir below) for
  interop — other sherpa clients and bare-sherpa usage of the same model
  dir benefit. English keeps today's automatic behaviour unchanged.
- Extend `discover_lexicon_assets`/`GRUUT_CACHE_LAYOUTS` to per-language
  layouts (`<key>/lexicon.txt` under the crate's cache root), replacing the
  eng-US/en hardcoding for allowlisted languages (English layouts stay for
  English).
- Fetch behind the new `sherpa-g2p` feature via `LexiconArchive::fetch`,
  constructing the archive pinned by the allowlist row (see Phase 1).
  Without the feature: discovery only, no download (today's behaviour).
- Extend `should_auto_generate_lexicon`: allowlisted non-English VITS/piper
  models qualify for generation (English rules unchanged, still
  pairs-gated). The German path needs NO pairs table — the operational
  basis is the probe's result, not convention identity: gruut-de is
  *close to* but demonstrably not identical to thorsten's encoding
  (mechanical normalization — tie bars, length marks, ASCII `g` —
  converted 277,912/277,912 entries), so conversion always runs and is
  always asserted; the pairs requirement stays English-only.
- **Update `test_lexicon_gate_against_full_registry`
  (`src/sherpaonnx_engine.rs:1699`)**: it asserts
  `!gated || is_english_model_id(&lower)` and pins per-family gated counts
  (piper 132, …). Extending the gate to allowlisted German models breaks
  both — deliberately: relax the invariant to "English or allowlisted"
  and re-pin the counts in the same PR.
- Read-only model dirs (iOS bundles): when the model dir is not writable,
  generate into `<cache_root>/lexicons-gen/<model_id>/lexicon.txt` and add
  that path to `collect_lexicons` for the config. New credentials key
  `lexiconCachePath` overrides the root. (Mostly moot under Path B, but
  fixes the compat option for sandboxed clients — issue bullet 1's
  "writable model directory".)

### Phase 3 — Path B: the phonemize front-end

New `src/sherpa_g2p.rs` (feature `sherpa-g2p`), mirroring
`floravox_engine`'s proven chain:

- **Chain**: `MmapLexicon::open(<bundle>/<lang>.fst)` first, then
  `ChainedFallback(PhonetisaurusG2p::open(<bundle>/phonetisaurus.fst), …)`
  for OOV, wrapped in the existing `CachedPhonemizer` (1024 entries).
  The WFST opens LAZILY on the first dictionary miss — its cost is
  bundle-dependent (see the bake-off numbers below) and is not paid for
  dictionary-only sentences on iOS.
- **Provisioning, not first-speak fetching**: the bundle is fetched and
  checksum-verified during model setup (engine construction with the model
  present / the model-download flow), so the first utterance works offline.
  A failed fetch must be RETRYABLE — do not cache the failure (the
  `OnceLock<Option<…>>` pattern in `floravox_engine` caches a miss forever;
  store the state so the next construction retries) — and construction
  fails soft to the plain text path with a logged reason.
- **Lookup normalization** mirrors sherpa's post-#4075 `NormalizeLexiconKey`:
  NFC → Unicode lowercase → NFC. Measured: all bundle keys are already
  lowercase (`Haus` is stored `haus`), so lowercased queries match directly;
  NFC composes decomposed input the same way sherpa now does.
- **Sentence splitting** on `. ! ? ; :` and newlines (#4075's set) —
  each sentence becomes one `Vec<i32>`.
- **Validation**: every mapped symbol must be a single Unicode codepoint
  present in `tokens.txt` (the codepoint path cannot address multi-char
  tokens). Any failure → the model uses the plain-text path with a loud
  one-line warning (for an espeak-orphaned model that means silence) and
  the log points at the `lexiconCompat` option — never silent garbling.
  Edge case to name in the code comment: if EVERY sentence phonemizes to
  empty (all words dropped), sherpa silently ignores empty codepoints
  (crate `tts.rs:629` guards `has_codepoints` on non-empty vectors) and
  synthesizes the empty text — surfacing as the existing "returned no
  audio" error, not a sherpa bug.
- **Unmappable/OOV-after-WFST words**: dropped with an `eprintln!` + a
  counter surfaced in stats; never abort the sentence.
- **Hook**: in `synthesize`, when the loaded model has an allowlist row
  (row PRESENCE activates Path B; `validated` gates only download
  surfacing, not the front-end — otherwise Phases 3–4 could not be tested
  end-to-end) and the feature is on and the chain loaded: phonemize → set
  `gen_config.phoneme_codepoints` → pass `""` as text at both
  `generate_with_config` call sites. Everything downstream (streaming
  delivery, prosody, boundary estimates, cancellation) is untouched.
- **German digits**: minimal normalizer (`3` → `drei` …) applied to tokens
  that are all digits, before lookup. Numbers otherwise vanish (upstream
  #4139). Dates/ordinals out of scope, documented as such.

### Phase 4 — German validation + the bundle bake-off (the issue's acceptance gate)

**Bake-off: `de` (base) vs `deu` (expanded) on the same word set.** The
manifests show a real trade-off, not a formality — expanded is NOT strictly
better:

| | `de` (base) | `deu` (expanded) |
|---|---|---|
| dictionary entries | 277,912 | 333,945 (base 306,939 + 27,006 distilled) |
| distilled-additions agreement | — | 0.711, `audit_exact: null`, no three-vote filter |
| Phonetisaurus holdout | 93.6% exact (README) | 86.1% exact, PER 3.6% (manifest) |
| `phonetisaurus.fst` | 32.5 MB | 50.2 MB |
| `lexicon.txt` / `.fst`+.pho | 11.7 MB / 9.4 MB | 13.7 MB / 11.3 MB |
| tarball (sha-verified) | 13.0 MB | 19.3 MB |

Note the expanded base is NOT the base bundle (306,939 vs 277,912 — the
expanded build starts from a larger gruut+wikipron staging set), and the
deu distilled additions carry 71% two-model agreement with no held-out
audit — those are the figures for the artifact we would ship, per the
manifest. The base bundle's WFST is markedly stronger; the expanded
dictionary is larger. The same-words test decides: run the Phase 4 word
set through BOTH bundles, score dictionary hits vs WFST guesses, and pick
the default from data; record the result here.

**Guesstimates (desktop/README-derived; physical-iPhone numbers to be
measured by hand on a real device — slot below):**

- WFST resident: order of the `.fst` file size when fully materialised
  (README measured ~70 MB desktop for a comparable model; deu has 934k
  states / 2.44M arcs, de is smaller). Budget line: **+35–75 MB**.
- WFST cold open: README ~650 ms on desktop (unstripped debug build,
  worst case); release on an A-series core likely **~0.3–0.8 s**. Lazy
  load keeps it off the first utterance unless an OOV word forces it.
- Disk: bundle tarball + extracted FSTs ≈ **45–95 MB**, i.e. the same
  order as a second piper model (64.1 MB for thorsten-medium per the
  registry).
- Dictionary tier: mmap'd, ~0 resident, <0.1 ms/word (README) — the tier
  almost every sentence hits.

⬜ **On-device measurement (physical iPhone, by hand)**: cold-start ms,
peak RSS with/without WFST loaded, first-utterance latency. Fill in here.

**Correctness tests** (`#[ignore]` live tests, `tests/sherpaonnx_live.rs`
pattern):

- thorsten + selected bundle: common words, proper names, inflections
  (`Häuser`, `liefen`, `des Kindes`), a compound
  (`Donaudampfschifffahrt`), OOV nonce words, digits.
- **Pre-synthesis phone assertions** (upstream #4139 lesson: a non-empty
  WAV passes even when the front-end dropped half the sentence): before
  any synthesis, assert that a capitalized noun (`Haus`), a name confirmed
  absent from the selected dictionary, and `42` each produce a non-empty
  phone sequence that is entirely ⊆ `tokens.txt`. Then the audio checks:
  non-empty for every sentence including the OOV one; OOV skip counter
  == 0 on in-dictionary sentences; sample rate 22050.
- Compat option regression: with `lexiconCompat`, the generated
  `lexicon.txt` loads and the plain-text path speaks.
- CI (`.github/workflows/sherpaonnx-live.yml`): add the thorsten model +
  both German bundle download steps beside the eng-US ones. Pre-seed the
  CRATE's own cache layout — `~/.voicegarden/lexicons/de/` (base) and
  `~/.voicegarden/lexicons/expanded/deu/` (expanded subdir, matching the
  existing `expanded/eng-US` layout) — AND write each dir's `.sha256`
  stamp with the manifest sha (`add4f489…` for de, `880f03da…` for deu):
  the crate's cache hit requires the stamp plus the `fst`/`pho` files, so
  without it `fetch` re-downloads 13–19 MB every run. With it, the live
  tests exercise the real `fetch` path and find the bundles cached.
- iOS: extend `tests/ios_sherpa_smoke.c` — front-end on, WFST forced to
  load (one OOV sentence), assert audio and no crash.
- Measure OOV rate on a German corpus (e.g. 50k Wikipedia words) with the
  selected bundle → recorded here; drives the allowlist decision.

### Phase 5 — enable download + docs

- Flip `validated: true` on the bake-off-winning German row (both rows have
  existed since Phase 1, `validated: false`) → voices surface/download
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
  Deliberately independent of it too — `floravox-lexicons` drags in the
  whole Floravox synthesis engine (`floravox-core`, an ONNX runtime),
  while the Apple XCFramework builds with `--no-default-features
  --features cloud,sherpaonnx` and must not.
- **Enable in both Apple build scripts**: `scripts/apple/build-xcframework.sh`
  (line 19) and `scripts/build-ios-sherpa.sh` (line 81) change to
  `--features cloud,sherpaonnx,sherpa-g2p`. Check `.github/workflows` and
  `Package.swift` for any other place the Apple feature set is spelled out.
- `floravox-g2p` WITHOUT the `onnx` feature — FST lexicon + Phonetisaurus
  only, no second ONNX runtime in the binary (the macOS CoreML/BNNS clash
  noted in Cargo.toml).
- `voicegarden-lexicons` vendors its own older `floravox-g2p`; only paths
  cross that boundary (same note as `floravox_engine.rs`).

## Risks

| Risk | Mitigation |
|---|---|
| `phoneme_codepoints` prosody contract wrong (pauses/punct/BOS-EOS) — input path itself proven on-device | contract A/B vs lexicon-path audio in Phase 3; probe binary kept |
| multi-codepoint tokens in some models break the codepoint path | per-model validation refuses → loud warning + plain-text path; `lexiconCompat` as repair |
| iOS memory / cold start (bundle-dependent: deu WFST 50 MB, de 32 MB) | lazy load on first dictionary miss; bake-off picks the bundle; physical-device measurement slotted |
| wrong default bundle (expanded's distilled additions un-audited at 0.711 agreement; base's WFST stronger) | Phase 4 bake-off on the same word set decides; manifest figures recorded above |
| sentence splitting diverges from sherpa's (abbreviations etc.) | use #4075's exact delimiter set; document; worst case odd pauses |
| bundle fetch on the speech path / offline first utterance | provision at model setup; retryable failures (no cached misses); `VOICEGARDEN_LEXICON_DIR` for pre-seeded offline caches (iOS bundles) |
| German noun capitalization vs lexicon keys | resolved by measurement: all keys lowercase; NFC+lowercase queries (matches #4075) |
| writing lexicons into read-only model dirs | compat option writes to fallback dir + `lexiconCachePath`; primary path needs no writes at all |

## Acceptance criteria (from the issue)

| Issue bullet | Where |
|---|---|
| bundle resolved from model language, phones validated against tokens.txt | Phase 1 + Phase 2 validation |
| model-aware conversion + checksum-verified per-language cache | Phase 2 (crate does checksums) |
| clients can provide a writable model dir | Phase 2 (`lexiconCachePath` + fallback dir); moot under Path B |
| Phonetisaurus for OOV, phones passed to sherpa | Phase 3 (primary path) |
| common words / names / inflections verified on macOS + iOS | Phase 4 |
| enable download for validated Piper languages | Phase 5 (allowlist) |
