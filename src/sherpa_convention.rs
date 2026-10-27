//! Phonological convention conversion: gruut (MIT) → model-native
//! conventions, entirely via rules — no GPL-phonemizer data.
//!
//! sherpa-onnx 2.x removed the bundled espeak-ng phonemizer; models whose
//! only front-end was espeak-ng-data need a lexicon in the convention
//! their training phonemizer produced. This module converts gruut
//! lexicon entries (MIT, from voicegarden-lexicons) into those
//! conventions using two layers:
//!
//! 1. **Symbol substitution** (mechanical): each gruut source symbol maps
//!    to the target convention's encoding of the same sound — e.g.
//!    gruut `eɪ` becomes misaki `A`, or stays `e ɪ` in espeak.
//! 2. **Phonological rules** (context-dependent): function-word
//!    destressing, rhotic splitting (`ɚ` → `ɜ ɹ` / `ə ɹ`), and
//!    unstressed vowel reduction — derived by comparing gruut output
//!    against each model family's own bundled lexicon.
//!
//! Two target conventions are supported:
//! - **misaki** (kokoro-en, kitten): five uppercase diphthong symbols
//!   (`A I O W Y`), single-character affricate ligatures (`ʧ ʤ`),
//!   split rhotics.
//! - **espeak** (piper, matcha-en): standard IPA symbols throughout
//!   (same character inventory as gruut for most sounds), function-word
//!   destressing, split rhotics.
//!
//! Both are applied to the MIT gruut lexicon; no espeak-ng or other GPL
//! phonemizer is invoked at build or runtime.

use std::io::{BufRead, Write};

use crate::sherpa_lexicon::LexiconGenError;

// ──────────────────────────────────────────────────────────────────
// Symbol substitution tables
// ──────────────────────────────────────────────────────────────────

/// gruut → misaki symbol mapping: the five diphthong shorthands and the
/// two affricate ligatures. All other gruut symbols map identity (the
/// misaki convention uses standard IPA for consonants and monophthongs).
///
/// Derived by co-occurrence analysis: for words gruut transcribes with
/// `eɪ`, the kokoro bundled lexicon always uses `A`, etc. (see
/// SHERPA2_TODO.md Phase 2 findings for the word-level evidence).
const MISAKI_SYMBOL_MAP: &[(&str, &str)] = &[
    ("eɪ", "A"),
    ("aɪ", "I"),
    ("oʊ", "O"),
    ("aʊ", "W"),
    ("ɔɪ", "Y"),
    ("t͡ʃ", "ʧ"),
    ("d͡ʒ", "ʤ"),
];

/// gruut → espeak symbol mapping: standard IPA throughout, but gruut's
/// multi-character diphthongs/affricates decompose into component IPA
/// characters (espeak emits them separately in its phoneme stream).
const ESPEAK_SYMBOL_MAP: &[(&str, &str, &str)] = &[
    ("eɪ", "e", "ɪ"),
    ("aɪ", "a", "ɪ"),
    ("oʊ", "o", "ʊ"),
    ("aʊ", "a", "ʊ"),
    ("ɔɪ", "ɔ", "ɪ"),
    ("t͡ʃ", "t", "ʃ"),
    ("d͡ʒ", "d", "ʒ"),
];

// ──────────────────────────────────────────────────────────────────
// Phonological rules
// ──────────────────────────────────────────────────────────────────

/// English function words that lose their stress mark in connected
/// speech. gruut marks primary stress on all monosyllabic words; the
/// misaki/espeak conventions only stress content words.
///
/// Derived by comparing gruut vs kokoro-multi-lang transcriptions of
/// the 200 most common English words (words where gruut has `ˈ` but
/// the target doesn't).
const DESTRESS_WORDS: &[&str] = &[
    "the", "be", "to", "of", "a", "in", "that", "have", "it", "for", "on", "with", "he", "as",
    "you", "at", "this", "but", "his", "by", "from", "they", "we", "she", "or", "an", "will",
    "would", "could", "should", "was", "were", "been", "has", "had", "them", "then", "these",
    "its", "us", "are", "is", "do", "does", "did", "not", "and", "but", "if", "so", "up", "out",
    "who", "what", "when", "where", "why", "how", "than", "then", "there", "here", "now",
];

