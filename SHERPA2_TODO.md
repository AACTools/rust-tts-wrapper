# sherpa-onnx 2.x upgrade + model expansion — working TODO

Not committed; the working checklist for this effort. One PR per numbered
phase, loop-review on each, CI-clean (fmt + clippy -D warnings + tests
across feature combos).

## STATUS: Phases 0–2 COMPLETE (2026-10-28)

- Phase 1 (#65, merged): sherpa-onnx 2.0.1 upgrade
- Phase 2 (#67, merged; #66 closed): P2P lexicon generation
  - pairs shipped: voicegarden-lexicons v0.3.1 (p2p-misaki.tsv +
    p2p-espeak.tsv inside en/eng-US bundles; release tooling with
    reproducible tars + corrected manifest shas)
  - floravox #18 (merged): PhonemeTransducer + trainer --p2p (WFST
    fallback 76.6% exact after loop-review fixed the search bugs)
  - floravox v0.8.10 tagged; **publish workflow blocked by GitHub
    Actions billing** — rerun the Publish run after fixing billing
- Phase 3 (Hume): not started

## Phase 0 — investigation (DONE, findings below)

**The headline**: sherpa-onnx 2.0.0 REMOVED espeak-ng + piper-phonemize
(PR #4055, fixes #3731) — sherpa is now GPL-free. Text frontends in 2.x:

1. **built-in lexicon** — needs lexicon.txt (+ optional dict_dir); the
   improved path (#4075: word boundaries/OOV/Unicode fixes, verified
   phoneme-stream-identical to espeak on tested text)
2. **external phonemization** — caller phonemizes and passes
   `GenerationConfig::phoneme_codepoints` (Vec<Vec<i32>> codepoints per
   sentence) or `::tokens` (pinyin-style strings); mutually exclusive
   with `text`

**API deltas (1.13.8 → 2.0.1, verified from crate sources):**
- `data_dir` on ALL tts model configs: deprecated, IGNORED at runtime
  (ABI slot kept) — our espeak-ng-data plumbing becomes dead
- Kokoro config gains `lexicon: Option<String>` + `lang: Option<String>`
- `GenerationConfig` gains `phoneme_codepoints`, `tokens`, `num_steps`,
  `extra` map; entry point `generate_with_config(text, &config, cb)`
- 2.0.1 adds VoxLock (target-speaker voice isolation, ASR-side; note for
  later), kokoro int8 keeps STFT convs f32 (quantization-quality fix)

**Model-family impact matrix (to verify empirically in Phase 1):**
| family | 1.13.8 frontend | 2.x expectation |
|---|---|---|
| piper (vits) | espeak-ng-data | lexicon-only (needs lexicon.txt we may supply) OR external phonemize |
| mms/melo (vits) | lexicon.txt | unchanged ✓ (improved OOV) |
| matcha | lexicon.txt/tokens | unchanged ✓ (multi-lexicon support added) |
| kokoro | espeak-ng-data + lexdict | NEW lexicon/lang fields → lexicon path, or external phonemize |
| supertonic | ? | verify empirically |
| zipvoice | ? | verify empirically |

**Chatterbox / NeuTTS / CosyVoice in 2.x**: not found in changelog nor
crate sources — 2.x did NOT add them. Deprioritized; the upgrade itself
+ phonemization architecture is the value.

**floravox-lexicons role (the user's call, confirmed important):**
voicegarden-lexicons = MIT gruut lexicons (13 langs) + CMUDict +
Phonetisaurus OOV WFSTs, cached+verified downloads, unpacked into
floravox FST lexicons. Two integration surfaces:
  a. **lexicon.txt generation** — write a sherpa-format lexicon for the
     built-in frontend (family-permitting)
  b. **external phonemization** — phonemize text → phoneme_codepoints
     via the FST lexicons (the piper-phonemize replacement, GPL-free)
Phoneme-convention question: kokoro tokens.txt is IPA; gruut's set is
IPA-adjacent — mapping audit needed per family.

## Phase 1 EMPIRICAL RESULTS (2.0.1, i5-8500T, real models)

| family | model tested | text-in result | why |
|---|---|---|---|
| piper (vits) | piper-en_US-amy-low | ❌ no audio → ✅ audio with lexicon.txt present, but GARBLED ("It's lit right?") | espeak gone; engine now passes lexicon; kokoro-format lexicon's phoneme conventions mismatch piper tokens — needs Phase 2 per-model mapping |
| kokoro-en | kokoro-en-v0_19 (int8) | ❌ no audio → ✅ SPEAKS with lexicon.txt dropped in (imperfect: "How this is the rescued") — lexicon built for the multi-lang model's 177-token set | engine already collects lexicon-*.txt; same-format lexicon works, quality depends on token-set match |
| kokoro multi-lang | kokoro-zh_en-multi-lang | ✅ works out of the box | ships its own lexicons |
| supertonic | supertonic-3-multilingual | ✅ works | own tokenizer |
| mms / melo / matcha | not tested locally (registry ids lack bundled models) | expected ✅ | lexicon/char frontends, no espeak involvement |
| kitten | not in registry 1.1.0 | unknown — espeak-shaped dir suggests broken like kokoro | test when reachable |
| zipvoice / pocket | not tested | expected ✅ (own frontends) | |

**The rescue mechanism confirmed**: a plain-text lexicon (`WORD ph ph ph`,
178k entries, IPA symbols) dropped into a model dir makes espeak-orphaned
models speak on 2.x. English lexicon source for Phase 2:
voicegarden-lexicons (gruut/CMUDict/Phonetisaurus) with per-model
token-vocabulary mapping (read tokens.txt, map source phonemes to
in-vocab symbols).

## Phase 1 — the upgrade PR (feat/sherpa2-upgrade → PR #65)

**STATUS: loop 1 review done (3/10) — rework applied, awaiting loop 2.**

Loop-1 findings (all addressed in the rework):
- ❌ example broke non-sherpa builds (ungated imports) → now
  `required-features = ["sherpaonnx"]` in Cargo.toml
- ❌ CI clippy (newer stable) caught chunks_exact → as_chunks + sign-loss
  cast fixes
- ❌ hardcoded 24 kHz in example (piper is 22050!) → new
  `SherpaOnnxEngine::loaded_sample_rate()` getter from the registry
- ❌ inert lexicon-us-en.txt fallback (matched zero downloadable
  archives) → replaced with `collect_lexicons()` (all lexicon*.txt,
  same collector kokoro uses)
- ❌ kitten never sets lexicon under 2.x → wired to collect_lexicons
- ❌ stale espeak-era doc comments + assert messages → updated
- ❌ live tests failed on CI without annotation → piper (6) + matcha (2)
  tests carry `#[ignore = "... #66"]` reasons; tracking issue #66 filed
  (lexicon generation = Phase 2 scope)

Progress:
- [x] Branch feat/sherpa2-upgrade off main
- [x] Bump sherpa-onnx + sherpa-onnx-sys =1.13.8 → =2.0.1 (pinned pair)
- [x] Fix compile deltas (none needed — API source-compatible)
- [x] Model-map: every existing family still detected
- [x] Phonemization plumbing (lexicon collection for vits + kitten;
      data_dir kept but documented dead)
- [x] EMPIRICAL smoke per family (matrix above)
- [x] fmt + clippy -D warnings: sherpaonnx / pocket-timing /
      system+cloud+sherpaonnx combos clean; floravox leg has a
      PRE-EXISTING on-main `unknown lint clippy::assert_is_empty` (our
      local clippy is older than CI's 1.99 — the allow attribute exists
      in tests/floravox_live.rs; needs a toolchain bump or lint-version
      guard, NOT this PR's problem)
- [x] Non-sherpa builds: default, no-default, examples all compile
- [x] Push rework + rewrite PR body honestly
- [x] Loop-1.5: CI's sherpa-live runs `--ignored` (runs ALL live tests) —
      #[ignore = reason] annotations alone don't skip them. Added
      runtime guards (SHERPA_MAJOR const + is_sherpa2_orphan +
      skip_if_sherpa2_orphan) to all 12 espeak-orphaned live tests;
      local live run: 20 passed / 0 failed (with piper models present).
      Const flips automatically nothing — it's manual, documented to
      remove when #66 lands.
- [x] Loop-review 2 → **7/10**: all loop-1 blockers verified closed;
      6 residual items (duration-print hardcode, stale collect_lexicons
      doc, over-guarded stop test, un-guard count 8→12, pin↔const
      cross-ref, confusing assert msg) — all fixed in the loop-2 commit.
      macOS/Windows sherpa-live legs now vacuously green (only download
      piper/matcha models, all guarded) — noted as pre-existing CI gap,
      not this PR's scope.
- [x] CI green on the loop-2 commit (28/28) → READY TO MERGE

## Phase 2 — lexicon generation PR (tracking issue #66) — DONE, #67 MERGED

Branch: feat/sherpa-lexicons (off main, after #65 merged).

**STATUS: COMPLETE. #67 merged (28/28 CI), #66 closed. Both conventions
shipped: misaki pairs (kokoro-en, kitten) + espeak pairs (piper,
matcha-en) in voicegarden-lexicons v0.3.1. Live tests un-guarded and
passing on all 3 CI platforms.**

Key discoveries (recorded for posterity):
- sherpa-onnx 2.x has TWO lexicon formats: space-separated (old) feeds
  raw ids with no BOS/EOS/pads — garbles piper; `word||phonemes`
  (new) routes through ParseLexiconFile with proper piper-style
  wrapping + phrase matching. Generation emits the new format.
- CMUdict's stress markers largely agree with espeak conventions once
  function words are destressed (CMUdict stresses every monosyllable;
  espeak doesn't stress function words).
- The "structural WFST limitation" was two search bugs + a trainer bug
  (see floravox #18): pipe keys stripped in build (compound arcs
  unreachable), epsilon arcs teleporting the input cursor (the vowel
  dropping), double-joined compound symbols. Fixed: 76.6% exact /
  4.4% PER / 100% coverage on held-out pairs.
- The old voicegarden manifests' sha256s mismatched the downloadable
  bytes for 13/14 bundles (non-reproducible tars) — the client's
  checksum would have failed. v0.3.1 fixes this + reproducible tars.

Scope locked by Phase 1 results: the ONLY viable front-end for
espeak-orphaned families (piper 536 voices, matcha-en, kokoro-en,
kitten) on 2.x is a per-model lexicon.txt.

### Phase 2 findings (gruut vs model conventions — the hard part)

The gruut→vocab symbol mapping is SOLVED (all 40 gruut IPA symbols map
or decompose into piper's 130-token vocab; 100% coverage, zero skips,
124k entries generated). But **symbol mapping ≠ phonological equivalence**:
each model family was trained on a different phonemizer's output and
expects that phonemizer's transcription CONVENTIONS:

| family | trained on | gruut matches? | evidence |
|---|---|---|---|
| kokoro-en | misaki | ❌ different | kokoro multi-lang lexicon uses uppercase `O` `A` `I` as internal phoneme symbols; gruut says `oʊ` `eɪ` — same sound, different encoding |
| piper | espeak-ng | ⚠️ close but wrong | gruut lexicon → audio but garbled ("No, this is Aster"); gruut marks stress on monosyllables, uses `ɛ` where espeak uses `ə` in unstressed syllables, etc. |
| matcha-en | espeak-ng (same as piper) | ⚠️ same issues | |
| kitten | misaki (like kokoro) | ❌ | |

**Key discovery: the kokoro multi-lang release ships `lexicon-us-en.txt`
and `lexicon-gb-en.txt` that WORK for kokoro-en** (proven in Phase 1).
That file uses the misaki convention and matches kokoro-en's tokens.

### Revised Phase 2 approach: per-family lexicon source
- [ ] kokoro-en / kitten: use the kokoro multi-lang's bundled
      lexicon-us-en.txt (already proven to work — just document the
      download path; or ship a copy in voicegarden-lexicons)
- [ ] piper / matcha-en: gruut→espeak phonological convention mapping
      (not just symbol mapping — stress placement, unstressed vowel
      reduction, rhotic unwinding). This is real linguistic work:
      - espeak reduces unstressed vowels to schwa more aggressively
      - espeak doesn't stress monosyllabic function words
      - espeak represents some diphthongs differently
      - Alternative: find/generate an espeak-compatible English lexicon
        (CMUdict with espeak-style phoneme labels, or phonemize the
        gruut lexicon through espeak once and cache)
- [ ] A/B: espeak-1.13 output vs lexicon-2.x output, whisper round-trip
- [ ] Un-guard the 12 orphaned live tests; close #66
- [ ] PR + loop-review + CI green

### Code written so far (sherpa_lexicon.rs + sherpa_convention.rs + sherpa_p2p.rs, 23 tests green)
- PhonemeMap::for_english() — builds gruut→target symbol mapping
- generate_lexicon_txt() — gruut lexicon → sherpa lexicon.txt format
- Convention::Misaki / Convention::Espeak — rule-based convention
  conversion: symbol substitution + function-word destressing + rhotic
  splitting + unstressed vowel reduction
- convert_lexicon_file() — layered generation: P2P table → rules →
  vocab filter, returns (written, skipped, via_table)
- P2pTable (sherpa_p2p.rs) — aligned-pairs TSV → exact lookup
- ensure_generated_lexicon() + discover_model_assets() — engine-side
  auto-generation with convention detection from tokens.txt

### Phase 2 hard finding: word-specific phonological differences

The rules-based conversion handles the SYMBOL layer and the systematic
PHONOLOGICAL layer, but a long tail of word-specific differences makes
the output garbled in practice:

| word | gruut (after rules) | misaki (correct) | issue |
|---|---|---|---|
| hello | `h ə l ˈ O` | `h ə l ˈ O` | ✓ identical |
| world | `w ˈ ɜ ɹ l d` | `w ˈ ɜ ɹ l d` | ✓ identical |
| go | `ɡ ˈ O` | `ɡ ˌ O` | stress level: primary vs secondary |
| the | `ð ə` | `ð i` | vowel quality: schwa vs /i/ |
| a | `ə` | `A` | strong form (letter name) vs weak form |
| converted | `k ə n v ˈ ɜ ɹ t ɪ d` | `k ə n v ˈ ɜ ɹ ɾ ᵻ d` | flap + barred-i allophones |

Most words (~90% of the tested 100) produce IDENTICAL output after the
rules. The ~10% that differ are enough to garble Whisper transcription.

### P2P transducer experiment (Phonetisaurus phoneme→phoneme)

**Architecture**: train a Phonetisaurus WFST on aligned phoneme pairs
(gruut → kokoro bundled), then at runtime: gruut phonemize →
transduce → model-ready phonemes. Single G2P source, per-convention
transducer, no per-model bundles. This is the RIGHT architecture.

**What we built**:
- `--p2p` mode in floravox-train-phonetisaurus (atomic pipe-separated units,
  identity-pair boost ×50 after co-occurrence counting)
- `PhonetisaurusG2p::transduce()` method (token-level search, same Dijkstra)
- 49k aligned pairs from gruut↔kokoro overlap, model trains in seconds
- `PhonemeTransducer` (floravox-g2p `p2p` module): direct-lookup HashMap +
  WFST fallback hybrid — lookup is exact for all in-vocabulary words

**What works**:
- Tiny model (10 examples): PERFECT transduction
- Symbol coverage: 100%, zero vocabulary violations
- The training + decode pipeline is sound
- The **lookup layer** of the hybrid: 100% exact on in-vocabulary words

**The WFST fallback limitation (accepted)**:
- Large models (49k pairs, 100k states): the Dijkstra decode takes
  epsilon-heavy paths through n-gram backoff arcs and drops vowels
  systematically; epsilon penalties (up to 100.0) and epsilon-closure
  both fail to fix it. The model structure — not the search — lacks
  reachable output arcs for those vowels.
- Root cause: M2M alignment optimises corpus likelihood, decomposing
  compound phonemes across the corpus; the n-gram WFST then generalises
  that decomposition.
- Pragmatic resolution: **hybrid** — direct HashMap lookup for
  in-vocabulary words (100% correct), WFST fallback only for OOV
  sequences (approximate). For lexicon GENERATION the rules layer is
  the OOV fallback instead (see below); the WFST matters only for
  runtime phonemization of unseen words, where approximation is
  acceptable.

### Phase 2 SOLUTION: P2P lookup table + rules fallback (VALIDATED)

`src/sherpa_p2p.rs` — std-only `P2pTable`: aligned-pairs TSV
(`gruut|ph1|ph2\ttgt1 tgt2`) → HashMap. Layered into
`convert_lexicon_file` as: table (exact, word-specific) → rules
(systematic) → vocab filter.

**Validation against the kokoro-en ground truth** (the model's own
bundled `lexicon-us-en.txt`, aligned via
`prepare_p2p_data.py`):
- 133,637 entries generated from the gruut lexicon, **0 skipped**
  (100% vocab coverage), 53,213 via table / 80,424 via rules
- All six garbling-causing words from the rules-only experiment now
  match ground truth EXACTLY: `go ɡ ˌ O` (stress level), `the ð i`
  (weak form), `a A` (strong form), `converted ... ɾ ᵻ d` (allophones),
  `hello`, `world`
- 88.8% of the 53,146 overlap words are byte-identical to ground truth;
  the residual 11.2% differs only in sub-phonemic allophone choices
  (`ə` vs `ᵊ`, plain vs flapped `t`/`d` — both schwa/flap class, no
  garbling) and homophone collisions inherent to phoneme-sequence
  keying (first-wins, stable)

**Engine wiring done**: `sherpaonnx_engine` calls
`ensure_generated_lexicon` at model load — when an espeak-orphaned
model ships no lexicon and (gruut lexicon + aligned pairs) are locally
available, one is generated into the model dir. Convention is detected
from the model's own tokens.txt (uppercase `A I O W Y` ⇒ misaki).
Asset discovery: `SHERPA_GRUUT_LEXICON`/`SHERPA_P2P_PAIRS` env vars →
pairs beside the model or gruut lexicon → voicegarden cache. Pairs are
REQUIRED (rules alone garble ~10% of words); no pairs → no generation.
A/B wavs generated (`examples/kokoro-ab`).

### Phase 2 remaining — ALL DONE (2026-10-28)
- [x] PR #67 merged (4 review loops 6→6→7→8, then the `||`-format
      fix + un-guarding); #66 closed
- [x] Pairs shipped as voicegarden-lexicons v0.3.1 release assets
      (both tables inside en.tar.gz + eng-US.tar.gz, beside
      lexicon.txt) — auto-generation works out of the box; CI
      downloads the eng-US bundle
- [x] p2p-espeak.tsv generated from gruut↔CMUDict-IPA alignment
      (123,747 overlap words, 94% of the gruut lexicon routes through
      the exact table)
- [x] floravox #18 merged (2 loops 5→8.5); v0.8.10 tagged — publish
      workflow BLOCKED by GitHub Actions billing (rerun after fixing)
- [x] A/B: whisper round-trips word-identical to the pre-2.x espeak
      reference (piper-en_US-amy-low, 4 sentences incl. stress cases)

## Phase 3 — Hume cloud engine (separate quiet PR)
- [ ] cloud_engine: hume provider following cartesia/deepgram patterns
- [ ] Mocked-HTTP tests; PR + loop-review + CI green

## Standing rules
- No commits to main; feature branch per phase, PR each
- loop-review on every PR; address blockers before merge
- CI parity locally: fmt, clippy -D warnings per feature, full tests;
  mirror the lint-and-test matrix legs before pushing
- Models stay optional features; heavyweight out of default
