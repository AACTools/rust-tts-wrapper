//! Phoneme-convention mapping from voicegarden (gruut) lexicons to
//! sherpa-onnx model vocabularies, and sherpa-format `lexicon.txt`
//! generation for espeak-orphaned models (piper, matcha-en, kokoro-en,
//! kitten) under sherpa-onnx 2.x.
//!
//! sherpa-onnx 2.0 removed the bundled espeak-ng phonemizer; models whose
//! only phonetic front-end was `espeak-ng-data/` synthesize silence
//! unless a lexicon ships beside the model. This module generates one
//! from the MIT-licensed voicegarden gruut lexicons (see #66).
//!
//! Two converter flavors:
//! - [`PhonemeMap::for_english`] — the #66/#67 tables for English
//!   (gruut → misaki/espeak conventions, P2P pairs, phonological rules).
//! - [`VocabPhonemeMap`] — the vocabulary-driven ladder for any bundle
//!   language (#82): no per-language tables, everything derived from the
//!   target `tokens.txt` plus universal Unicode facts (tie bars, length
//!   marks, the offglide breve, ASCII→IPA homoglyphs, NFD base
//!   stripping). Validated against `piper-de_DE-thorsten-medium`: the
//!   base `de` bundle converts 277,912/277,912 entries, expanded `deu`
//!   333,944/333,945 (one loanword carries `ɝ`, which no German model
//!   vocabulary contains).
//!
//! Non-English gruut languages (de, fr, es, …) follow the same pattern:
//! the generator reads the target model's `tokens.txt` and only emits
//! words whose phonemes are fully in-vocabulary after mapping, so it
//! works for any language where the gruut inventory ⊆ model inventory
//! (or decomposes into it).

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization as _;

/// A gruut→model phoneme mapping. Each gruut source symbol maps to a
/// sequence of model-vocabulary symbols (identity for most, two-part
/// decomposition for diphthongs/affricates, stress + parts for stressed
/// variants).
#[derive(Debug)]
pub struct PhonemeMap {
    /// gruut symbol → sequence of target-vocab symbols
    map: std::collections::HashMap<String, Vec<String>>,
    /// the target vocabulary (from tokens.txt) — used to filter unmapped
    target_vocab: HashSet<String>,
}

/// gruut English base phonemes that appear as single IPA characters in
/// every observed sherpa-onnx English model vocabulary.
const GRUUT_EN_IDENTITY: &[&str] = &[
    "b", "d", "f", "h", "i", "j", "k", "l", "m", "n", "p", "s", "t", "u", "v", "w",
    "z", // consonants
    "æ", "ð", "ŋ", "ɑ", "ɔ", "ə", "ɚ", "ɛ", "ɡ", "ɪ", "ɹ", "ʃ", "ʊ", "ʌ", "ʒ",
    "θ", // vowels & fricatives
];

/// gruut multi-character symbols and their two-part decompositions.
/// Every component character is present in every observed vocabulary.
const GRUUT_EN_DECOMP: &[(&str, &str, &str)] = &[
    ("aɪ", "a", "ɪ"),
    ("aʊ", "a", "ʊ"),
    ("eɪ", "e", "ɪ"),
    ("oʊ", "o", "ʊ"),
    ("ɔɪ", "ɔ", "ɪ"),
    ("t͡ʃ", "t", "ʃ"),
    ("d͡ʒ", "d", "ʒ"),
];

const STRESS_PRIMARY: &str = "ˈ";
const STRESS_SECONDARY: &str = "ˌ";

/// Lexicon-generation failure (parse or generation error message).
#[derive(Debug)]
pub struct LexiconGenError(pub String);

impl std::fmt::Display for LexiconGenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for LexiconGenError {}

impl PhonemeMap {
    /// Build a mapping for the gruut English inventory against a target
    /// model's `tokens.txt` vocabulary.
    ///
    /// # Errors
    /// Returns an error if the tokens.txt is unreadable or if any
    /// identity-mapped symbol is absent from the vocabulary (the model
    /// uses a non-IPA convention — kitten/matcha non-English variants).
    pub fn for_english(tokens_path: &Path) -> Result<Self, LexiconGenError> {
        let target_vocab = read_tokens_txt(tokens_path)?;

        // verify all identity symbols are in the target vocab
        let missing: Vec<&str> = GRUUT_EN_IDENTITY
            .iter()
            .filter(|s| !target_vocab.contains(**s))
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(LexiconGenError(format!(
                "target vocabulary missing {} identity symbols: {} — not an IPA-convention English model",
                missing.len(),
                missing.join(" ")
            )));
        }

        // verify all decomposition components are in the target vocab
        for (src, a, b) in GRUUT_EN_DECOMP {
            if !target_vocab.contains(*a) || !target_vocab.contains(*b) {
                return Err(LexiconGenError(format!(
                    "target vocabulary missing decomposition components for {src}: need {a} + {b}"
                )));
            }
        }

        let mut map = std::collections::HashMap::new();

        // identity symbols
        for sym in GRUUT_EN_IDENTITY {
            map.insert((*sym).to_string(), vec![(*sym).to_string()]);
        }

        // decompositions
        for (src, a, b) in GRUUT_EN_DECOMP {
            let parts = vec![a.to_string(), b.to_string()];
            map.insert((*src).to_string(), parts.clone());
            // stressed variants: ˈsrc → ˈ a b, ˌsrc → ˌ a b
            map.insert(format!("{STRESS_PRIMARY}{src}"), {
                let mut v = vec![STRESS_PRIMARY.to_string()];
                v.extend(parts.iter().cloned());
                v
            });
            map.insert(format!("{STRESS_SECONDARY}{src}"), {
                let mut v = vec![STRESS_SECONDARY.to_string()];
                v.extend(parts.iter().cloned());
                v
            });
        }

        Ok(Self { map, target_vocab })
    }

    /// Map a sequence of gruut phoneme tokens to target-vocabulary tokens.
    /// Returns `None` if any token is unmappable (caller should skip the
    /// word — partial lexicons are fine, sherpa falls back to character
    /// splitting for OOV words).
    #[must_use]
    pub fn map_phonemes(&self, gruut_tokens: &[&str]) -> Option<Vec<String>> {
        let mut out = Vec::with_capacity(gruut_tokens.len() + 2);
        for tok in gruut_tokens {
            if let Some(mapped) = self.map.get(*tok) {
                out.extend(mapped.iter().cloned());
            } else if self.target_vocab.contains(*tok) {
                // already in the target vocab (punctuation, pauses, etc.)
                out.push((*tok).to_string());
            } else {
                return None;
            }
        }
        Some(out)
    }

    /// Number of gruut source symbols in the mapping.
    #[must_use]
    pub fn source_symbol_count(&self) -> usize {
        self.map.len()
    }
}

/// Read a sherpa-onnx `tokens.txt` into a vocabulary set.
/// Format: `symbol id` per line (the id is ignored; symbols may be
/// multi-character Unicode).
///
/// # Errors
/// Returns an error if the file is unreadable or contains invalid UTF-8.
pub fn read_tokens_txt_public(path: &Path) -> Result<HashSet<String>, LexiconGenError> {
    read_tokens_txt(path)
}

// ──────────────────────────────────────────────────────────────────
// Vocabulary-driven symbol ladder (#82)
// ──────────────────────────────────────────────────────────────────

/// Tie bars joining an affricate into one symbol (`p͡f`, `t͡s`).
const TIE_BARS: [char; 2] = ['\u{0361}', '\u{035C}'];
/// Modifier letters that lengthen the preceding vowel.
const LENGTH_MARKS: [char; 2] = ['\u{02D0}', '\u{02D1}'];
/// Stress marks (suprasegmental; German bundle carries none, gruut
/// English does).
const STRESS_MARKS: [char; 2] = ['\u{02C8}', '\u{02CC}'];
/// Diacritics stripped after identity/tie/modifier attempts: the
/// offglide breve (`̯`, diphthong offglides `aʊ̯`), syllabic (`̩`),
/// voiceless (`̥`), nasalized (`̃`), lowered (`̞`), dental (`̪`),
/// raised (`̝`), pre-aspirated (`ˀ`). Occurrence counts in the v0.3.1
/// German bundles are recorded in SHERPA_NONENGLISH_G2P_PLAN.md (the
/// offglide breve is common — ~12k — the rest ≤60 each).
const STRIPPABLE_DIACRITICS: [char; 8] = [
    '\u{032F}', '\u{0329}', '\u{0325}', '\u{0303}', '\u{031E}', '\u{032A}', '\u{031D}', '\u{02C0}',
];

