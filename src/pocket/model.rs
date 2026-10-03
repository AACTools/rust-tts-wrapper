//! The tapped pocket model: loads the five ONNX graphs (lm_main PATCHED to
//! expose attention softmaxes — see floravox SPRINTS.md for the patch).
//!
//! STATUS: tokenizer (complete, tested against the Python reference) +
//! graph loading + config. The native streaming loop (state threading ×
//! 18 tensors, Euler flow, chunked mimi decode) is the remaining port —
//! the validated Python reference is ~/models/pocket-onnx/run_tapped.py
//! and the wasm-native equivalent lives in the js crate's engine.

use crate::pocket::tokenizer::PocketTokenizer;
use ort::session::Session;
use std::path::{Path, PathBuf};

pub struct PocketConfig {
    pub lm_main: PathBuf,
    pub lm_flow: PathBuf,
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub text_conditioner: PathBuf,
    pub vocab: PathBuf,
    pub token_scores: PathBuf,
}

impl PocketConfig {
    /// Sherpa's standard bundle layout: files directly in `dir`, or under
    /// `<dir>/<model-id>/` (our fleet convention).
    pub fn from_dir(dir: &Path) -> Option<Self> {
        let probe = |d: &Path| d.join("lm_main.onnx").is_file() && d.join("vocab.json").is_file();
        let base = if probe(dir) {
            dir.to_path_buf()
        } else {
            let mut found = None;
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                if entry.path().is_dir() && probe(&entry.path()) {
                    found = Some(entry.path());
                    break;
                }
            }
            found?
        };
        Some(Self {
            lm_main: base.join("lm_main_tapped.onnx"),
            lm_flow: base.join("lm_flow.onnx"),
            encoder: base.join("encoder.onnx"),
            decoder: base.join("decoder.onnx"),
            text_conditioner: base.join("text_conditioner.onnx"),
            vocab: base.join("vocab.json"),
            token_scores: base.join("token_scores.json"),
        })
    }
}

pub struct PocketTtsModel {
    _lm_main: Session,
    _lm_flow: Session,
    _encoder: Session,
    _decoder: Session,
    _conditioner: Session,
    pub tokenizer: PocketTokenizer,
}

impl PocketTtsModel {
    pub fn load(cfg: &PocketConfig) -> Result<Self, String> {
        let err = |e: ort::Error| e.to_string();
        Ok(Self {
            _lm_main: Session::builder()
                .map_err(err)?
                .commit_from_file(&cfg.lm_main)
                .map_err(err)?,
            _lm_flow: Session::builder()
                .map_err(err)?
                .commit_from_file(&cfg.lm_flow)
                .map_err(err)?,
            _encoder: Session::builder()
                .map_err(err)?
                .commit_from_file(&cfg.encoder)
                .map_err(err)?,
            _decoder: Session::builder()
                .map_err(err)?
                .commit_from_file(&cfg.decoder)
                .map_err(err)?,
            _conditioner: Session::builder()
                .map_err(err)?
                .commit_from_file(&cfg.text_conditioner)
                .map_err(err)?,
            tokenizer: PocketTokenizer::load(&cfg.vocab, &cfg.token_scores)?,
        })
    }
}
