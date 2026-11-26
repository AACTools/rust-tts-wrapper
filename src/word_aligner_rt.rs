//! The `ort` model runner for [`crate::word_aligner`] (feature
//! `word-align`, issue #109): runs the MMS multilingual uroman CTC
//! aligner ONNX and returns word timings.
//!
//! Pipeline (fairseq `examples/mms/data_prep` ported):
//!
//! ```text
//! waveform 16 kHz f32 -> ort wav2vec2-CTC -> logits [T, 31]
//!   -> log_softmax -> forced_align (word_aligner) -> word spans
//! text -> normalize (lowercase a-z') -> per-char token ids
//! ```
//!
//! The 31-entry vocabulary is fixed by the model
//! (`ctc_alignment_mling_uroman/dictionary.txt`) and embedded here.
//! For Latin-script languages uroman is a passthrough (the reference
//! normalizes to `a-z` anyway); non-Latin scripts need the uroman
//! romanizer — not ported yet, non-Latin text is aligned per its
//! Latin-representable characters only (documented limitation).
//!
//! The ONNX file: convert once with the reference tooling (the
//! campaign spike script: `~/.cache/rust-tts-wrapper/mms-aligner/
//! export_onnx.py`, verified against torch to 1e-4). The model weights
//! are **CC-BY-NC 4.0** — an optional artifact, fetched/provided by
//! the caller; commercial builds keep the estimate path.