/// ASCII→IPA homoglyphs: phonemizers that emit ASCII look-alikes for IPA
/// letters. NFC/NFD do NOT fold these. ASCII `g` appears 92,996 times in
/// the deu lexicon; IPA `ɡ` (U+0261) is what piper vocabularies carry.
const HOMOGLYPHS: &[(char, char)] = &[('g', '\u{0261}')];

fn homoglyph_sub(s: &str) -> String {
    s.chars()
        .map(|c| {
            HOMOGLYPHS
                .iter()
                .find(|(a, _)| *a == c)
                .map_or(c, |(_, b)| *b)
        })
        .collect()
}

/// A bundle phone symbol → model-token-sequence mapper driven entirely
/// by the target vocabulary: no per-language tables. The ladder, per
/// symbol (SHERPA_NONENGLISH_G2P_PLAN.md Phase 2, rules 1–8):
///
/// 1. NFC → exact vocabulary match (identity);
/// 2. homoglyph substitution (`g` → `ɡ`), identity retry;
/// 3. tie bars: the tie-stripped whole (`p͡f` → `pf`) if it is a token,
///    else split at the tie bars and resolve each segment recursively
///    (`p͡f` → `p` `f`; `t͡s̯` → `t` `s` via the segment ladder);
/// 4. trailing length mark split (`uː` → `u` `ː`), base resolved
///    recursively;
/// 5. strip one strippable diacritic and recurse (offglide breve before
///    the rare ones);
/// 6. NFD: precomposed letters (`ã ä ü õ`) decompose to base + combining
///    mark; drop the combining marks and retry (`äː` → `a` `ː`);
/// 7. stress marks: bare marks absent from the vocabulary map to nothing
///    (dropped); stress-prefixed symbols retry without the prefix;
/// 8. character split as the last resort (`aʊ̯` → `a` `ʊ` after the
///    breve strip);
///
/// Returns `None` when unmappable — the caller skips the word / entry.
///
/// Resolutions are memoized per symbol: a lexicon repeats symbols
/// heavily (92,996 `g`s), and the ladder allocates.
pub struct VocabPhonemeMap {
    target_vocab: HashSet<String>,
    cache: std::collections::HashMap<String, Option<Vec<String>>>,
}

impl VocabPhonemeMap {
    /// Build a ladder over a model's `tokens.txt`.
    ///
    /// # Errors
    /// Returns an error if the tokens.txt is unreadable, invalid UTF-8,
    /// or parses to an empty vocabulary (same refusal as the English
    /// path: an empty vocabulary would silently convert nothing).
    pub fn for_language(tokens_path: &Path) -> Result<Self, LexiconGenError> {
        let target_vocab = read_tokens_txt(tokens_path)?;
        if target_vocab.is_empty() {
            return Err(LexiconGenError(format!(
                "{} parsed to an empty vocabulary — refusing to convert a lexicon",
                tokens_path.display()
            )));
        }
        Ok(Self {
            target_vocab,
            cache: std::collections::HashMap::new(),
        })
    }

    /// Map one bundle phone symbol to target-vocabulary tokens. `None` =
    /// unmappable; `Some(vec![])` = resolves to nothing (a stress mark
    /// the vocabulary does not carry). Input is NFC-normalized first
    /// (rule 1).
    #[must_use]
    pub fn map_symbol(&mut self, sym: &str) -> Option<Vec<String>> {
        if let Some(cached) = self.cache.get(sym) {
            return cached.clone();
        }
        let nfc: String = sym.nfc().collect();
        let resolved = self.resolve(&nfc);
        self.cache.insert(sym.to_string(), resolved.clone());
        resolved
    }

    /// The ladder, on the memoized path's already-NFC'd input.
    fn resolve(&mut self, sym: &str) -> Option<Vec<String>> {
        // rules 1–2: identity, then homoglyph identity
        for candidate in [sym.to_string(), homoglyph_sub(sym)] {
            if self.target_vocab.contains(&candidate) {
                return Some(vec![candidate]);
            }
        }
        // rule 3: tie bars
        if sym.chars().any(|c| TIE_BARS.contains(&c)) {
            let stripped: String = sym.chars().filter(|c| !TIE_BARS.contains(c)).collect();
            if self.target_vocab.contains(&stripped) {
                return Some(vec![stripped]);
            }
            let mut out = Vec::new();
            let mut all = true;
            for segment in tie_segments(sym) {
                if let Some(mut parts) = self.resolve_segment(&segment) {
                    out.append(&mut parts);
                } else {
                    all = false;
                    break;
                }
            }
            if all {
                return Some(out);
            }
        }
        self.resolve_segment(sym)
    }

    /// The segment ladder (no tie bars inside): identity, homoglyph,
    /// length-mark split, diacritic strip (recursive), NFD base strip,
    /// stress handling, char split.
    fn resolve_segment(&mut self, seg: &str) -> Option<Vec<String>> {
        // rules 1–2 again at segment level
        for candidate in [seg.to_string(), homoglyph_sub(seg)] {
            if self.target_vocab.contains(&candidate) {
                return Some(vec![candidate]);
            }
        }
        // rule 4: trailing length mark splits off; the base resolves
        // recursively (e.g. `ɡ̥ː` strips U+0325 first, then splits)
        let last = seg.chars().next_back();
        if last.is_some_and(|c| LENGTH_MARKS.contains(&c)) {
            let base: String = seg.chars().take(seg.chars().count() - 1).collect();
            if let Some(mut parts) = self.resolve_segment(&base) {
                parts.push(last.unwrap().to_string());
                if parts.iter().all(|p| self.target_vocab.contains(p)) {
                    return Some(parts);
                }
            }
        }
        // rule 7: stress marks
        let chars: Vec<char> = seg.chars().collect();
        if chars.len() == 1 && STRESS_MARKS.contains(&chars[0]) {
            // a bare stress mark the vocabulary does not carry → emit
            // nothing (identity above already handled the carried case)
            return Some(Vec::new());
        }
        if chars.first().is_some_and(|c| STRESS_MARKS.contains(c)) {
            let mark = chars[0];
            let rest: String = chars[1..].iter().collect();
            if let Some(mut parts) = self.resolve_segment(&rest) {
                // the mark is kept only when the vocabulary carries it
                // (rule 7: deleted when absent); prepending preserves
                // prosody for languages whose bundles stress-prefix
                if self.target_vocab.contains(&mark.to_string()) {
                    parts.insert(0, mark.to_string());
                }
                return Some(parts);
            }
        }
        // rule 5: strip one strippable diacritic, recurse (offglide
        // breve first — it is the common case)
        if let Some(pos) = seg.chars().position(|c| STRIPPABLE_DIACRITICS.contains(&c)) {
            let stripped: String = seg
                .chars()
                .enumerate()
                .filter(|(i, _)| *i != pos)
                .map(|(_, c)| c)
                .collect();
            if let Some(parts) = self.resolve_segment(&stripped) {
                return Some(parts);
            }
        }
        // rule 6: NFD — precomposed letters to base + combining marks;
        // drop the combining marks and retry (ã→a, ä→a, ü→u, õ→o)
        let nfd: String = seg.nfd().filter(|c| !is_combining_mark(*c)).collect();
        if nfd != seg {
            if let Some(parts) = self.resolve_segment(&nfd) {
                return Some(parts);
            }
        }
        // rule 8: char split — every character must be a token
        if chars.len() > 1
            && chars
                .iter()
                .all(|c| self.target_vocab.contains(&c.to_string()))
        {
            return Some(chars.iter().map(std::string::ToString::to_string).collect());
        }
        None
    }
}

