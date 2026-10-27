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
use std::path::Path;

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
    "b", "d", "f", "h", "i", "j", "k", "l", "m", "n", "p", "s", "t", "u", "v", "w", "z", // consonants
    "æ", "ð", "ŋ", "ɑ", "ɔ", "ə", "ɚ", "ɛ", "ɡ", "ɪ", "ɹ", "ʃ", "ʊ", "ʌ", "ʒ", "θ", // vowels & fricatives
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
/// Returns an error if the file is unreadable.
pub fn read_tokens_txt_public(path: &Path) -> Result<HashSet<String>, LexiconGenError> {
    read_tokens_txt(path)
}

fn read_tokens_txt(path: &Path) -> Result<HashSet<String>, LexiconGenError> {
    let file = std::fs::File::open(path)
        .map_err(|e| LexiconGenError(format!("reading {}: {e}", path.display())))?;
    let reader = std::io::BufReader::new(file);
    let mut vocab = HashSet::new();
    for line in reader.lines().map_while(Result::ok) {
        // the format is "symbol<TAB or SPACE>id"
        if let Some(sym) = line.rsplit_once(' ').map(|(s, _)| s) {
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
    let reader = std::io::BufReader::new(input);

    let output = std::fs::File::create(output_path)
        .map_err(|e| LexiconGenError(format!("creating {}: {e}", output_path.display())))?;
    let mut writer = std::io::BufWriter::new(output);

    let mut written = 0usize;
    let mut skipped = 0usize;

    for line in reader.lines().map_while(Result::ok) {
        let mut parts = line.split_whitespace();
        let Some(word) = parts.next() else { continue };
        let gruut_phones: Vec<&str> = parts.collect();
        if gruut_phones.is_empty() {
            continue;
        }
        match map.map_phonemes(&gruut_phones) {
            Some(mapped) => {
                writeln!(writer, "{word} {}", mapped.join(" "))
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

    fn write_test_tokens(vocab: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("sherpa_lexicon_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tokens.txt");
        let mut s = String::new();
        for (i, sym) in vocab.iter().enumerate() {
            s.push_str(&format!("{sym} {i}\n"));
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
        v.extend_from_slice(&["_", "^", "$", "!", "'", "(", ")", ",", "-", ".", ":", ";", "?", " "]);
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
        assert!(map.map_phonemes(&["x́"]).is_none());
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
        assert!(content.contains("hello h ə l o ʊ"));
        assert!(content.contains("sky s k a ɪ"));
        assert!(!content.contains("xyz"));
    }
}
