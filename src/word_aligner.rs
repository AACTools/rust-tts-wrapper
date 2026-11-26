//! Word-level forced alignment over CTC emissions (issue #109, the
//! "ready half"): replace the 150-wpm proportional boundary estimates
//! with real per-word timings measured from the delivered audio.
//!
//! The design ports fairseq's MMS alignment recipe
//! (`examples/mms/data_prep`): a multilingual wav2vec2-CTC aligner
//! (31-token uroman alphabet, 1,130 languages) produces per-frame
//! log-probabilities; a Viterbi forced alignment finds the frame path
//! matching the known text; character runs roll up to word spans.
//!
//! This module holds the MODEL-FREE half — the Viterbi DP and the
//! span logic — pure and unit-testable. The `ort` model runner and
//! the engine integration live behind the `word-align` feature.
//!
//! MMS model weights are **CC-BY-NC 4.0** (non-commercial): the model
//! is an optional, separately-fetched artifact; commercial builds
//! simply keep the proportional-estimate path.

/// CTC forced alignment (the `torchaudio.functional.forced_align`
/// algorithm): the frame path that maximizes the joint probability of
/// emitting `targets`.
///
/// State layout (standard CTC DP): `2L+1` interleaved states — even
/// states are the CTC blank, odd state `2i+1` is `targets[i]`. A
/// direct token-to-token skip (two states apart) is legal only when
/// adjacent tokens differ — between identical tokens the blank is
/// mandatory.
///
/// Returns one label per frame: the CTC blank index or a token value
/// (which may itself equal `blank` only via `targets` containing it —
/// callers keep blanks out of `targets`).
#[must_use]
pub fn forced_align(log_probs: &[Vec<f32>], targets: &[usize], blank: usize) -> Vec<usize> {
    let t_len = log_probs.len();
    let l_len = targets.len();
    assert!(t_len > 0, "empty emissions");
    assert!(l_len > 0, "empty targets");
    let n_states = 2 * l_len + 1;
    let n_vocab = log_probs[0].len();

    // label emitted while occupying a state (blank or the token)
    let state_label = |s: usize| -> usize {
        if s.is_multiple_of(2) {
            blank
        } else {
            targets[s / 2]
        }
    };

    let neg_inf = f32::NEG_INFINITY;
    let mut alpha = vec![neg_inf; n_states];
    let mut back: Vec<Vec<i32>> = vec![vec![-1; n_states]; t_len];

    // t = 0: start at the initial blank or the first token.
    if n_vocab > blank {
        alpha[0] = log_probs[0][blank];
    }
    if n_states > 1 && n_vocab > targets[0] {
        alpha[1] = log_probs[0][targets[0]];
    }

    for t in 1..t_len {
        let mut next = vec![neg_inf; n_states];
        for s in 0..n_states {
            let label = state_label(s);
            if label >= n_vocab {
                continue;
            }
            // Predecessors: stay (s), extend (s-1), skip (s-2).
            let mut best = alpha[s];
            let mut best_p: i32 = s as i32;
            if s > 0 && alpha[s - 1] > best {
                best = alpha[s - 1];
                best_p = s as i32 - 1;
            }
            if s > 1 {
                // A two-state skip connects two TOKEN states; legal
                // only when the adjacent tokens differ.
                let skip_ok = s % 2 == 1 && targets[s / 2] != targets[s / 2 - 1];
                if skip_ok && alpha[s - 2] > best {
                    best = alpha[s - 2];
                    best_p = s as i32 - 2;
                }
            }
            next[s] = best + log_probs[t][label];
            back[t][s] = best_p;
        }
        alpha = next;
    }

    // Terminate on the final token or the trailing blank.
    let last_token = 2 * l_len - 1;
    let final_state = if alpha[n_states - 1] >= alpha[last_token] {
        n_states - 1
    } else {
        last_token
    };

    // Backtrack the state path, then emit the per-frame labels.
    let mut state = final_state as i32;
    let mut path = vec![blank; t_len];
    for t in (0..t_len).rev() {
        path[t] = state_label(state.max(0) as usize);
        if t > 0 {
            state = back[t][state.max(0) as usize];
        }
    }
    path
}