/// Split a symbol at its tie bars into segments (`p͡f` → `p`, `f`).
fn tie_segments(sym: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in sym.chars() {
        if TIE_BARS.contains(&c) {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// Locally available conversion assets for one model.
#[derive(Debug, Default, Clone)]
pub struct LexiconAssets {
    /// gruut source lexicon (`word\tph ph ph` per line).
    pub gruut_lexicon: Option<PathBuf>,
    /// aligned P2P pairs TSV (`gruut|ph\ttgt ph` per line).
    pub pairs: Option<PathBuf>,
}

/// The five uppercase diphthong symbols that distinguish the misaki
/// phoneme convention (kokoro-en, kitten) from espeak IPA (piper,
/// matcha-en) in a model's `tokens.txt`.
const MISAKI_SIGNATURE: [&str; 5] = ["A", "I", "O", "W", "Y"];

/// Detect the model's phoneme convention from its vocabulary: the
/// misaki convention uses the five uppercase diphthongs as standalone
/// symbols; their absence means espeak IPA.
fn detect_convention(vocab: &HashSet<String>) -> crate::sherpa_convention::Convention {
    use crate::sherpa_convention::Convention;
    if MISAKI_SIGNATURE.iter().all(|s| vocab.contains(*s)) {
        Convention::Misaki
    } else {
        Convention::Espeak
    }
}

/// An explicitly-set env var whose path doesn't exist — worth a
/// warning, or the silent fallthrough costs an afternoon of debugging.
fn env_path_or_warn(var: &str) -> Option<PathBuf> {
    let val = std::env::var_os(var)?;
    let p = PathBuf::from(&val);
    if p.exists() {
        Some(p)
    } else {
        eprintln!(
            "sherpa: {var} is set but {} does not exist — ignoring",
            p.display()
        );
        None
    }
}

/// voicegarden lexicon cache layouts that can hold the gruut English
/// lexicon, in preference order:
/// - `eng-US/` — the expanded v2 layout (what the local machine and
///   the CI download use)
/// - `en/` — the `lexicons.json` client layout (`fetch("en")`)
/// - `expanded/eng-US/` — the `expanded.json` client layout
const GRUUT_CACHE_LAYOUTS: [&str; 3] = ["eng-US", "en", "expanded/eng-US"];

/// Locate the cached gruut English lexicon across the known cache
/// layouts (see [`GRUUT_CACHE_LAYOUTS`]).
fn cached_gruut_lexicon() -> Option<PathBuf> {
    let cache = voicegarden_cache_dir()?;
    GRUUT_CACHE_LAYOUTS
        .iter()
        .map(|sub| cache.join(sub).join("lexicon.txt"))
        .find(|p| p.exists())
}

/// Discover conversion assets for an orphaned model: the `SHERPA_*` env
/// vars first, then conventional locations (pairs beside the model or
/// the gruut lexicon; the voicegarden lexicon cache).
#[must_use]
pub fn discover_lexicon_assets(model_dir: &Path, pairs_name: &str) -> LexiconAssets {
    let gruut = env_path_or_warn("SHERPA_GRUUT_LEXICON").or_else(cached_gruut_lexicon);

    let pairs = env_path_or_warn("SHERPA_P2P_PAIRS").or_else(|| {
        let in_model = model_dir.join(pairs_name);
        if in_model.exists() {
            return Some(in_model);
        }
        // beside whichever gruut lexicon was found, then any other
        // cache layout (the voicegarden release ships the pairs inside
        // every English bundle)
        let beside = gruut
            .as_ref()
            .and_then(|g| g.parent())
            .map(|d| d.join(pairs_name));
        if let Some(p) = beside.filter(|p| p.exists()) {
            return Some(p);
        }
        let cache = voicegarden_cache_dir()?;
        GRUUT_CACHE_LAYOUTS
            .iter()
            .map(|sub| cache.join(sub).join(pairs_name))
            .find(|p| p.exists())
    });

    LexiconAssets {
        gruut_lexicon: gruut,
        pairs,
    }
}

/// Discover conversion assets for an orphaned model, detecting the
/// convention from the model's own `tokens.txt` (uppercase diphthongs
/// `A I O W Y` ⇒ misaki pairs, else espeak pairs).
///
/// Cheap enough for per-load calls, but prefer calling it only after
/// [`ensure_generated_lexicon`]'s "already has a lexicon" check — see
/// the engine integration.
#[must_use]
pub fn discover_model_assets(model_dir: &Path) -> LexiconAssets {
    let pairs_name = detect_pairs_name(model_dir);
    discover_lexicon_assets(model_dir, &pairs_name)
}

/// Pairs filename for the model's phoneme convention (`p2p-misaki.tsv`
/// when the model vocabulary has the five uppercase misaki diphthongs,
/// else `p2p-espeak.tsv`).
#[must_use]
pub fn detect_pairs_name(model_dir: &Path) -> String {
    let convention = read_tokens_txt(&model_dir.join("tokens.txt")).map_or(
        // no tokens.txt / unreadable: misaki pairs are the ones
        // that exist today, so that's the useful default
        crate::sherpa_convention::Convention::Misaki,
        |vocab| detect_convention(&vocab),
    );
    match convention {
        crate::sherpa_convention::Convention::Misaki => "p2p-misaki.tsv".to_owned(),
        crate::sherpa_convention::Convention::Espeak => "p2p-espeak.tsv".to_owned(),
    }
}

/// Is this a lexicon file name sherpa collects (`lexicon.txt`,
/// `lexicon-*.txt`)?
#[must_use]
pub fn is_lexicon_file_name(name: &str) -> bool {
    name == "lexicon.txt" || name.starts_with("lexicon-")
}

/// True when the model dir already carries at least one lexicon file.
/// Cheap (one read_dir) — safe to call before any asset discovery.
#[must_use]
pub fn has_lexicon_file(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_str().is_some_and(is_lexicon_file_name))
    })
}

