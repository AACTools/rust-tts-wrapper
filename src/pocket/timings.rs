//! Attention -> word boundaries. The per-step softmax slice over target
//! token positions is the model's own alignment (validated against whisper
//! within ~0.17s on matched words; see SPRINTS.md).

use crate::pocket::tokenizer::PocketTokenizer;

pub struct WordTiming {
    pub word: String,
    pub start_s: f32,
    pub end_s: f32,
}

/// `attn_per_step`: per generation step, the head-averaged attention over
/// the TARGET token positions (columns = target tokens). `target_ids`:
/// those tokens. `step_dur_s`: audio seconds per step. Tokens sharing a
/// word (no ▁ marker) merge; boundary = dominance transitions.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn word_boundaries(
    tokenizer: &PocketTokenizer,
    target_ids: &[u32],
    attn_per_step: &[Vec<f32>],
    step_dur_s: f32,
) -> Vec<WordTiming> {
    // token -> word grouping
    let mut words: Vec<String> = Vec::new();
    let mut tok2word = Vec::with_capacity(target_ids.len());
    for id in target_ids {
        let piece = tokenizer.id_to_piece(*id);
        if piece.starts_with('\u{2581}') && piece.len() > 1 {
            words.push(piece.trim_start_matches('\u{2581}').to_string());
            tok2word.push(words.len() - 1);
        } else if words.is_empty() {
            words.push(piece);
            tok2word.push(0);
        } else {
            tok2word.push(words.len() - 1);
        }
    }
    // per-step dominant word
    let n_words = words.len();
    let mut dom: Vec<usize> = Vec::with_capacity(attn_per_step.len());
    for attn in attn_per_step {
        let mut mass = vec![0f32; n_words];
        for (ti, m) in attn.iter().enumerate() {
            if ti < tok2word.len() {
                mass[tok2word[ti]] += m;
            }
        }
        dom.push(
            mass.iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map_or(0, |(i, _)| i),
        );
    }
    // spans, ignoring single-step regressions
    let mut spans: Vec<Option<(usize, usize)>> = vec![None; n_words];
    for (step, w) in dom.iter().enumerate() {
        match spans[*w] {
            Some((s0, ref mut s1)) if step + 1 > *s1 && step.saturating_sub(*s1) <= 1 => {
                *s1 = step;
                let _ = s0;
            }
            Some((s0, s1)) if step >= s0 && step <= s1 => {}
            None => spans[*w] = Some((step, step)),
            _ => {}
        }
    }
    // fill gaps by neighbor extension
    for w in 0..n_words {
        if spans[w].is_none() {
            let prev_end = w.checked_sub(1).and_then(|p| spans[p]).map(|(_, e)| e);
            let next_start = spans[w + 1..].iter().flatten().map(|(s, _)| *s).min();
            if let (Some(p), Some(nx)) = (prev_end, next_start) {
                spans[w] = Some((p + 1, nx.saturating_sub(1).max(p + 1)));
            }
        }
    }
    spans
        .iter()
        .zip(&words)
        .filter_map(|(sp, word)| {
            sp.map(|(s0, s1)| WordTiming {
                word: word.clone(),
                start_s: (s0 as f32) * step_dur_s,
                end_s: ((s1 + 1) as f32) * step_dur_s,
            })
        })
        .collect()
}