/// Weak forms for function words: the full vowel reduces to schwa (or
/// another reduced vowel) when unstressed. gruut uses the full form;
/// misaki/espeak use the reduced form for these words.
///
/// These are the standard English weak forms — grammar facts, not
/// phonemizer-specific data.
const WEAK_FORMS: &[(&str, &[&str])] = &[
    // (word, gruut full form, target weak form — shown as the gruut tokens to REPLACE)
    // "the": gruut ð ə → stays ð ə (already schwa in gruut)
    // "to": gruut t ə → stays t ə (already schwa)
    // "a": gruut ə → stays ə
    // Most function words already have schwa in gruut; the main
    // difference is just the stress removal, not vowel changes.
    // The few words where gruut uses a full vowel:
    ("of", &["ə", "v"]),             // gruut sometimes ʌ v → target ə v
    ("and", &["ə", "n"]),            // gruut ˈæ n d → target ə n (weak)
    ("can", &["k", "ə", "n"]),       // gruut k ə n → target k ə n (already)
    ("from", &["f", "ɹ", "ə", "m"]), // gruut f ɹ ˈʌ m → target f ɹ ə m
    ("was", &["w", "ə", "z"]),       // gruut w ə z → target w ʌ z (check!)
];

// ──────────────────────────────────────────────────────────────────
// Conversion engine
// ──────────────────────────────────────────────────────────────────

/// The target phoneme convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Convention {
    /// kokoro-en, kitten: uppercase diphthongs, ligature affricates
    Misaki,
    /// piper, matcha-en: standard IPA, decomposed diphthongs
    Espeak,
}

/// Convert one gruut lexicon entry (word + phoneme tokens) to the target
/// convention. Returns the converted phoneme tokens, or `None` if any
/// symbol is unmappable.
///
/// Applies, in order:
/// 1. Function-word destressing (remove `ˈ` from known function words)
/// 2. Rhotic splitting (`ɚ` → `ɜ ɹ` stressed / `ə ɹ` unstressed)
/// 3. Symbol substitution (diphthong/affricate encoding per convention)
/// 4. Unstressed vowel reduction (`ɛ` → `ə` in unstressed syllables,
///    espeak convention only)
#[must_use]
#[allow(clippy::implicit_hasher)] // callers hold plain HashSets from read_tokens_txt
pub fn convert_entry(
    word: &str,
    gruut_tokens: &[&str],
    convention: Convention,
    target_vocab: &std::collections::HashSet<String>,
) -> Option<Vec<String>> {
    let mut tokens: Vec<String> = gruut_tokens.iter().map(ToString::to_string).collect();

    // 1. function-word destressing
    if DESTRESS_WORDS.contains(&word) {
        // gruut emits stress+vowel as one token ("ˈæ"); strip the prefix
        // from combined tokens, and remove standalone stress marks
        tokens = tokens
            .into_iter()
            .filter_map(|t| {
                let s = t.as_str();
                if s == "\u{02c8}" || s == "\u{02cc}" {
                    None
                } else if let Some(rest) = s.strip_prefix('\u{02c8}') {
                    Some(rest.to_string())
                } else if let Some(rest) = s.strip_prefix('\u{02cc}') {
                    Some(rest.to_string())
                } else {
                    Some(t)
                }
            })
            .collect();
    }

    // 2. rhotic splitting: ɚ → ɜ ɹ (in a stressed syllable) or ə ɹ
    let has_stress = tokens.iter().any(|t| t == "ˈ" || t == "ˌ");
    tokens = split_rhotics(tokens, has_stress);

    // 3. symbol substitution
    tokens = substitute_symbols(tokens, convention);

    // 4. unstressed vowel reduction (both conventions — gruut uses full
    //    vowels where both misaki and espeak reduce to schwa)
    tokens = reduce_unstressed_vowels(tokens);

    // verify all output symbols are in the target vocabulary
    // gruut emits stress+vowel as one token ("ˈʌ"); the target vocab has
    // them as separate entries — split before checking
    let mut flat: Vec<String> = Vec::with_capacity(tokens.len() + 4);
    for tok in tokens {
        if let Some(rest) = tok.strip_prefix('\u{02c8}') {
            flat.push("\u{02c8}".to_string());
            if !rest.is_empty() {
                flat.push(rest.to_string());
            }
        } else if let Some(rest) = tok.strip_prefix('\u{02cc}') {
            flat.push("\u{02cc}".to_string());
            if !rest.is_empty() {
                flat.push(rest.to_string());
            }
        } else {
            flat.push(tok);
        }
    }
    if flat.iter().all(|t| target_vocab.contains(t.as_str())) {
        Some(flat)
    } else {
        None
    }
}