/// Generate a lexicon.txt for an espeak-orphaned model when the needed
/// assets are available. No-op (returns `None`) when the model already
/// has a lexicon, has no `tokens.txt`, or the assets are incomplete.
///
/// **Callers must gate this to orphaned English model families** — the
/// engine does (see `should_auto_generate_lexicon` there). This
/// function only checks file presence, not model identity.
///
/// **Convention detection**: from the model's own `tokens.txt` — the
/// misaki convention (kokoro-en, kitten) uses the five uppercase
/// diphthong symbols `A I O W Y`; their absence means the espeak
/// convention (piper, matcha-en). An empty/unparseable vocabulary is an
/// error, not a default.
///
/// **Pairs are required** — rules alone garble ~10% of common words
/// (word-specific stress/weak-form/allophone choices; see
/// [`crate::sherpa_p2p`]).
///
/// **Atomicity**: writes a unique `lexicon.txt.<pid>.<n>.tmp` sibling
/// first and renames — a crash mid-generation can't leave a truncated
/// `lexicon.txt` that poisons every later load, and concurrent
/// generation attempts (threads share a pid) don't share a temp path.
/// An output with zero entries is deleted and reported as an error for
/// the same reason.
///
/// Generation only runs for English models; non-English gruut languages
/// can be wired the same way once their pairs exist.
///
/// # Errors
/// Returns an error when assets were found but generation failed (I/O,
/// an unreadable vocabulary, or zero convertible entries); `Ok(None)`
/// means "nothing to do".
pub fn ensure_generated_lexicon(
    model_dir: &Path,
    assets: &LexiconAssets,
) -> Result<Option<crate::sherpa_convention::LexiconGenStats>, LexiconGenError> {
    // per-call counter for unique temp names — two threads in one
    // process share a pid, so the pid alone isn't enough
    static GEN_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    // already has a lexicon (lexicon.txt or lexicon-*.txt)? done.
    if has_lexicon_file(model_dir) {
        return Ok(None);
    }
    let tokens_path = model_dir.join("tokens.txt");
    if !tokens_path.exists() {
        return Ok(None);
    }
    // both assets required (see module docs)
    let (Some(gruut), Some(pairs)) = (&assets.gruut_lexicon, &assets.pairs) else {
        return Ok(None);
    };

    // detect the convention from the model's own vocabulary
    let vocab = read_tokens_txt(&tokens_path)?;
    if vocab.is_empty() {
        return Err(LexiconGenError(format!(
            "{} parsed to an empty vocabulary — refusing to generate a lexicon",
            tokens_path.display()
        )));
    }
    let convention = detect_convention(&vocab);

    let table = crate::sherpa_p2p::P2pTable::from_tsv(pairs)?;
    if table.is_empty() {
        // an empty table would silently downgrade generation to
        // rules-only, which garbles ~10% of common words — the exact
        // failure the pairs requirement exists to prevent
        return Err(LexiconGenError(format!(
            "pairs table {} is empty — refusing rules-only generation",
            pairs.display()
        )));
    }

    // atomic: write to a temp sibling, rename on success. The
    // pid+counter suffix keeps concurrent generation attempts from
    // interleaving writes to a shared temp path; the rename stays
    // same-directory, i.e. atomic.
    let n = GEN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = model_dir.join(format!("lexicon.txt.{}.{}.tmp", std::process::id(), n));
    let stats = crate::sherpa_convention::convert_lexicon_file(
        gruut,
        &tokens_path,
        convention,
        Some(&table),
        &tmp,
    );
    match stats {
        Ok(stats) if stats.written > 0 => {
            std::fs::rename(&tmp, model_dir.join("lexicon.txt")).map_err(|e| {
                LexiconGenError(format!("renaming into {}: {e}", model_dir.display()))
            })?;
            Ok(Some(stats))
        }
        Ok(stats) => {
            // zero entries: a wrong-language gruut lexicon or an
            // incompatible vocabulary — an empty lexicon.txt would
            // change engine behaviour forever (the "has lexicon" check
            // would pass on every later load). Remove and report.
            let _ = std::fs::remove_file(&tmp);
            Err(LexiconGenError(format!(
                "generation produced 0 entries (skipped {}) — wrong gruut lexicon or \
                 incompatible vocabulary? no lexicon written",
                stats.skipped
            )))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// voicegarden lexicon cache root (`~/.voicegarden/lexicons` by
/// default; `VOICEGARDEN_LEXICON_DIR` overrides, mirroring the crate),
/// when it exists.
fn voicegarden_cache_dir() -> Option<PathBuf> {
    let dir = if let Some(v) = std::env::var_os("VOICEGARDEN_LEXICON_DIR") {
        PathBuf::from(v)
    } else {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        PathBuf::from(home).join(".voicegarden").join("lexicons")
    };
    dir.exists().then_some(dir)
}

/// Bundle lexicon locations for one bundle key, in preference order:
/// the flat layout (`<key>/` — crate 0.3.0 caches BOTH archives flat)
/// and the newer `expanded/<key>/` layout. The bundle's `lexicon.txt`
/// is the plain-text form of the dictionary (word<TAB>phones).
fn bundle_lexicon_candidates(cache: &Path, key: &str) -> Vec<PathBuf> {
    vec![
        cache.join(key).join("lexicon.txt"),
        cache.join("expanded").join(key).join("lexicon.txt"),
    ]
}

/// Locate the cached bundle lexicon for a bundle key: the
/// `SHERPA_BUNDLE_LEXICON` env override first (warn when it points
/// nowhere), then the voicegarden cache layouts.
fn discover_bundle_lexicon(key: &str) -> Option<PathBuf> {
    if let Some(p) = env_path_or_warn("SHERPA_BUNDLE_LEXICON") {
        return Some(p);
    }
    let cache = voicegarden_cache_dir()?;
    bundle_lexicon_candidates(&cache, key)
        .into_iter()
        .find(|p| p.exists())
}

/// Outcome of a compat-lexicon generation: stats plus where the file
/// landed (the model dir, or the fallback dir when the model dir is not
/// writable — the config must point there explicitly).
#[derive(Debug)]
pub struct CompatLexiconResult {
    /// entries written by the converter (0 when an existing lexicon was
    /// reused — see [`CompatLexiconResult::reused`])
    pub written: usize,
    /// entries skipped (unmappable symbols)
    pub skipped: usize,
    /// final lexicon.txt location
    pub path: PathBuf,
    /// true when a lexicon already existed at the target and the full
    /// conversion was skipped. The engine re-runs this on every
    /// `synthesize` until the model dir has its own lexicon, so a
    /// read-only model dir (iOS bundles) must NOT re-convert 277k
    /// entries per utterance — the fallback-dir lexicon is reused and
    /// its path returned cheaply (the config needs it every call: it
    /// sits outside the model dir and is not auto-discovered).
    pub reused: bool,
}

/// Generate a sherpa `lexicon.txt` for a non-English model from its
/// language bundle (the `lexiconCompat: true` option, plan Phase 2).
/// Resolves the bundle via [`discover_bundle_lexicon`] and delegates to
/// [`ensure_compat_lexicon_from`]. Purely local: the bundle must already
/// be cached (fetched by provisioning, Phase 3) — no download happens
/// here. See [`ensure_compat_lexicon_from`] for the write-target and
/// atomicity contract.
///
/// # Errors
/// See [`ensure_compat_lexicon_from`].
pub fn ensure_compat_lexicon(
    model_dir: &Path,
    bundle_key: &str,
    fallback_root: &Path,
    model_id: &str,
) -> Result<Option<CompatLexiconResult>, LexiconGenError> {
    let Some(bundle_lexicon) = discover_bundle_lexicon(bundle_key) else {
        // an opted-in user must not get silence: say what is missing and
        // where the bundle is expected (fetching arrives with the
        // sherpa-g2p feature, plan Phase 3 — provisioning for now)
        eprintln!(
            "sherpa: lexiconCompat: bundle '{bundle_key}' not found in the lexicon cache — \
             skipped. Provision the bundle or set VOICEGARDEN_LEXICON_DIR."
        );
        return Ok(None);
    };
    ensure_compat_lexicon_from(model_dir, &bundle_lexicon, fallback_root, model_id)
}

/// [`ensure_compat_lexicon`] with an explicit bundle lexicon path (the
/// cache-layout resolution is the only difference — tests inject paths
/// directly instead of racing on env vars).
///
/// No-op (`Ok(None)`) when the model already has a lexicon or has no
/// `tokens.txt`. Write target: the model dir when writable; otherwise
/// `<fallback_root>/lexicons-gen/<model_id>/lexicon.txt` — the caller
/// must add a returned outside path to the model config (it is not
/// discovered automatically). Atomic like the English path: temp
/// sibling + rename; a zero-entry output is deleted and reported.
///
/// # Errors
/// Assets-found-but-conversion-failed (I/O, empty vocabulary, or zero
/// convertible entries).
pub fn ensure_compat_lexicon_from(
    model_dir: &Path,
    bundle_lexicon: &Path,
    fallback_root: &Path,
    model_id: &str,
) -> Result<Option<CompatLexiconResult>, LexiconGenError> {
    // per-call counter for unique temp names — see ensure_generated_lexicon
    static COMPAT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    if has_lexicon_file(model_dir) {
        return Ok(None);
    }
    let tokens_path = model_dir.join("tokens.txt");
    if !tokens_path.exists() {
        return Ok(None);
    }

    let mut map = VocabPhonemeMap::for_language(&tokens_path)?;

    // write into the model dir when possible, else the fallback dir
    // (a lexicon outside the model dir must be passed to the config
    // explicitly — it is not discovered automatically)
    let probe = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .truncate(false)
        .open(model_dir.join(".lexicon-write-probe"));
    let target_dir = if probe.is_ok() {
        let _ = std::fs::remove_file(model_dir.join(".lexicon-write-probe"));
        model_dir.to_path_buf()
    } else {
        let dir = fallback_root.join("lexicons-gen").join(model_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| LexiconGenError(format!("creating {}: {e}", dir.display())))?;
        dir
    };

    // the engine calls this on EVERY synthesize until the model dir has
    // its own lexicon: a read-only model dir lands here repeatedly, and
    // must reuse the fallback-dir lexicon instead of re-converting the
    // whole bundle per utterance
    let existing = target_dir.join("lexicon.txt");
    if existing.is_file() {
        return Ok(Some(CompatLexiconResult {
            written: 0,
            skipped: 0,
            path: existing,
            reused: true,
        }));
    }

    let n = COMPAT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = target_dir.join(format!("lexicon.txt.{}.{}.tmp", std::process::id(), n));
    let stats = generate_lexicon_txt(bundle_lexicon, &mut map, &tmp);
    match stats {
        Ok((written, skipped)) if written > 0 => {
            let final_path = target_dir.join("lexicon.txt");
            std::fs::rename(&tmp, &final_path).map_err(|e| {
                LexiconGenError(format!("renaming into {}: {e}", target_dir.display()))
            })?;
            Ok(Some(CompatLexiconResult {
                written,
                skipped,
                path: final_path,
                reused: false,
            }))
        }
        Ok((_written, skipped)) => {
            let _ = std::fs::remove_file(&tmp);
            Err(LexiconGenError(format!(
                "conversion produced 0 entries (skipped {skipped}) — bundle {} against {} \
                 has no convertible words; no lexicon written",
                bundle_lexicon.display(),
                tokens_path.display()
            )))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn read_tokens_txt(path: &Path) -> Result<HashSet<String>, LexiconGenError> {
    let file = std::fs::File::open(path)
        .map_err(|e| LexiconGenError(format!("reading {}: {e}", path.display())))?;
    let reader = std::io::BufReader::new(file);
    let mut vocab = HashSet::new();
    for line in reader.lines() {
        let line = line.map_err(|e| LexiconGenError(format!("reading {}: {e}", path.display())))?;
        // the format is "symbol<SPACE or TAB>id"; the symbol may itself
        // contain anything except the separator
        let sym = line
            .rsplit_once(' ')
            .or_else(|| line.rsplit_once('\t'))
            .map(|(s, _)| s);
        if let Some(sym) = sym {
            vocab.insert(sym.to_string());
        }
    }
    Ok(vocab)
}

/// A symbol mapper: one source phone symbol → a sequence of
/// target-vocabulary tokens. `None` = unmappable (skip the word);
/// empty = resolves to nothing (e.g. a stress mark the vocabulary does
/// not carry). Implemented by the English tables ([`PhonemeMap`]) and
/// the vocabulary-driven ladder ([`VocabPhonemeMap`]).
pub trait SymbolMapper {
    /// Map one phone symbol; see the trait docs for the contract.
    fn map_symbol(&mut self, sym: &str) -> Option<Vec<String>>;
}

impl SymbolMapper for PhonemeMap {
    fn map_symbol(&mut self, sym: &str) -> Option<Vec<String>> {
        self.map_phonemes(&[sym])
    }
}

impl SymbolMapper for VocabPhonemeMap {
    fn map_symbol(&mut self, sym: &str) -> Option<Vec<String>> {
        VocabPhonemeMap::map_symbol(self, sym)
    }
}

/// Convert a gruut lexicon file (`word ph1 ph2 ph3...` per line) into a
/// sherpa-onnx lexicon.txt, mapping phonemes through `map`.
///
/// Words with unmappable phonemes are skipped (sherpa's lexicon front-end
/// falls back to character-splitting for OOV words at runtime).
/// Returns (entries_written, entries_skipped).
///
/// # Errors
/// Returns an error if either file is unreadable or the output cannot be
/// written.
pub fn generate_lexicon_txt<M: SymbolMapper>(
    gruut_lexicon_path: &Path,
    map: &mut M,
    output_path: &Path,
) -> Result<(usize, usize), LexiconGenError> {
    let input = std::fs::File::open(gruut_lexicon_path)
        .map_err(|e| LexiconGenError(format!("reading {}: {e}", gruut_lexicon_path.display())))?;
    let mut reader = std::io::BufReader::new(input);

    let output = std::fs::File::create(output_path)
        .map_err(|e| LexiconGenError(format!("creating {}: {e}", output_path.display())))?;
    let mut writer = std::io::BufWriter::new(output);

    let mut written = 0usize;
    let mut skipped = 0usize;

    // read_line loop (not `lines().map_while(Result::ok)`): a mid-file
    // I/O or UTF-8 error must abort — same rule as convert_lexicon_file
    let mut buf = String::new();
    loop {
        buf.clear();
        if reader.read_line(&mut buf).map_err(|e| {
            LexiconGenError(format!("reading {}: {e}", gruut_lexicon_path.display()))
        })? == 0
        {
            break;
        }
        let line = buf.trim_end();
        let mut parts = line.split_whitespace();
        let Some(word) = parts.next() else { continue };
        let gruut_phones: Vec<&str> = parts.collect();
        if gruut_phones.is_empty() {
            continue;
        }
        let mut mapped: Vec<String> = Vec::with_capacity(gruut_phones.len() + 2);
        let mut ok = true;
        for sym in &gruut_phones {
            if let Some(parts) = map.map_symbol(sym) {
                mapped.extend(parts);
            } else {
                ok = false;
                break;
            }
        }
        if ok && !mapped.is_empty() {
            writeln!(writer, "{word}||{}", mapped.join(" "))
                .map_err(|e| LexiconGenError(format!("writing: {e}")))?;
            written += 1;
        } else {
            skipped += 1;
        }
    }

    writer
        .flush()
        .map_err(|e| LexiconGenError(format!("flushing: {e}")))?;
    Ok((written, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    /// Write a tokens.txt with `vocab` into its OWN temp dir (tests that
    /// share a fixed path race each other and flake by execution order).
    fn write_test_tokens(vocab: &[&str]) -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap().keep();
        let p = dir.join("tokens.txt");
        let mut s = String::new();
        for (i, sym) in vocab.iter().enumerate() {
            writeln!(s, "{sym} {i}").expect("in-memory write");
        }
        std::fs::write(&p, s).unwrap();
        p
    }

    fn english_vocab() -> Vec<&'static str> {
        let mut v: Vec<&str> = GRUUT_EN_IDENTITY.to_vec();
        // decomposition components
        for (_, a, b) in GRUUT_EN_DECOMP {
            v.push(a);
            v.push(b);
        }
        v.push(STRESS_PRIMARY);
        v.push(STRESS_SECONDARY);
        // punctuation / pad / bos / eos
        v.extend_from_slice(&[
            "_", "^", "$", "!", "'", "(", ")", ",", "-", ".", ":", ";", "?", " ",
        ]);
        v
    }

    #[test]
    fn identity_symbols_map_to_themselves() {
        let tokens = write_test_tokens(&english_vocab());
        let map = PhonemeMap::for_english(&tokens).unwrap();
        assert_eq!(map.source_symbol_count(), 40 + 14); // 40 base + 14 stressed variants
        let out = map.map_phonemes(&["h", "ɛ", "l", "oʊ"]).unwrap();
        assert_eq!(out, vec!["h", "ɛ", "l", "o", "ʊ"]);
    }

    #[test]
    fn diphthongs_decompose() {
        let tokens = write_test_tokens(&english_vocab());
        let map = PhonemeMap::for_english(&tokens).unwrap();
        // "my" → gruut: m aɪ → mapped: m a ɪ
        let out = map.map_phonemes(&["m", "aɪ"]).unwrap();
        assert_eq!(out, vec!["m", "a", "ɪ"]);
        // "choice" → gruut: t͡ʃ ɔɪ → mapped: t ʃ ɔ ɪ
        let out = map.map_phonemes(&["t͡ʃ", "ɔɪ"]).unwrap();
        assert_eq!(out, vec!["t", "ʃ", "ɔ", "ɪ"]);
    }

    #[test]
    fn stressed_variants_split_into_stress_plus_parts() {
        let tokens = write_test_tokens(&english_vocab());
        let map = PhonemeMap::for_english(&tokens).unwrap();
        // gruut "ˈaɪ" → "ˈ a ɪ"
        let out = map.map_phonemes(&["ˈaɪ"]).unwrap();
        assert_eq!(out, vec!["ˈ", "a", "ɪ"]);
        let out = map.map_phonemes(&["ˌeɪ"]).unwrap();
        assert_eq!(out, vec!["ˌ", "e", "ɪ"]);
    }

    #[test]
    fn unmappable_symbol_returns_none() {
        let tokens = write_test_tokens(&english_vocab());
        let map = PhonemeMap::for_english(&tokens).unwrap();
        // a symbol not in the mapping and not in the vocab
        // (combining vertical line below U+0335 — deliberately non-NFC)
        assert!(map.map_phonemes(&["x\u{0335}"]).is_none());
    }

    #[test]
    fn non_ipa_vocabulary_rejected() {
        // a vocabulary missing IPA symbols → clear error
        let tokens = write_test_tokens(&["a", "b", "c", "_", "^"]);
        let err = PhonemeMap::for_english(&tokens).unwrap_err();
        assert!(err.0.contains("identity symbols"));
    }

    #[test]
    fn lexicon_generation_round_trip() {
        let tokens = write_test_tokens(&english_vocab());
        let mut map = PhonemeMap::for_english(&tokens).unwrap();

        let dir = tokens.parent().unwrap();
        let gruut = dir.join("test_gruut.txt");
        std::fs::write(&gruut, "hello h ə l oʊ\nsky s k aɪ\nxyz ɑ b ϥ\n").unwrap();

        let out = dir.join("test_lexicon.txt");
        let (written, skipped) = generate_lexicon_txt(&gruut, &mut map, &out).unwrap();
        assert_eq!(written, 2);
        assert_eq!(skipped, 1);

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("hello||h ə l o ʊ"));
        assert!(content.contains("sky||s k a ɪ"));
        assert!(!content.contains("xyz"));
    }

    // ── ensure_generated_lexicon (engine auto-generation) ──

    /// Build a fake orphaned-model dir: tokens.txt + no lexicon.
    fn orphan_model_dir(vocab: &[&str]) -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap().keep();
        let mut s = String::new();
        for (i, sym) in vocab.iter().enumerate() {
            writeln!(s, "{sym} {i}").expect("in-memory write");
        }
        std::fs::write(dir.join("tokens.txt"), s).unwrap();
        dir
    }

    /// Full misaki-ish vocabulary + gruut + pairs in one temp tree.
    fn assets_tree() -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap().keep();
        let gruut_dir = dir.join("eng-US");
        std::fs::create_dir_all(&gruut_dir).unwrap();
        std::fs::write(gruut_dir.join("lexicon.txt"), "the ð ə\nday d ˈeɪ\n").unwrap();
        std::fs::write(gruut_dir.join("p2p-misaki.tsv"), "ð|ə\tð i\n").unwrap();
        dir
    }

    fn assets_for(tree: &std::path::Path) -> LexiconAssets {
        LexiconAssets {
            gruut_lexicon: Some(tree.join("eng-US").join("lexicon.txt")),
            pairs: Some(tree.join("eng-US").join("p2p-misaki.tsv")),
        }
    }

    #[test]
    fn ensure_generates_when_assets_present() {
        let assets = assets_for(&assets_tree());
        // misaki vocab (uppercase diphthongs)
        let model = orphan_model_dir(&["ð", "ə", "i", "d", "ˈ", "A", "I", "O", "W", "Y", "ʧ", "ʤ"]);

        let stats = ensure_generated_lexicon(&model, &assets)
            .unwrap()
            .expect("should generate");
        assert_eq!((stats.written, stats.skipped, stats.via_table), (2, 0, 1)); // "the" table, "day" rules
        assert_eq!(stats.rules(), 1);
        let content = std::fs::read_to_string(model.join("lexicon.txt")).unwrap();
        assert!(content.contains("the||ð i"));
        assert!(content.contains("day||d ˈ A"));
        assert!(!model.join("lexicon.txt.tmp").exists());
        // pid-suffixed temp variant also cleaned up
        assert!(
            !std::fs::read_dir(&model)
                .unwrap()
                .filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().ends_with(".tmp")),
            "a .tmp file survived generation"
        );
    }

    #[test]
    fn ensure_noop_when_lexicon_exists() {
        let assets = assets_for(&assets_tree());
        let model = orphan_model_dir(&["a", "b"]);
        std::fs::write(model.join("lexicon.txt"), "existing\n").unwrap();
        assert!(ensure_generated_lexicon(&model, &assets).unwrap().is_none());
        // unchanged
        assert_eq!(
            std::fs::read_to_string(model.join("lexicon.txt")).unwrap(),
            "existing\n"
        );
    }

    #[test]
    fn ensure_noop_without_assets() {
        let model = orphan_model_dir(&["a", "b"]);
        assert!(ensure_generated_lexicon(&model, &LexiconAssets::default())
            .unwrap()
            .is_none());
        assert!(!model.join("lexicon.txt").exists());
    }

    #[test]
    fn ensure_noop_without_pairs() {
        let tree = assets_tree();
        // gruut exists but pairs don't → no generation (rules alone garble)
        let assets = LexiconAssets {
            gruut_lexicon: Some(tree.join("eng-US/lexicon.txt")),
            pairs: None,
        };
        let model = orphan_model_dir(&["ð", "ə", "i", "A", "I", "O", "W", "Y"]);
        assert!(ensure_generated_lexicon(&model, &assets).unwrap().is_none());
        assert!(!model.join("lexicon.txt").exists());
    }

    #[test]
    fn zero_entry_generation_refused_and_cleaned_up() {
        let tree = assets_tree();
        // a gruut lexicon whose words nothing can convert (vocab without
        // any matching symbols) → written == 0 must NOT leave a lexicon
        let empty_out = tree.join("eng-US/empty.txt");
        // combining-mark symbols no vocabulary contains
        std::fs::write(&empty_out, "zzz x\u{0335} y\u{0335}\n").unwrap();
        let assets = LexiconAssets {
            gruut_lexicon: Some(empty_out),
            pairs: Some(tree.join("eng-US/p2p-misaki.tsv")),
        };
        let model = orphan_model_dir(&["ð", "ə", "i", "A", "I", "O", "W", "Y"]);
        let err = ensure_generated_lexicon(&model, &assets).unwrap_err();
        assert!(err.0.contains("0 entries"), "got: {err}");
        // neither the final file nor any temp remains (the temp name
        // carries pid + counter, so sweep for the suffix)
        assert!(!model.join("lexicon.txt").exists());
        assert!(
            !std::fs::read_dir(&model)
                .unwrap()
                .filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().ends_with(".tmp")),
            "a .tmp file survived the refused generation"
        );
    }

    #[test]
    fn empty_vocabulary_is_an_error() {
        let assets = assets_for(&assets_tree());
        // a tokens.txt whose lines don't parse → empty vocab, not a
        // silent espeak-convention default
        let model = tempfile::tempdir().unwrap().keep();
        std::fs::write(model.join("tokens.txt"), "\n\n").unwrap();
        assert!(ensure_generated_lexicon(&model, &assets).is_err());
        assert!(!model.join("lexicon.txt").exists());
    }

    #[test]
    fn pairs_name_follows_vocabulary_convention() {
        let misaki = orphan_model_dir(&["a", "A", "I", "O", "W", "Y"]);
        let espeak = orphan_model_dir(&["a", "e", "ɪ"]);
        assert_eq!(detect_pairs_name(&misaki), "p2p-misaki.tsv");
        assert_eq!(detect_pairs_name(&espeak), "p2p-espeak.tsv");
    }

    #[test]
    fn tokens_txt_tab_separator_parsed() {
        let model = tempfile::tempdir().unwrap().keep();
        std::fs::write(model.join("tokens.txt"), "a\t0\nb\t1\n").unwrap();
        let vocab = read_tokens_txt(&model.join("tokens.txt")).unwrap();
        assert!(
            vocab.contains("a") && vocab.contains("b"),
            "vocab: {vocab:?}"
        );
    }

    // ── VocabPhonemeMap: the vocabulary-driven ladder (#82) ──

    /// A thorsten-like vocabulary: the real de_DE-thorsten-medium
    /// tokens.txt is 152 single-character IPA symbols including `ː ɡ ˈ ˌ`
    /// — and NOT ASCII `g`, `pf`, `aʊ`, or `ts` (affricates/diphthongs
    /// must decompose). This synthetic set mirrors those facts.
    fn thorsten_vocab() -> std::path::PathBuf {
        let syms = [
            "_", "^", "$", " ", "!", "?", ",", ".", "\u{02C8}",
            "\u{02CC}", // stress IS carried
            "a", "e", "i", "o", "u", "ʏ", "y", "ø", "œ", "ɔ", "ɛ", "ə", "ɪ", "ʊ", "ɐ",
            "a", // vowels
            "b", "d", "f", "h", "j", "k", "l", "m", "n", "p", "s", "t", "v", "x",
            "z", // consonants
            "\u{0261}", "ʁ", "ʃ", "ʒ", "ç", "ŋ", "ɲ", "ʔ", "\u{02D0}", // ɡ (IPA), ː
        ];
        let dir = tempfile::tempdir().unwrap().keep();
        let mut s = String::new();
        for (i, sym) in syms.iter().enumerate() {
            writeln!(s, "{sym} {i}").expect("in-memory write");
        }
        std::fs::write(dir.join("tokens.txt"), s).unwrap();
        dir.join("tokens.txt")
    }

    /// The measured v0.3.1 German lexicon symbols that drive the ladder
    /// rules — one case per rule, counts in the plan's inventory table.
    #[test]
    fn vocab_ladder_resolves_german_symbol_inventory() {
        let tokens = thorsten_vocab();
        let mut map = VocabPhonemeMap::for_language(&tokens).unwrap();

        // rule 1: identity
        assert_eq!(map.map_symbol("haus-not-a-symbol").map(|_| ()), None); // sanity: unmappable
        assert_eq!(map.map_symbol("ʃ").unwrap(), vec!["ʃ"]);
        // rule 2: ASCII g → IPA ɡ (92,996 occurrences in deu)
        assert_eq!(map.map_symbol("g").unwrap(), vec!["\u{0261}"]);
        // rule 3: tie-split p͡f → p f (7,910 occurrences)
        assert_eq!(map.map_symbol("p\u{0361}f").unwrap(), vec!["p", "f"]);
        // rule 3 + 5 chained: t͡s̯ → t s (tie-split, then offglide strip)
        assert_eq!(
            map.map_symbol("t\u{0361}s\u{032F}").unwrap(),
            vec!["t", "s"]
        );
        // rule 4: length split uː → u ː (346,964 occurrences)
        assert_eq!(map.map_symbol("u\u{02D0}").unwrap(), vec!["u", "\u{02D0}"]);
        // rule 4 + 5: ɡ̥ː → ɡ ː (strip voiceless mark, then split)
        assert_eq!(
            map.map_symbol("\u{0261}\u{0325}\u{02D0}").unwrap(),
            vec!["\u{0261}", "\u{02D0}"]
        );
        // rule 5: offglide breve — aʊ̯ tries aʊ (not a token), char-splits
        assert_eq!(
            map.map_symbol("a\u{028A}\u{032F}").unwrap(),
            vec!["a", "\u{028A}"]
        );
        // rule 6: NFD — precomposed ãː / ä / ü / õ strip to base + marks
        assert_eq!(
            map.map_symbol("\u{00E3}\u{02D0}").unwrap(),
            vec!["a", "\u{02D0}"]
        );
        assert_eq!(map.map_symbol("\u{00E4}").unwrap(), vec!["a"]);
        assert_eq!(map.map_symbol("\u{00FC}").unwrap(), vec!["u"]);
        assert_eq!(
            map.map_symbol("\u{00F5}\u{02D0}").unwrap(),
            vec!["o", "\u{02D0}"]
        );
        // rule 7: stress IS in this vocab → kept; bare stress in a
        // vocab without it → dropped (checked in the next test)
        assert_eq!(map.map_symbol("\u{02C8}").unwrap(), vec!["\u{02C8}"]);
        // rule 8 / unmappable: ɝ (English rhotic, the one deu loanword)
        assert!(map.map_symbol("\u{025D}").is_none());
        // memoization: repeated symbols resolve consistently
        assert_eq!(map.map_symbol("g").unwrap(), vec!["\u{0261}"]);
    }

    /// The probe sentence shape: a full word from the bundle converts.
    #[test]
    fn vocab_ladder_maps_a_bundle_word() {
        let tokens = thorsten_vocab();
        let mut map = VocabPhonemeMap::for_language(&tokens).unwrap();
        // haus → h aʊ̯ s  ⇒  h a ʊ s
        let seq: Vec<String> = ["h", "a\u{028A}\u{032F}", "s"]
            .iter()
            .flat_map(|s| map.map_symbol(s).unwrap())
            .collect();
        assert_eq!(seq, vec!["h", "a", "\u{028A}", "s"]);
    }

    /// Stress marks absent from the vocabulary are dropped, not fatal;
    /// when the vocabulary carries them, stress-prefixed composites
    /// KEEP the mark (rule 7 — prosody survives for bundle languages
    /// that stress-prefix).
    #[test]
    fn vocab_ladder_drops_uncarried_stress_marks() {
        let dir = tempfile::tempdir().unwrap().keep();
        std::fs::write(dir.join("tokens.txt"), "a 0\nb 1\n").unwrap();
        let mut map = VocabPhonemeMap::for_language(&dir.join("tokens.txt")).unwrap();
        assert_eq!(map.map_symbol("\u{02C8}"), Some(Vec::new()));
        assert_eq!(map.map_symbol("\u{02C8}a"), Some(vec!["a".to_string()]));

        // vocab WITH the stress mark: bare identity, prefix kept
        let dir = tempfile::tempdir().unwrap().keep();
        std::fs::write(dir.join("tokens.txt"), "a 0\n\u{02C8} 1\n").unwrap();
        let mut map = VocabPhonemeMap::for_language(&dir.join("tokens.txt")).unwrap();
        assert_eq!(
            map.map_symbol("\u{02C8}"),
            Some(vec!["\u{02C8}".to_string()])
        );
        assert_eq!(
            map.map_symbol("\u{02C8}a"),
            Some(vec!["\u{02C8}".to_string(), "a".to_string()])
        );
    }

    /// A vocabulary carrying the tie-stripped whole (`pf` as a token)
    /// prefers it over the split.
    #[test]
    fn vocab_ladder_prefers_tie_stripped_whole_token() {
        let dir = tempfile::tempdir().unwrap().keep();
        std::fs::write(dir.join("tokens.txt"), "p 0\nf 1\npf 2\n").unwrap();
        let mut map = VocabPhonemeMap::for_language(&dir.join("tokens.txt")).unwrap();
        assert_eq!(
            map.map_symbol("p\u{0361}f").unwrap(),
            vec!["pf".to_string()]
        );
    }

    /// Empty vocabulary refuses construction (same rule as English).
    #[test]
    fn vocab_ladder_refuses_empty_vocabulary() {
        let dir = tempfile::tempdir().unwrap().keep();
        std::fs::write(dir.join("tokens.txt"), "\n\n").unwrap();
        assert!(VocabPhonemeMap::for_language(&dir.join("tokens.txt")).is_err());
    }

    /// Bundle lexicon conversion end-to-end on a mini file: written
    /// entries carry `word||mapped phones`, unmappable words are skipped.
    #[test]
    fn bundle_lexicon_conversion_end_to_end() {
        let tokens = thorsten_vocab();
        let dir = tokens.parent().unwrap();
        let bundle = dir.join("mini-bundle.txt");
        std::fs::write(
            &bundle,
            // word<TAB>phones — the bundle format (tab separator)
            "haus\th a\u{028A}\u{032F} s\n\
             gummi\t\u{0261} \u{028A} m i\u{02D0}\n\
             suessreserve\ts y\u{02D0} s \u{025D} e\u{02D0} v \u{0259}\n",
        )
        .unwrap();
        let out = dir.join("out-lexicon.txt");
        let mut map = VocabPhonemeMap::for_language(&tokens).unwrap();
        let (written, skipped) = generate_lexicon_txt(&bundle, &mut map, &out).unwrap();
        assert_eq!((written, skipped), (2, 1)); // ɝ word skipped
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("haus||h a \u{028A} s"));
        assert!(!content.contains("suessreserve"));
    }

    // ── ensure_compat_lexicon (#82 Phase 2) ──

    /// Generation into a writable model dir + the already-has-lexicon
    /// no-op. Uses ensure_compat_lexicon_from with an explicit bundle
    /// path — the env-var discovery layer would race across parallel
    /// tests.
    #[test]
    fn compat_lexicon_generates_into_model_dir() {
        let tmp = tempfile::tempdir().unwrap().keep();
        let bundle = tmp.join("bundle-de.txt");
        std::fs::write(
            &bundle,
            "haus\th a\u{028A}\u{032F} s\ngummi\t\u{0261} \u{028A} m i\u{02D0}\n",
        )
        .unwrap();
        let model = tmp.join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::copy(thorsten_vocab(), model.join("tokens.txt")).unwrap();

        let res = ensure_compat_lexicon_from(&model, &bundle, &tmp, "test-model")
            .unwrap()
            .expect("generated");
        assert_eq!((res.written, res.skipped), (2, 0));
        assert_eq!(res.path, model.join("lexicon.txt"));
        let content = std::fs::read_to_string(&res.path).unwrap();
        assert!(content.contains("haus||h a \u{028A} s"));
        // no temp litter
        assert!(!std::fs::read_dir(&model)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp")));
        // second call: already has a lexicon → no-op
        assert!(
            ensure_compat_lexicon_from(&model, &bundle, &tmp, "test-model")
                .unwrap()
                .is_none()
        );
    }

    /// The cache layouts resolve in preference order: flat `<key>/`
    /// (crate 0.3.0 caches BOTH archives flat) then `expanded/<key>/`.
    #[test]
    fn bundle_lexicon_candidates_prefer_flat_layout() {
        let cache = std::path::Path::new("/cache");
        let cands = bundle_lexicon_candidates(cache, "deu");
        assert_eq!(cands[0], cache.join("deu").join("lexicon.txt"));
        assert_eq!(
            cands[1],
            cache.join("expanded").join("deu").join("lexicon.txt")
        );
    }

    /// Read-only model dir → the fallback dir under the provided root,
    /// and the returned path is outside the model dir (the caller must
    /// pass it to the config). Skipped when running as root (root
    /// ignores directory permissions).
    #[test]
    fn compat_lexicon_falls_back_when_model_dir_readonly() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap().keep();
        let model = tmp.join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::copy(thorsten_vocab(), model.join("tokens.txt")).unwrap();
        let bundle = tmp.join("bundle-de.txt");
        std::fs::write(&bundle, "haus\th a\u{028A}\u{032F} s\n").unwrap();

        std::fs::set_permissions(&model, std::fs::Permissions::from_mode(0o555)).unwrap();
        // root bypasses mode bits: detect and skip
        let probe = model.join(".write-probe");
        let is_root = std::fs::write(&probe, b"").is_ok();
        let _ = std::fs::remove_file(&probe);
        let res = if is_root {
            None
        } else {
            ensure_compat_lexicon_from(&model, &bundle, &tmp, "m").unwrap()
        };
        std::fs::set_permissions(&model, std::fs::Permissions::from_mode(0o755)).unwrap();
        let Some(res) = res else { return };
        assert!(!res.path.starts_with(&model), "must land outside");
        assert_eq!(
            res.path,
            tmp.join("lexicons-gen").join("m").join("lexicon.txt")
        );
        assert!(res.path.is_file());
    }

    /// THE iOS scenario: a read-only model dir must reuse the
    /// fallback-dir lexicon on subsequent calls — the engine re-runs
    /// this per synthesize, and re-converting 277k entries per
    /// utterance would be a multi-second tax. Skipped as root.
    #[test]
    fn compat_lexicon_reuses_fallback_on_second_call() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap().keep();
        let model = tmp.join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::copy(thorsten_vocab(), model.join("tokens.txt")).unwrap();
        let bundle = tmp.join("bundle-de.txt");
        std::fs::write(&bundle, "haus\th a\u{028A}\u{032F} s\n").unwrap();

        std::fs::set_permissions(&model, std::fs::Permissions::from_mode(0o555)).unwrap();
        let probe = model.join(".write-probe");
        let is_root = std::fs::write(&probe, b"").is_ok();
        let _ = std::fs::remove_file(&probe);
        if !is_root {
            let first = ensure_compat_lexicon_from(&model, &bundle, &tmp, "m")
                .unwrap()
                .expect("generated");
            assert!(!first.reused);
            assert_eq!(first.written, 1);

            let second = ensure_compat_lexicon_from(&model, &bundle, &tmp, "m")
                .unwrap()
                .expect("reused");
            assert!(second.reused, "must not re-convert");
            assert_eq!(second.written, 0);
            assert_eq!(second.path, first.path, "same fallback path");
        }
        std::fs::set_permissions(&model, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A bundle whose nothing converts (wrong-language) → zero-entry
    /// refusal, nothing written, temp cleaned.
    #[test]
    fn compat_lexicon_refuses_zero_entry_output() {
        let tmp = tempfile::tempdir().unwrap().keep();
        let bundle = tmp.join("bundle-wrong.txt");
        std::fs::write(&bundle, "zzz x\u{025D} y\u{025D}\n").unwrap();
        let model = tmp.join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::copy(thorsten_vocab(), model.join("tokens.txt")).unwrap();

        let err = ensure_compat_lexicon_from(&model, &bundle, &tmp, "m").unwrap_err();
        assert!(err.0.contains("0 entries"), "got: {err}");
        assert!(!model.join("lexicon.txt").exists());
        assert!(!std::fs::read_dir(&model)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp")));
    }

    // ── Real-bundle coverage (the probe's rates, pinned) ──
    //
    // The macOS probe converted all 277,912 base entries; the prototype
    // ladder resolves de 277,912/0 and deu 333,944/1 (süßreserve carries
    // ɝ). These tests re-run the FULL bundles against the real thorsten
    // vocabulary:
    //
    //   cargo test --features sherpaonnx --lib -- --ignored sherpa_lexicon \
    //     SHERPA_TEST_THORSTEN_TOKENS=/path/tokens.txt \
    //     SHERPA_TEST_DE_LEXICON=.../de/lexicon.txt \
    //     SHERPA_TEST_DEU_LEXICON=.../deu/lexicon.txt
    //
    // CI provisions the files in Phase 4 (plan: both bundles + model).

    fn env_file(var: &str) -> Option<std::path::PathBuf> {
        std::env::var_os(var)
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_file())
    }

    #[test]
    #[ignore = "needs the real thorsten tokens.txt + base de bundle (env: SHERPA_TEST_THORSTEN_TOKENS, SHERPA_TEST_DE_LEXICON)"]
    fn real_coverage_base_de_bundle() {
        let (Some(tokens), Some(bundle)) = (
            env_file("SHERPA_TEST_THORSTEN_TOKENS"),
            env_file("SHERPA_TEST_DE_LEXICON"),
        ) else {
            eprintln!("skipping: env vars not set");
            return;
        };
        let out = tempfile::tempdir().unwrap().keep().join("lexicon.txt");
        let mut map = VocabPhonemeMap::for_language(&tokens).unwrap();
        let (written, skipped) = generate_lexicon_txt(&bundle, &mut map, &out).unwrap();
        assert_eq!((written, skipped), (277_912, 0), "the probe's rate, exact");
    }

    #[test]
    #[ignore = "needs the real thorsten tokens.txt + expanded deu bundle (env: SHERPA_TEST_THORSTEN_TOKENS, SHERPA_TEST_DEU_LEXICON)"]
    fn real_coverage_expanded_deu_bundle() {
        let (Some(tokens), Some(bundle)) = (
            env_file("SHERPA_TEST_THORSTEN_TOKENS"),
            env_file("SHERPA_TEST_DEU_LEXICON"),
        ) else {
            eprintln!("skipping: env vars not set");
            return;
        };
        let out = tempfile::tempdir().unwrap().keep().join("lexicon.txt");
        let mut map = VocabPhonemeMap::for_language(&tokens).unwrap();
        let (written, skipped) = generate_lexicon_txt(&bundle, &mut map, &out).unwrap();
        // süßreserve carries ɝ — the single unmappable entry
        assert_eq!((written, skipped), (333_944, 1));
    }
}
