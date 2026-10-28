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
//! The gruut English phoneme inventory is IPA-based with:
//! - 33 single-character symbols that map identity to piper/kokoro/kitten
//!   vocabularies (consonants, monophthongs, syllabics)
//! - 7 multi-character symbols (diphthongs `aɪ aʊ eɪ oʊ ɔɪ`, affricates
//!   `t͡ʃ d͡ʒ`) that decompose into their component characters — every
//!   component is in every observed model vocabulary
//! - stress-prefixed variants (`ˈaɪ`, `ˌeɪ`, …) that split into the
//!   stress mark + the decomposed parts
//!
//! Non-English gruut languages (de, fr, es, …) follow the same pattern:
//! the generator reads the target model's `tokens.txt` and only emits
//! words whose phonemes are fully in-vocabulary after mapping, so it
//! works for any language where the gruut inventory ⊆ model inventory
//! (or decomposes into it).

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

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

/// voicegarden lexicon cache root (`~/.voicegarden/lexicons`), when it
/// exists.
fn voicegarden_cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let dir = PathBuf::from(home).join(".voicegarden").join("lexicons");
    dir.exists().then_some(dir)
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
pub fn generate_lexicon_txt(
    gruut_lexicon_path: &Path,
    map: &PhonemeMap,
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
        match map.map_phonemes(&gruut_phones) {
            Some(mapped) => {
                writeln!(writer, "{word}||{}", mapped.join(" "))
                    .map_err(|e| LexiconGenError(format!("writing: {e}")))?;
                written += 1;
            }
            None => skipped += 1,
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
        let map = PhonemeMap::for_english(&tokens).unwrap();

        let dir = tokens.parent().unwrap();
        let gruut = dir.join("test_gruut.txt");
        std::fs::write(&gruut, "hello h ə l oʊ\nsky s k aɪ\nxyz ɑ b ϥ\n").unwrap();

        let out = dir.join("test_lexicon.txt");
        let (written, skipped) = generate_lexicon_txt(&gruut, &map, &out).unwrap();
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
}