use crate::word_aligner::{forced_align, merge_repeats, word_spans, WordSpan};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The fixed CTC vocabulary of the MMS uroman aligner (31 entries, in
/// index order — `dictionary.txt` of the model).
pub const VOCAB: [&str; 31] = [
    "<blank>", "<pad>", "</s>", "<unk>", "a", "i", "e", "n", "o", "u", "t", "s", "r", "m", "k",
    "l", "d", "g", "h", "y", "b", "p", "w", "c", "v", "j", "z", "f", "'", "q", "x",
];

/// CTC blank index (position 0 in the vocabulary).
pub(crate) const BLANK: usize = 0;

/// The aligner session: loaded once, reused per utterance. `run`
/// needs `&mut` in ort rc.14, so the session sits behind a mutex.
pub struct WordAligner {
    _env: Arc<ort::environment::Environment>,
    session: Mutex<Session>,
}

/// Normalize text the way the reference does for Latin input:
/// lowercase, strip everything outside `a-z' `.
#[must_use]
pub fn normalize_text(text: &str) -> String {
    let lowered = text.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_space = true;
    for ch in lowered.chars() {
        if ch.is_ascii_lowercase() || ch == '\'' {
            out.push(ch);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
}

/// Map a normalized character to its token id (None for chars the
/// model cannot emit — they are skipped, exactly like the reference's
/// `if c in dictionary`).
#[must_use]
pub fn char_id(ch: char) -> Option<usize> {
    VOCAB.iter().position(|v| v.len() == 1 && v.starts_with(ch))
}

impl WordAligner {
    /// Load the aligner from an ONNX file (see the module docs for the
    /// conversion recipe and the license note).
    ///
    /// # Errors
    /// When the file cannot be loaded as an ONNX session.
    pub fn load(onnx_path: &Path) -> Result<Self, String> {
        fn err<E: std::fmt::Display>(e: E) -> String {
            e.to_string()
        }
        let env = Arc::new(ort::init().build().map_err(|e| format!("ort env: {e}"))?);
        let session = Session::builder(&env)
            .map_err(err)?
            .with_intra_threads(4)
            .map_err(err)?
            // pin CPU (floravox#17 class of issue: ort's default
            // EP selection can route graphs to accelerators that
            // fault on them; this model is CPU-fast anyway)
            .with_auto_device(ort::session::builder::AutoDevicePolicy::PreferCPU)
            .map_err(err)?
            .commit_from_file(onnx_path)
            .map_err(|e| format!("ort session from {}: {e}", onnx_path.display()))?;
        Ok(Self {
            _env: env,
            session: Mutex::new(session),
        })
    }

    /// Align `text` against `waveform` (16 kHz mono f32 in [-1, 1])
    /// and return per-word timings.
    ///
    /// # Errors
    /// When inference fails or the text produces no alignable tokens.
    pub fn align_words(&self, waveform: &[f32], text: &str) -> Result<Vec<WordSpan>, String> {
        fn err<E: std::fmt::Display>(e: E) -> String {
            e.to_string()
        }
        let tokens = tokenize(text);
        if tokens.is_empty() {
            return Err("no alignable tokens in text".into());
        }
        let wave =
            Tensor::from_array((vec![1usize, waveform.len()], waveform.to_vec())).map_err(err)?;
        let logits = {
            let mut session = self.session.lock().map_err(|_| "session poisoned")?;
            let outputs = session.run(ort::inputs!["waveform" => wave]).map_err(err)?;
            let (shape, data) = outputs["logits"].try_extract_tensor::<f32>().map_err(err)?;
            (shape.to_vec(), data.to_vec())
        };
        let shape = logits.0;
        let flat = logits.1;
        // [1, T, 31] -> [T][31] log_softmax.
        let t_len = shape[1] as usize;
        let n = shape[2] as usize;
        assert_eq!(n, 31, "unexpected vocab size");
        assert_eq!(flat.len(), t_len * n, "logits size");
        let mut emissions: Vec<Vec<f32>> = Vec::with_capacity(t_len);
        for frame in flat.chunks_exact(n) {
            let max = frame.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let sum: f32 = frame.iter().map(|v| (v - max).exp()).sum();
            let logz = sum.ln();
            emissions.push(frame.iter().map(|v| v - max - logz).collect());
        }

        let path = forced_align(&emissions, &tokens, BLANK);
        let segments = merge_repeats(&path);

        // Walk the words over the segments (greedy, per-char).
        let normalized = normalize_text(text);
        let words: Vec<&str> = normalized.split(' ').collect();
        let mut word_chars: Vec<Vec<usize>> = Vec::with_capacity(words.len());
        let mut cursor = 0usize;
        for w in &words {
            let mut idxs = Vec::new();
            for ch in w.chars() {
                let Some(id) = char_id(ch) else { continue };
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
        // Frames -> seconds from the OBSERVED frame count (the
        // reference: waveform_ms / T; the conv stack is 20 ms/frame).
        let t = emissions.len().max(1);
        // Sample/frame counts stay far below f32's mantissa.
        #[allow(clippy::cast_precision_loss)]
        let stride_secs = waveform.len() as f32 / 16_000.0 / t as f32;
        Ok(word_spans(&segments, &word_chars, BLANK, stride_secs))
    }
}

/// Load a WAV file as 16 kHz mono f32 in [-1, 1] (the aligner's input
/// format). Integer PCM is scaled; any channel count is downmixed.
///
/// # Errors
/// When the file cannot be read or decoded.
pub fn load_wav16k(path: &Path) -> Result<Vec<f32>, String> {
    let reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 {
        return Err(format!(
            "aligner input must be 16 kHz, got {} (resample first)",
            spec.sample_rate
        ));
    }
    let ch = spec.channels.max(1) as usize;
    let samples: Result<Vec<f32>, _> = reader
        .into_samples::<i16>()
        .map(|s| s.map(|v| f32::from(v) / 32_768.0))
        .collect();
    let flat = samples.map_err(|e| e.to_string())?;
    #[allow(clippy::cast_precision_loss)]
    let scale = 1.0 / ch as f32;
    Ok(flat
        .chunks(ch)
        .map(|c| c.iter().sum::<f32>() * scale)
        .collect())
}

/// Token ids for the normalized text (spaces dropped, unknown chars
/// skipped — the reference's `if c in dictionary`).
fn tokenize(text: &str) -> Vec<usize> {
    let normalized = normalize_text(text);
    let mut ids = Vec::new();
    for ch in normalized.chars() {
        if let Some(id) = char_id(ch) {
            ids.push(id);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_matches_reference() {
        // The reference: lowercase, non-[a-z' ] -> space, collapse.
        assert_eq!(
            normalize_text("Hello, this is the control probe."),
            "hello this is the control probe"
        );
        assert_eq!(normalize_text("It's fine."), "it's fine");
        assert_eq!(normalize_text("  spaced   out  "), "spaced out");
    }

    #[test]
    fn vocab_shape() {
        assert_eq!(VOCAB[0], "<blank>");
        assert_eq!(VOCAB[28], "'");
        assert_eq!(VOCAB.len(), 31);
        assert_eq!(char_id('a'), Some(4));
        assert_eq!(char_id('\''), Some(28));
        assert_eq!(char_id('A'), None); // normalize first
        assert_eq!(char_id('é'), None);
    }

    #[test]
    fn tokenize_skips_unknown() {
        assert_eq!(
            tokenize("hello, world!"),
            vec![
                char_id('h').unwrap(),
                char_id('e').unwrap(),
                char_id('l').unwrap(),
                char_id('l').unwrap(),
                char_id('o').unwrap(),
                char_id('w').unwrap(),
                char_id('o').unwrap(),
                char_id('r').unwrap(),
                char_id('l').unwrap(),
                char_id('d').unwrap(),
            ]
        );
    }

    /// End-to-end spike (issue #109): the full Rust pipeline — ort
    /// inference on the exported MMS ONNX + the Viterbi port — against
    /// the reference oracle's word timings on the same audio.
    ///
    /// ```text
    /// WORD_ALIGNER_MODEL=~/.cache/rust-tts-wrapper/mms-aligner \
    /// WORD_ALIGNER_ORACLE=~/.cache/rust-tts-wrapper/mms-aligner \
    ///     cargo test --features word-align --lib word_aligner -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the exported ONNX model + oracle dump"]
    fn end_to_end_matches_reference() {
        let Some(dir) = std::env::var_os("WORD_ALIGNER_ORACLE") else {
            eprintln!("WORD_ALIGNER_ORACLE not set; skipping");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let model = dir.join("mms-aligner.onnx");
        if !model.is_file() {
            eprintln!("no model at {}; run export_onnx.py first", model.display());
            return;
        }
        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("oracle.json")).expect("oracle.json"),
        )
        .expect("parse");
        let text = meta["text"].as_str().expect("text");
        let wav = dir.join("test16k.wav");

        let waveform = load_wav16k(&wav).expect("load wav");

        let aligner = WordAligner::load(&model).expect("load");
        let spans = aligner.align_words(&waveform, text).expect("align");

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
        let words: Vec<String> = meta["words"]
            .as_array()
            .expect("words")
            .iter()
            .map(|v| v.as_str().expect("str").to_string())
            .collect();

        assert_eq!(spans.len(), oracle.len(), "word count");
        let tol = 0.06; // a few frames: ort-vs-torch logits differ ~1e-4
        for (got, (wi, s, e)) in spans.iter().zip(&oracle) {
            assert_eq!(got.word_index, *wi);
            assert!(
                (got.start_secs - *s as f32).abs() <= tol,
                "start word {wi} ('{}'): {} vs {s}",
                words[*wi],
                got.start_secs
            );
            assert!(
                (got.end_secs - *e as f32).abs() <= tol,
                "end word {wi} ('{}'): {} vs {e}",
                words[*wi],
                got.end_secs
            );
            eprintln!(
                "  '{}': {:.3}..{:.3} (oracle {:.3}..{:.3})",
                words[*wi], got.start_secs, got.end_secs, s, e
            );
        }
        eprintln!("END-TO-END MATCH: {} words within {tol}s", spans.len());
    }
}