/// A run of identical labels in the alignment path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The CTC label (token id or blank) of this run.
    pub label: usize,
    /// First frame of the run (inclusive).
    pub start: usize,
    /// Last frame of the run (inclusive — matches the reference).
    pub end: usize,
}

/// Collapse the per-frame path into maximal runs
/// (`merge_repeats` in the reference).
#[must_use]
pub fn merge_repeats(path: &[usize]) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut i1 = 0usize;
    while i1 < path.len() {
        let mut i2 = i1;
        while i2 < path.len() && path[i1] == path[i2] {
            i2 += 1;
        }
        segments.push(Segment {
            label: path[i1],
            start: i1,
            end: i2 - 1,
        });
        i1 = i2;
    }
    segments
}

/// Word timing in seconds (frame indices × the frame stride).
#[derive(Debug, Clone, PartialEq)]
pub struct WordSpan {
    /// Zero-based word index in the input word list.
    pub word_index: usize,
    /// Word onset in seconds.
    pub start_secs: f32,
    /// Word offset in seconds.
    pub end_secs: f32,
}

/// Roll character segments up to word spans, with the reference's
/// silence padding: a word's span extends into a neighbouring blank
/// run at its midpoint (the first/last words take the whole run).
///
/// `segments` is the FULL merged path (blanks included); `word_chars`
/// holds, per word, the indices into `segments` of that word's
/// character runs in order (an empty word contributes nothing).
#[must_use]
pub fn word_spans(
    segments: &[Segment],
    word_chars: &[Vec<usize>],
    blank: usize,
    stride_secs: f32,
) -> Vec<WordSpan> {
    // Per word: first and last segment index (into the full list).
    let mut first: Vec<Option<usize>> = vec![None; word_chars.len()];
    let mut last: Vec<usize> = vec![0; word_chars.len()];
    for (wi, idxs) in word_chars.iter().enumerate() {
        if let Some(&k0) = idxs.first() {
            first[wi] = Some(k0);
            last[wi] = *idxs.last().expect("non-empty has a last");
        }
    }
    let words_with_segs: Vec<usize> = (0..word_chars.len())
        .filter(|&wi| first[wi].is_some())
        .collect();
    let Some(&first_word) = words_with_segs.first() else {
        return Vec::new();
    };
    let &last_word = words_with_segs
        .last()
        .expect("words_with_segs is non-empty");

    let mut out = Vec::new();
    for wi in 0..word_chars.len() {
        let Some(k0) = first[wi] else {
            continue; // word with no segments (all chars unmapped)
        };
        let k1 = last[wi];
        let mut start = segments[k0].start;
        let mut end = segments[k1].end;
        // Pad into the neighbouring blank run — the reference's rule:
        // the first/last word's interval takes the whole run, others
        // the midpoint.
        if k0 > 0 {
            let prev = &segments[k0 - 1];
            if prev.label == blank {
                let pad = if wi == first_word {
                    prev.start
                } else {
                    usize::midpoint(prev.start, prev.end)
                };
                start = start.min(pad);
            }
        }
        if k1 + 1 < segments.len() {
            let next = &segments[k1 + 1];
            if next.label == blank {
                let pad = if wi == last_word {
                    next.end
                } else {
                    usize::midpoint(next.start, next.end)
                };
                end = end.max(pad);
            }
        }
        // Frame counts are far below f32's mantissa; seconds math is f32.
        #[allow(clippy::cast_precision_loss)]
        let start_f = start as f32;
        #[allow(clippy::cast_precision_loss)]
        let end_f = (end + 1) as f32;
        out.push(WordSpan {
            word_index: wi,
            start_secs: start_f * stride_secs,
            end_secs: end_f * stride_secs,
        });
    }
    out
}

#[cfg(test)]
mod oracle_crosscheck {
    use super::*;