/// Split rhotacized vowels: `ɚ` becomes `ɜ ɹ` when it carries the
/// syllabic nucleus of a stressed syllable, or `ə ɹ` elsewhere.
fn split_rhotics(tokens: Vec<String>, _has_stress: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len() + 4);
    for tok in tokens {
        if tok == "ɚ" {
            // stressed ˈɚ → ˈ ɜ ɹ; bare ɚ → ə ɹ
            if let Some(last) = out.last() {
                if last == "ˈ" {
                    out.pop();
                    out.push("ˈ".to_string());
                    out.push("ɜ".to_string());
                    out.push("ɹ".to_string());
                    continue;
                }
            }
            out.push("ə".to_string());
            out.push("ɹ".to_string());
        } else if tok == "ˈɚ" {
            out.push("ˈ".to_string());
            out.push("ɜ".to_string());
            out.push("ɹ".to_string());
        } else if tok == "ˌɚ" {
            out.push("ˌ".to_string());
            out.push("ə".to_string());
            out.push("ɹ".to_string());
        } else {
            out.push(tok);
        }
    }
    out
}

/// Substitute multi-character gruut symbols for the target convention.
fn substitute_symbols(tokens: Vec<String>, convention: Convention) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len() + 4);
    for tok in tokens {
        // gruut lexicon format emits stress + diphthong as one token
        // (e.g. "ˈeɪ"); split the stress prefix before substituting
        let (stress, base) = if let Some(rest) = tok.strip_prefix('\u{02c8}') {
            (Some("\u{02c8}"), rest)
        } else if let Some(rest) = tok.strip_prefix('\u{02cc}') {
            (Some("\u{02cc}"), rest)
        } else {
            (None, tok.as_str())
        };

        let mut matched = false;
        match convention {
            Convention::Misaki => {
                for (src, dst) in MISAKI_SYMBOL_MAP {
                    if base == *src {
                        if let Some(s) = stress {
                            out.push(s.to_string());
                        }
                        out.push((*dst).to_string());
                        matched = true;
                        break;
                    }
                }
            }
            Convention::Espeak => {
                for (src, a, b) in ESPEAK_SYMBOL_MAP {
                    if base == *src {
                        if let Some(s) = stress {
                            out.push(s.to_string());
                        }
                        out.push((*a).to_string());
                        out.push((*b).to_string());
                        matched = true;
                        break;
                    }
                }
            }
        }
        if !matched {
            out.push(tok);
        }
    }
    out
}

