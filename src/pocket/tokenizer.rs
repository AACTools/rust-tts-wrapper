//! SentencePiece-style Viterbi tokenizer over pocket's vocab.json +
//! token_scores.json — a direct port of sherpa's test_tokenizer.py
//! (▁-space markers, byte fallback, NO added BOS/EOS).

use std::collections::HashMap;
use std::path::Path;

pub struct PocketTokenizer {
    token2id: HashMap<String, u32>,
    token2score: HashMap<String, f32>,
    by_first_char: HashMap<char, Vec<String>>,
}

const NEG: f32 = -1.0e30;

impl PocketTokenizer {
    /// Load vocab + scores.
    ///
    /// # Errors
    ///
    /// IO or JSON parse failures.
    pub fn load(vocab_json: &Path, token_scores_json: &Path) -> Result<Self, String> {
        let vocab: HashMap<String, u32> =
            serde_json::from_str(&std::fs::read_to_string(vocab_json).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let scores: HashMap<String, f32> = serde_json::from_str(
            &std::fs::read_to_string(token_scores_json).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut by_first_char: HashMap<char, Vec<String>> = HashMap::new();
        for tok in vocab.keys() {
            if let Some(c) = tok.chars().next() {
                by_first_char.entry(c).or_default().push(tok.clone());
            }
        }
        Ok(Self {
            token2id: vocab,
            token2score: scores,
            by_first_char,
        })
    }

    /// Encode text to token ids (Viterbi over scores; byte fallback).
    #[must_use]
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut s = text.replace(' ', "\u{2581}");
        if !s.starts_with('\u{2581}') {
            s.insert(0, '\u{2581}');
        }
        // work on chars for indexing, pieces are char-sliced (vocab pieces may
        // be multi-byte; compare on char boundaries)
        let cs: Vec<char> = s.chars().collect();
        let n = cs.len();
        let mut dp = vec![NEG; n + 1];
        let mut back: Vec<Option<String>> = vec![None; n + 1];
        dp[n] = 0.0;
        for i in (0..n).rev() {
            let c = cs[i];
            if let Some(toks) = self.by_first_char.get(&c) {
                for tok in toks {
                    let tcs: Vec<char> = tok.chars().collect();
                    if i + tcs.len() <= n && cs[i..i + tcs.len()] == tcs[..] {
                        let sc =
                            self.token2score.get(tok).copied().unwrap_or(0.0) + dp[i + tcs.len()];
                        if sc > dp[i] {
                            dp[i] = sc;
                            back[i] = Some(tok.clone());
                        }
                    }
                }
            }
            if back[i].is_none() {
                // byte fallback: first UTF-8 byte of the char
                let b = c.encode_utf8(&mut [0u8; 4]).as_bytes()[0];
                let tok = format!("<0x{b:02X}>");
                dp[i] = self.token2score.get(&tok).copied().unwrap_or(0.0) + dp[i + 1];
                back[i] = Some(tok);
            }
        }
        let mut ids = Vec::new();
        let mut i = 0;
        while i < n {
            let tok = back[i].clone().unwrap_or_else(|| "<unk>".into());
            ids.push(self.token2id.get(&tok).copied().unwrap_or(0));
            i += tok.chars().count();
        }
        ids
    }

    /// Map token ids back to piece strings (for word grouping).
    #[must_use]
    pub fn id_to_piece(&self, id: u32) -> String {
        self.token2id
            .iter()
            .find(|(_, v)| **v == id)
            .map(|(k, _)| k.clone())
            .unwrap_or_default()
    }
}

/// WordLevel whitespace tokenizer (HF `tokenizers` json, WordLevel model) —
/// the phoneme bundles: one id per whitespace-separated phone, `<unk>` = 0.
/// Word structure is supplied by the caller (`|`-separated phoneme words).
pub struct WordLevelTokenizer {
    token2id: HashMap<String, u32>,
    id2token: Vec<String>,
}

#[derive(serde::Deserialize)]
struct TokenizersJson {
    model: WordLevelModel,
}

#[derive(serde::Deserialize)]
struct WordLevelModel {
    vocab: HashMap<String, u32>,
}

impl WordLevelTokenizer {
    /// Load a tokenizer.json (WordLevel).
    ///
    /// # Errors
    ///
    /// IO or JSON parse failures.
    pub fn load(path: &Path) -> Result<Self, String> {
        let j: TokenizersJson =
            serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let max_id = j.model.vocab.values().copied().max().unwrap_or(0) as usize;
        let mut id2token = vec![String::new(); max_id + 1];
        for (tok, id) in &j.model.vocab {
            if let Some(slot) = id2token.get_mut(*id as usize) {
                slot.clone_from(tok);
            }
        }
        Ok(Self {
            token2id: j.model.vocab,
            id2token,
        })
    }

    /// Encode a raw phoneme string (whitespace-split tokens; unknown -> 0).
    #[must_use]
    pub fn encode(&self, text: &str) -> Vec<u32> {
        text.split_whitespace()
            .map(|t| self.token2id.get(t).copied().unwrap_or(0))
            .collect()
    }

    /// Encode `|`-separated phoneme words -> (ids, tok2word, words).
    #[must_use]
    pub fn encode_grouped(&self, text: &str) -> (Vec<u32>, Vec<usize>, Vec<String>) {
        let mut ids = Vec::new();
        let mut tok2word = Vec::new();
        let mut words = Vec::new();
        for w in text.split('|') {
            let w = w.trim();
            if w.is_empty() {
                continue;
            }
            words.push(w.to_string());
            for t in w.split_whitespace() {
                ids.push(self.token2id.get(t).copied().unwrap_or(0));
                tok2word.push(words.len() - 1);
            }
        }
        (ids, tok2word, words)
    }

    /// Piece for an id (for display).
    #[must_use]
    pub fn id_to_piece(&self, id: u32) -> String {
        self.id2token.get(id as usize).cloned().unwrap_or_default()
    }
}

/// Either tokenizer flavor, chosen by the bundle layout.
pub enum AnyTokenizer {
    Viterbi(PocketTokenizer),
    WordLevel(WordLevelTokenizer),
}

impl AnyTokenizer {
    #[must_use]
    pub fn encode(&self, text: &str) -> Vec<u32> {
        match self {
            Self::Viterbi(t) => t.encode(text),
            Self::WordLevel(t) => t.encode(text),
        }
    }

    #[must_use]
    pub fn id_to_piece(&self, id: u32) -> String {
        match self {
            Self::Viterbi(t) => t.id_to_piece(id),
            Self::WordLevel(t) => t.id_to_piece(id),
        }
    }
}