    /// Cross-check the Rust port against the reference pipeline
    /// (torchaudio `forced_align` + fairseq `get_spans`) on the real
    /// model's emissions: identical inputs, so any span difference is
    /// a port bug. Generate the oracle first:
    ///
    /// ```text
    /// ~/.cache/rust-tts-wrapper/mms-aligner/reference_oracle.py
    /// WORD_ALIGNER_ORACLE=~/.cache/rust-tts-wrapper/mms-aligner \
    ///     cargo test --lib word_aligner -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the reference oracle dump (generate with reference_oracle.py)"]
    #[allow(clippy::too_many_lines)] // test body: load, align, compare
    fn matches_torchaudio_reference_alignment() {
        let Some(dir) = std::env::var_os("WORD_ALIGNER_ORACLE") else {
            eprintln!("WORD_ALIGNER_ORACLE not set; skipping");
            return;
        };
        let dir = std::path::PathBuf::from(dir);

        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("oracle.json")).expect("oracle.json"),
        )
        .expect("parse oracle.json");
        let blank = meta["blank"].as_u64().expect("blank") as usize;
        let stride = meta["stride_ms"].as_f64().expect("stride") as f32 / 1000.0;
        let char_ids: std::collections::HashMap<char, usize> = meta["char_ids"]
            .as_object()
            .expect("char_ids")
            .iter()
            .map(|(c, i)| {
                (
                    c.chars().next().expect("single char"),
                    i.as_u64().expect("u64") as usize,
                )
            })
            .collect();
        let token_indices: Vec<usize> = meta["token_indices"]
            .as_array()
            .expect("token_indices")
            .iter()
            .map(|v| v.as_u64().expect("u64") as usize)
            .collect();
        let words: Vec<String> = meta["words"]
            .as_array()
            .expect("words")
            .iter()
            .map(|v| v.as_str().expect("str").to_string())
            .collect();

        // Load the emissions dump (T x 31 f32, little-endian).
        let raw = std::fs::read(dir.join("oracle_emissions.f32")).expect("emissions dump");
        // 31 f32 per frame (const-generic chunks_exact: clippy)
        let flat: Vec<f32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let emissions: Vec<Vec<f32>> = flat
            .as_chunks::<31>()
            .0
            .iter()
            .map(|c: &[f32; 31]| c.to_vec())
            .collect();

        // The port under test.
        let path = forced_align(&emissions, &token_indices, blank);
        let segments = merge_repeats(&path);

        // Walk the words over the segments (the same greedy walk as
        // the oracle generator: each word char takes the NEXT segment
        // carrying that char's token id).
        let mut word_chars: Vec<Vec<usize>> = Vec::with_capacity(words.len());
        let mut cursor = 0usize;
        for w in &words {
            let mut idxs = Vec::new();
            for ch in w.chars() {
                let Some(&id) = char_ids.get(&ch) else {
                    continue; // char not in the vocab at all
                };
                while cursor < segments.len() && segments[cursor].label != id {
                    cursor += 1;
                }
                if cursor < segments.len() {
                    idxs.push(cursor);
                    cursor += 1;
                }
            }
            word_chars.push(idxs);
        }
        let spans = word_spans(&segments, &word_chars, blank, stride);

        // The oracle spans (word_index, start_secs, end_secs).
        let oracle: Vec<(usize, f64, f64)> = meta["spans"]
            .as_array()
            .expect("spans")
            .iter()
            .map(|v| {
                let a = v.as_array().expect("triple");
                (
                    a[0].as_u64().expect("u") as usize,
                    a[1].as_f64().expect("f"),
                    a[2].as_f64().expect("f"),
                )
            })
            .collect();
        assert_eq!(spans.len(), oracle.len(), "span count");
        let tol = stride; // one frame of tolerance
        for (got, (wi, s, e)) in spans.iter().zip(&oracle) {
            assert_eq!(got.word_index, *wi, "word index");
            assert!(
                (got.start_secs - *s as f32).abs() <= tol,
                "start mismatch word {wi} ('{}'): {} vs {s}",
                words[*wi],
                got.start_secs
            );
            assert!(
                (got.end_secs - *e as f32).abs() <= tol,
                "end mismatch word {wi} ('{}'): {} vs {e}",
                words[*wi],
                got.end_secs
            );
            eprintln!(
                "  '{}': {:.3}..{:.3} (oracle {:.3}..{:.3})",
                words[*wi], got.start_secs, got.end_secs, s, e
            );
        }
        eprintln!("ORACLE MATCH: {} words within one frame", spans.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two-token vocab (0 = blank, 1 = token) over hand-built
    /// emissions: frames 0-1 blank, 2-3 token, 4 blank.
    #[test]
    fn aligns_single_token() {
        let lp: Vec<Vec<f32>> = (0..6)
            .map(|t| {
                let tok = (2..=3).contains(&t);
                vec![if tok { -5.0 } else { 0.0 }, if tok { 0.0 } else { -5.0 }]
            })
            .collect();
        let path = forced_align(&lp, &[1], 0);
        let segs = merge_repeats(&path);
        assert_eq!(segs.len(), 3);
        assert_eq!((segs[0].label, segs[0].start, segs[0].end), (0, 0, 1));
        assert_eq!((segs[1].label, segs[1].start, segs[1].end), (1, 2, 3));
        assert_eq!((segs[2].label, segs[2].start, segs[2].end), (0, 4, 5));
    }

    #[test]
    fn identical_tokens_need_the_blank_between() {
        // Emissions that tempt a direct 1->1 skip: a strong token run
        // between two 'a' targets must split at the weakest frame.
        let lp: Vec<Vec<f32>> = (0..7)
            .map(|t| {
                // frames 3 is the dip
                let strong = if t == 3 { -1.0 } else { 0.0 };
                vec![-5.0, strong]
            })
            .collect();
        let path = forced_align(&lp, &[1, 1], 0);
        // Every frame consumed: the path covers 7 frames.
        assert_eq!(path.len(), 7);
        // The blank must appear between the two token runs.
        assert!(path.contains(&0), "a blank between identical tokens");
        let segs = merge_repeats(&path);
        let token_runs: Vec<&Segment> = segs.iter().filter(|s| s.label == 1).collect();
        assert_eq!(token_runs.len(), 2, "two separate token runs");
    }

    #[test]
    fn word_spans_roll_up_and_pad() {
        // Path: blank | 'h' 'e' | blank | 'w' | blank
        let segs = vec![
            Segment {
                label: 0,
                start: 0,
                end: 1,
            },
            Segment {
                label: 8,
                start: 2,
                end: 2,
            }, // 'h'
            Segment {
                label: 5,
                start: 3,
                end: 3,
            }, // 'e'
            Segment {
                label: 0,
                start: 4,
                end: 5,
            },
            Segment {
                label: 23,
                start: 6,
                end: 8,
            }, // 'w'
            Segment {
                label: 0,
                start: 9,
                end: 10,
            },
        ];
        let spans = word_spans(&segs, &[vec![1, 2], vec![4]], 0, 0.02);
        assert_eq!(spans.len(), 2);
        // "he": first word — pads into the whole leading blank run.
        assert_eq!(spans[0].start_secs, 0.0);
        // ends at frame 3; the middle blank midpoint (4+5)/2 = 4
        assert_eq!(spans[0].end_secs, 5.0 * 0.02);
        // "w": pads back to the middle blank midpoint 4
        assert_eq!(spans[1].start_secs, 4.0 * 0.02);
        // last word — takes the whole trailing blank run (end 10)
        assert_eq!(spans[1].end_secs, 11.0 * 0.02);
    }

    #[test]
    fn word_spans_skip_unmapped_words() {
        let segs = vec![
            Segment {
                label: 0,
                start: 0,
                end: 2,
            },
            Segment {
                label: 5,
                start: 3,
                end: 4,
            }, // 'e'
            Segment {
                label: 0,
                start: 5,
                end: 6,
            },
        ];
        // Middle word has no segments (e.g. all punctuation): skipped,
        // the span list carries only words 0 and 2 with their indices.
        let spans = word_spans(&segs, &[vec![1], vec![], vec![1]], 0, 0.02);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].word_index, 0);
        assert_eq!(spans[1].word_index, 2);
        // Word 0 is NOT the last word with segments → midpoint padding;
        // word 2 IS → the whole trailing blank run.
        assert_eq!(spans[0].end_secs, 6.0 * 0.02);
        assert_eq!(spans[1].end_secs, 7.0 * 0.02);
    }
}