/// Reduce unstressed vowels: gruut uses full vowel quality (ɛ, ʌ, ɪ)
/// in unstressed syllables; the target conventions reduce most to `ə`.
///
/// This applies to every syllable that lacks a stress mark — gruut marks
/// stress explicitly, so any vowel NOT in a stress-marked syllable is
/// fair game for reduction. `ɪ` is kept (both conventions preserve it
/// in many unstressed positions); `ɛ` and `ʌ` are reduced to `ə`.
fn reduce_unstressed_vowels(tokens: Vec<String>) -> Vec<String> {
    // syllable boundaries: a new syllable starts after each vowel (or
    // the beginning). A syllable is stressed if a stress mark appears
    // within it. We reduce ɛ→ə and ʌ→ə in unstressed syllables.
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut in_stressed_syllable = false;
    let mut seen_vowel_in_syllable = false;

    for tok in tokens {
        let is_stress = tok == "\u{02c8}" || tok == "\u{02cc}";
        let is_vowel = matches!(
            tok.as_str(),
            "æ" | "ɑ"
                | "ɔ"
                | "ə"
                | "ɛ"
                | "ɪ"
                | "ʊ"
                | "ʌ"
                | "i"
                | "u"
                | "e"
                | "o"
                | "a"
                | "A"
                | "I"
                | "O"
                | "W"
                | "Y"
                | "ɜ"
                | "ɚ"
        );

        if is_stress {
            in_stressed_syllable = true;
            seen_vowel_in_syllable = false;
            out.push(tok);
        } else if is_vowel {
            if seen_vowel_in_syllable {
                // second vowel in this syllable = new syllable boundary
                in_stressed_syllable = false;
            }
            if in_stressed_syllable {
                out.push(tok);
            } else {
                // unstressed syllable: reduce
                if tok == "ɛ" || tok == "ʌ" {
                    out.push("ə".to_string());
                } else {
                    out.push(tok);
                }
            }
            seen_vowel_in_syllable = true;
        } else {
            // consonant: start of potential new syllable
            out.push(tok);
        }
    }
    out
}

// ──────────────────────────────────────────────────────────────────
// Full lexicon conversion
// ──────────────────────────────────────────────────────────────────

