//! Phoneme→phoneme lookup table: word-specific convention conversion
//! learned from aligned lexicon pairs.
//!
//! The rule-based conversion in [`crate::sherpa_convention`] handles the
//! symbol layer and the systematic phonology (destressing, rhotic
//! splitting, vowel reduction), but ~10% of common words differ between
//! conventions in ways no rule set captures — stress level choices
//! (`ˈ` vs `ˌ`), weak-form selection (`ð ə` vs `ð i`), allophones
//! (`t` vs `ɾ`, `ɪ` vs `ᵻ`). Those differences are word-specific facts
//! about how each phonemizer transcribes each word.
//!
//! This module encodes them as data: an aligned-pairs table mapping
//! gruut phoneme sequences to target-convention sequences, generated
//! offline by aligning the gruut lexicon against the target model
//! family's own bundled lexicon (e.g. kokoro multi-lang's
//! `lexicon-us-en.txt`, Apache 2.0). One table per convention — not per
//! model. At lexicon-generation time the table is the FIRST conversion
//! layer (exact match, 100% correct for the ~47k overlapping English
//! words); the rules remain the fallback for out-of-table sequences.
//!
//! Table format (TSV): `gruut|ph1|ph2\ttgt1 tgt2 tgt3` per line; `#`
//! comments and blank lines are skipped. Duplicate keys keep the first
//! entry (homograph variants; the rules fallback never runs for these,
//! so the choice is arbitrary but stable).

use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use crate::sherpa_lexicon::LexiconGenError;

/// gruut phoneme sequence (joined with spaces) → target tokens.
#[derive(Debug, Default)]
pub struct P2pTable {
    table: HashMap<String, Vec<String>>,
}

impl P2pTable {
    /// Load an aligned-pairs TSV.
    ///
    /// # Errors
    /// Returns an error if the file is unreadable.
    pub fn from_tsv(path: &Path) -> Result<Self, LexiconGenError> {
        let file = std::fs::File::open(path)
            .map_err(|e| LexiconGenError(format!("reading {}: {e}", path.display())))?;
        let reader = std::io::BufReader::new(file);
        let mut table = HashMap::new();
        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((source, target)) = line.split_once('\t') else {
                continue;
            };
            // source: pipe-separated gruut phonemes; target: space-separated
            let key = source.replace('|', " ");
            let value: Vec<String> = target.split_whitespace().map(str::to_owned).collect();
            if !key.is_empty() && !value.is_empty() {
                // first entry wins (stable homograph choice)
                table.entry(key).or_insert(value);
            }
        }
        Ok(Self { table })
    }

    /// Build from inline TSV text (tests).
    #[must_use]
    pub fn from_tsv_str(text: &str) -> Self {
        let mut table = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((source, target)) = line.split_once('\t') else {
                continue;
            };
            let key = source.replace('|', " ");
            let value: Vec<String> = target.split_whitespace().map(str::to_owned).collect();
            if !key.is_empty() && !value.is_empty() {
                table.entry(key).or_insert(value);
            }
        }
        Self { table }
    }

    /// Exact lookup: gruut phoneme tokens → target tokens, or `None`
    /// when the sequence is not in the table.
    #[must_use]
    pub fn lookup(&self, gruut_tokens: &[&str]) -> Option<Vec<String>> {
        self.table.get(&gruut_tokens.join(" ")).cloned()
    }

    /// Number of table entries (diagnostics).
    #[must_use]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// True when the table has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
h|ɛ|l|ˈoʊ\th ə l ˈ O
w|ˈɚ|l|d\tw ˈ ɜ ɹ l d
ð|ə\tð i
# comment line
";
    fn table() -> P2pTable {
        P2pTable::from_tsv_str(SAMPLE)
    }

    #[test]
    fn exact_lookup_returns_target_sequence() {
        let t = table();
        assert_eq!(t.len(), 3);
        // word-specific fact the rules get wrong: "the" → ð i (not ð ə)
        assert_eq!(
            t.lookup(&["ð", "ə"]),
            Some(vec!["ð".to_string(), "i".to_string()])
        );
        assert_eq!(
            t.lookup(&["h", "ɛ", "l", "ˈoʊ"]),
            Some(vec![
                "h".to_string(),
                "ə".to_string(),
                "l".to_string(),
                "ˈ".to_string(),
                "O".to_string()
            ])
        );
    }

    #[test]
    fn unknown_sequence_returns_none() {
        assert!(table().lookup(&["k", "æ", "t"]).is_none());
        assert!(table().lookup(&[]).is_none());
    }

    #[test]
    fn first_entry_wins_on_duplicate_keys() {
        let t = P2pTable::from_tsv_str("ə\tA\nə\tB\n");
        assert_eq!(t.lookup(&["ə"]), Some(vec!["A".to_string()]));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn tsv_file_round_trip() {
        let dir = std::env::temp_dir().join("sherpa_p2p_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("pairs.tsv");
        std::fs::write(&p, SAMPLE).unwrap();
        let t = P2pTable::from_tsv(&p).unwrap();
        assert_eq!(t.lookup(&["w", "ˈɚ", "l", "d"]).unwrap()[2], "ɜ");
        let missing = P2pTable::from_tsv(&dir.join("nope.tsv"));
        assert!(missing.is_err());
    }
}