/// Convert an entire gruut lexicon file to the target convention,
/// writing a sherpa-format lexicon.txt. Two conversion layers:
///
/// 1. **P2P table** (when `p2p_table` is `Some`): exact lookup of the
///    gruut phoneme sequence — word-specific conversions learned from
///    the target family's own lexicon. 100% correct for in-table words.
/// 2. **Rules** (this module): symbol substitution + phonological
///    rules for out-of-table sequences. Approximate for the ~10% of
///    words whose conventions differ unpredictably.
///
/// Words whose converted symbols fall outside the target vocabulary
/// are skipped. Returns (written, skipped, via_table).
///
/// # Errors
/// Returns an error on file I/O problems.
pub fn convert_lexicon_file(
    gruut_lexicon_path: &std::path::Path,
    tokens_txt_path: &std::path::Path,
    convention: Convention,
    p2p_table: Option<&crate::sherpa_p2p::P2pTable>,
    output_path: &std::path::Path,
) -> Result<(usize, usize, usize), LexiconGenError> {
    let target_vocab = crate::sherpa_lexicon::read_tokens_txt_public(tokens_txt_path)?;

    let input = std::fs::File::open(gruut_lexicon_path)
        .map_err(|e| LexiconGenError(format!("reading {}: {e}", gruut_lexicon_path.display())))?;
    let reader = std::io::BufReader::new(input);

    let output = std::fs::File::create(output_path)
        .map_err(|e| LexiconGenError(format!("creating {}: {e}", output_path.display())))?;
    let mut writer = std::io::BufWriter::new(output);

    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut via_table = 0usize;

    for line in reader.lines().map_while(Result::ok) {
        let mut parts = line.split_whitespace();
        let Some(word) = parts.next() else { continue };
        let gruut_phones: Vec<&str> = parts.collect();
        if gruut_phones.is_empty() {
            continue;
        }
        // layer 1: exact P2P table lookup (word-specific, learned),
        // accepted only when every output symbol is in the vocabulary
        let from_table = p2p_table
            .and_then(|t| t.lookup(&gruut_phones))
            .filter(|tokens| tokens.iter().all(|t| target_vocab.contains(t.as_str())));
        let converted = match from_table {
            Some(tokens) => {
                via_table += 1;
                Some(tokens)
            }
            // layer 2: rules fallback (systematic conversion)
            None => convert_entry(word, &gruut_phones, convention, &target_vocab),
        };
        match converted {
            Some(converted) => {
                use std::io::Write;
                writeln!(writer, "{word} {}", converted.join(" "))
                    .map_err(|e| LexiconGenError(format!("writing: {e}")))?;
                written += 1;
            }
            None => skipped += 1,
        }
    }

    writer
        .flush()
        .map_err(|e| LexiconGenError(format!("flushing: {e}")))?;
    Ok((written, skipped, via_table))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn vocab_with(symbols: &[&str]) -> HashSet<String> {
        symbols.iter().map(ToString::to_string).collect()
    }

    fn full_english_vocab() -> HashSet<String> {
        // all IPA symbols that appear in gruut English + both conventions
        let base = [
            "b", "d", "f", "h", "i", "j", "k", "l", "m", "n", "p", "s", "t", "u", "v", "w", "z",
            "æ", "ð", "ŋ", "ɑ", "ɔ", "ə", "ɚ", "ɛ", "ɜ", "ɡ", "ɪ", "ɹ", "ʃ", "ʊ", "ʌ", "ʒ", "θ",
            "ˈ", "ˌ", "a", "e", "o", // diphthong components
            // misaki additions
            "A", "I", "O", "W", "Y", "ʧ", "ʤ",
        ];
        vocab_with(&base)
    }

    // ── misaki convention ──

    #[test]
    fn misaki_diphthongs_become_uppercase() {
        let vocab = full_english_vocab();
        // "day" → gruut: d ˈeɪ → misaki: d ˈ A
        let out = convert_entry("day", &["d", "ˈeɪ"], Convention::Misaki, &vocab).unwrap();
        assert_eq!(out, vec!["d", "ˈ", "A"]);

        // "time" → gruut: t ˈaɪ m → misaki: t ˈ I m
        let out = convert_entry("time", &["t", "ˈaɪ", "m"], Convention::Misaki, &vocab).unwrap();
        assert_eq!(out, vec!["t", "ˈ", "I", "m"]);
    }

    #[test]
    fn misaki_affricates_become_ligatures() {
        let vocab = full_english_vocab();
        // "just" → gruut: d͡ʒ ˈʌ s t → misaki: ʤ ˈ ʌ s t
        let out =
            convert_entry("just", &["d͡ʒ", "ˈʌ", "s", "t"], Convention::Misaki, &vocab).unwrap();
        assert_eq!(out, vec!["ʤ", "ˈ", "ʌ", "s", "t"]);

        // "speech" → gruut: s p ˈi t͡ʃ → misaki: s p ˈ i ʧ
        let out = convert_entry(
            "speech",
            &["s", "p", "ˈi", "t͡ʃ"],
            Convention::Misaki,
            &vocab,
        )
        .unwrap();
        assert_eq!(out, vec!["s", "p", "ˈ", "i", "ʧ"]);
    }

    #[test]
    fn function_words_lose_stress() {
        let vocab = full_english_vocab();
        // "the" → gruut: ð ə (already unstressed in gruut) → unchanged
        let out = convert_entry("the", &["ð", "ə"], Convention::Misaki, &vocab).unwrap();
        assert_eq!(out, vec!["ð", "ə"]);

        // "and" → gruut: ˈæ n d → misaki: æ n d (destressed)
        let out = convert_entry("and", &["ˈæ", "n", "d"], Convention::Misaki, &vocab).unwrap();
        assert_eq!(out, vec!["æ", "n", "d"]);
    }

    #[test]
    fn rhotic_schwa_splits() {
        let vocab = full_english_vocab();
        // "world" → gruut: w ˈɚ l d → misaki: w ˈ ɜ ɹ l d
        let out =
            convert_entry("world", &["w", "ˈɚ", "l", "d"], Convention::Misaki, &vocab).unwrap();
        assert_eq!(out, vec!["w", "ˈ", "ɜ", "ɹ", "l", "d"]);

        // "over" → gruut: ˈoʊ v ɚ → misaki: ˈ O v ə ɹ
        let out = convert_entry("over", &["ˈoʊ", "v", "ɚ"], Convention::Misaki, &vocab).unwrap();
        assert_eq!(out, vec!["ˈ", "O", "v", "ə", "ɹ"]);
    }

    // ── espeak convention ──

    #[test]
    fn espeak_diphthongs_decompose() {
        let vocab = full_english_vocab();
        // "day" → gruut: d ˈeɪ → espeak: d ˈ e ɪ
        let out = convert_entry("day", &["d", "ˈeɪ"], Convention::Espeak, &vocab).unwrap();
        assert_eq!(out, vec!["d", "ˈ", "e", "ɪ"]);
    }

    #[test]
    fn espeak_keeps_standard_ipa() {
        let vocab = full_english_vocab();
        // "test" → gruut: t ˈɛ s t → espeak: t ˈ ɛ s t (identity)
        let out =
            convert_entry("test", &["t", "ˈɛ", "s", "t"], Convention::Espeak, &vocab).unwrap();
        assert_eq!(out, vec!["t", "ˈ", "ɛ", "s", "t"]);
    }

    // ── vocab filtering ──

    #[test]
    fn unmappable_symbols_skip_word() {
        // vocab missing `A` → misaki conversion of "day" fails
        let vocab = vocab_with(&["d", "ˈ", "e", "ɪ"]); // no `A`
        let out = convert_entry("day", &["d", "ˈeɪ"], Convention::Misaki, &vocab);
        assert!(out.is_none());
    }

    // ── layered conversion (P2P table + rules) ──

    fn write_file(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn table_takes_priority_and_rules_cover_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let gruut = write_file(
            tmp.path(),
            "gruut.txt",
            // "the": rules would keep ð ə; the table says ð i
            // "day": not in the table → rules apply (d ˈ A)
            "the ð ə\nday d ˈeɪ\n",
        );
        let tokens = write_file(tmp.path(), "tokens.txt", "ð 0\nə 1\ni 2\nd 3\nˈ 4\nA 5\n");
        let pairs = write_file(tmp.path(), "pairs.tsv", "ð|ə\tð i\n");
        let out = tmp.path().join("lexicon.txt");
        let table = crate::sherpa_p2p::P2pTable::from_tsv(&pairs).unwrap();

        let (written, skipped, via_table) =
            convert_lexicon_file(&gruut, &tokens, Convention::Misaki, Some(&table), &out).unwrap();
        assert_eq!((written, skipped, via_table), (2, 0, 1));

        let content = std::fs::read_to_string(&out).unwrap();
        // table layer: word-specific conversion wins
        assert!(content.contains("the ð i"));
        // rules layer: fallback for out-of-table words
        assert!(content.contains("day d ˈ A"));
    }

    #[test]
    fn table_entry_with_out_of_vocab_symbols_falls_back_to_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let gruut = write_file(tmp.path(), "gruut.txt", "day d ˈeɪ\n");
        // vocab has no `A` — the table's output would violate it
        let tokens = write_file(tmp.path(), "tokens.txt", "d 0\nˈ 1\ne 2\nɪ 3\n");
        let pairs = write_file(tmp.path(), "pairs.tsv", "d|ˈeɪ\td ˈ A\n");
        let out = tmp.path().join("lexicon.txt");
        let table = crate::sherpa_p2p::P2pTable::from_tsv(&pairs).unwrap();

        let (written, _skipped, via_table) =
            convert_lexicon_file(&gruut, &tokens, Convention::Espeak, Some(&table), &out).unwrap();
        // rules produce d ˈ e ɪ (all in vocab); table output rejected
        assert_eq!((written, via_table), (1, 0));
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("day d ˈ e ɪ"));
    }
}
