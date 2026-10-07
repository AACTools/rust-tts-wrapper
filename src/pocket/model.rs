//! The tapped pocket model: loads the five ONNX graphs (lm_main PATCHED to
//! expose attention softmaxes — see floravox SPRINTS.md for the patch).
//!
//! The streaming loop follows the validated Python reference
//! (`run_tapped.py`): voice conditioning -> text conditioning -> per-step
//! lm_main (capturing attention over the text tokens) -> Euler-integrated
//! flow -> chunked mimi decode.

use crate::pocket::tokenizer::{AnyTokenizer, PocketTokenizer, WordLevelTokenizer};
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};

/// ort's `Session::run` takes `&mut self` for internal scratch; sessions are
/// semantically immutable (graph + weights fixed after load). This wrapper
/// exposes `&mut` access for run calls without exterior locking.
/// NOT thread-safe: one generation at a time per model (fine for TTS use).
struct RunCell(std::cell::UnsafeCell<Session>);
impl std::ops::Deref for RunCell {
    type Target = Session;
    fn deref(&self) -> &Session {
        unsafe { &*self.0.get() }
    }
}
impl RunCell {
    /// # Safety
    /// Callers must not run the same session concurrently.
    #[allow(clippy::mut_from_ref)]
    unsafe fn get_mut(&self) -> &mut Session {
        unsafe { &mut *self.0.get() }
    }
}
// Safety justification: ort sessions are semantically immutable (fixed graph
// and weights); `run` requires &mut only for internal scratch state. The
// model is used single-threaded per instance (TTS generation).
unsafe impl Send for RunCell {}
unsafe impl Sync for RunCell {}

/// Audio seconds per latent step (measured: 1.44s/18 steps; 2.56s/32 — ~0.08).
pub(crate) const STEP_SECONDS: f32 = 0.08;
pub(crate) const SAMPLE_RATE: u32 = 24_000;

pub struct PocketConfig {
    pub lm_main: PathBuf,
    pub lm_flow: PathBuf,
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub text_conditioner: PathBuf,
    pub vocab: Option<PathBuf>,
    pub token_scores: Option<PathBuf>,
    /// WordLevel tokenizer.json (phoneme bundles) — mutually exclusive with
    /// vocab/token_scores.
    pub tokenizer_json: Option<PathBuf>,
    /// `bos_before_voice` embedding prepended to voice conditioning
    /// (phoneme bundles ship it as .npy; sherpa bundles bake it into the
    /// encoder graph and ship nothing).
    pub bos: Option<PathBuf>,
}

impl PocketConfig {
    /// Bundle layouts: sherpa (lm_main*.onnx + vocab.json) or phoneme
    /// (flow_lm_main*.onnx + tokenizer json + bundle.json). Files directly
    /// in `dir`, or under `<dir>/<model-id>/`.
    #[must_use]
    pub fn from_dir(dir: &Path) -> Option<Self> {
        let sherpa = |d: &Path| {
            (d.join("lm_main_tapped.onnx").is_file() || d.join("lm_main.onnx").is_file())
                && d.join("vocab.json").is_file()
        };
        let phoneme = |d: &Path| {
            (d.join("flow_lm_main_tapped.onnx").is_file() || d.join("flow_lm_main.onnx").is_file())
                && d.join("bundle.json").is_file()
                && (d.join("tokenizer4.json").is_file() || d.join("tokenizer.json").is_file())
        };
        let base = if sherpa(dir) || phoneme(dir) {
            dir.to_path_buf()
        } else {
            let mut found = None;
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                if entry.path().is_dir() && (sherpa(&entry.path()) || phoneme(&entry.path())) {
                    found = Some(entry.path());
                    break;
                }
            }
            found?
        };
        if sherpa(&base) {
            // prefer the tapped graph when present
            let lm = if base.join("lm_main_tapped.onnx").is_file() {
                base.join("lm_main_tapped.onnx")
            } else {
                base.join("lm_main.onnx")
            };
            Some(Self {
                lm_main: lm,
                lm_flow: base.join("lm_flow.onnx"),
                encoder: base.join("encoder.onnx"),
                decoder: base.join("decoder.onnx"),
                text_conditioner: base.join("text_conditioner.onnx"),
                vocab: Some(base.join("vocab.json")),
                token_scores: Some(base.join("token_scores.json")),
                tokenizer_json: None,
                bos: None,
            })
        } else {
            let lm = if base.join("flow_lm_main_tapped.onnx").is_file() {
                base.join("flow_lm_main_tapped.onnx")
            } else {
                base.join("flow_lm_main.onnx")
            };
            let tok = if base.join("tokenizer4.json").is_file() {
                base.join("tokenizer4.json")
            } else {
                base.join("tokenizer.json")
            };
            let bos = base.join("bos_before_voice.npy");
            Some(Self {
                lm_main: lm,
                lm_flow: base.join("flow_lm_flow.onnx"),
                encoder: base.join("mimi_encoder.onnx"),
                decoder: base.join("mimi_decoder.onnx"),
                text_conditioner: base.join("text_conditioner.onnx"),
                vocab: None,
                token_scores: None,
                tokenizer_json: Some(tok),
                bos: bos.is_file().then_some(bos),
            })
        }
    }
}

pub struct GeneratedSpeech {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// per generation step: head-averaged attention over the TARGET tokens
    pub text_attention: Vec<Vec<f32>>,
    /// token ids of the target text (columns of text_attention)
    pub target_token_ids: Vec<u32>,
    /// leading KV positions occupied by the voice conditioning
    pub voice_len: usize,
    /// phoneme mode: explicit token->word grouping (None = derive from
    /// piece markers in `timings`)
    pub grouped: Option<(Vec<usize>, Vec<String>)>,
}

/// A raw state tensor fed back between lm_main/decoder calls.
#[derive(Clone)]
enum RawTensor {
    F32(Vec<f32>, Vec<i64>),
    I64(Vec<i64>, Vec<i64>),
    Bool(Vec<bool>, Vec<i64>),
}

pub struct PocketTtsModel {
    lm_main: RunCell,
    lm_flow: RunCell,
    encoder: RunCell,
    decoder: RunCell,
    conditioner: RunCell,
    pub tokenizer: AnyTokenizer,
    /// [1,1,1024] prepended to voice embeddings when the bundle ships it
    bos: Option<Vec<f32>>,
    lm_state_names: Vec<String>,
    lm_out_state_count: usize,
    /// attention-tap output names to average (6L: single Softmax tap;
    /// phoneme 24L: attn_tap_8..attn_tap_15 — measured 2026-10: layers
    /// 8-15 mean + per-token peak-step gives monotonic word order)
    lm_attn_names: Vec<String>,
    dec_state_names: Vec<String>,
    dec_out_state_count: usize,
    lm_init: Vec<RawTensor>,
    dec_init: Vec<RawTensor>,
}

fn shape_of(i: &ort::value::Outlet) -> Vec<i64> {
    match i.dtype() {
        ort::value::ValueType::Tensor { shape, .. } => shape.iter().copied().collect(),
        _ => Vec::new(),
    }
}

fn elem_is_i64(i: &ort::value::Outlet) -> bool {
    matches!(
        i.dtype(),
        ort::value::ValueType::Tensor {
            ty: ort::value::TensorElementType::Int64,
            ..
        }
    )
}

fn elem_is_bool(i: &ort::value::Outlet) -> bool {
    matches!(
        i.dtype(),
        ort::value::ValueType::Tensor {
            ty: ort::value::TensorElementType::Bool,
            ..
        }
    )
}

impl PocketTtsModel {
    /// Load the graphs + tokenizer.
    ///
    /// # Errors
    ///
    /// IO or ONNX session failures.
    #[allow(clippy::too_many_lines)]
    pub fn load(cfg: &PocketConfig) -> Result<Self, String> {
        fn err<E: std::fmt::Display>(e: E) -> String {
            e.to_string()
        }
        // small sequential matmuls in the AR loop oversubscribe with default
        // threading; cap intra-op (measured ~2x on the pocket graphs)
        fn mk_session(path: &Path) -> Result<Session, String> {
            Session::builder()
                .map_err(|e| e.to_string())?
                .with_intra_threads(6)
                .map_err(|e| e.to_string())?
                .with_inter_threads(1)
                .map_err(|e| e.to_string())?
                .commit_from_file(path)
                .map_err(err)
        }
        let lm_main = mk_session(&cfg.lm_main)?;
        let lm_flow = mk_session(&cfg.lm_flow)?;
        let encoder = mk_session(&cfg.encoder)?;
        let decoder = mk_session(&cfg.decoder)?;
        let conditioner = Session::builder()
            .map_err(err)?
            .commit_from_file(&cfg.text_conditioner)
            .map_err(err)?;
        let tokenizer = match (&cfg.vocab, &cfg.token_scores, &cfg.tokenizer_json) {
            (Some(v), Some(s), _) => AnyTokenizer::Viterbi(PocketTokenizer::load(v, s)?),
            (_, _, Some(t)) => AnyTokenizer::WordLevel(WordLevelTokenizer::load(t)?),
            _ => return Err("bundle has neither vocab.json nor tokenizer json".into()),
        };
        let bos = match &cfg.bos {
            Some(p) => match read_npy_f32(p) {
                Ok(v) if !v.is_empty() => Some(v),
                Ok(_) => None,
                Err(e) => return Err(format!("bos npy: {e}")),
            },
            None => None,
        };

        let lm_state_names = lm_main
            .inputs()
            .iter()
            .skip(2)
            .map(|i| i.name().to_string())
            .collect();
        let lm_out_state_count = lm_main
            .outputs()
            .iter()
            .filter(|o| o.name().starts_with("out_state"))
            .count();
        let tap_names: Vec<String> = lm_main
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .filter(|n| {
                n.as_str() != "conditioning"
                    && n.as_str() != "eos_logit"
                    && !n.starts_with("out_state")
            })
            .collect();
        let lm_attn_names = if let Some(six) = tap_names
            .iter()
            .find(|n| n.contains("layers.3") && n.contains("Softmax"))
        {
            vec![six.clone()]
        } else {
            // phoneme graphs: layers 8-15 mean (validated monotonic);
            // fall back to every tap if the suffix parse fails
            let picked: Vec<String> = tap_names
                .iter()
                .filter(|n| {
                    n.rsplit('_')
                        .next()
                        .and_then(|s| s.parse::<usize>().ok())
                        .is_some_and(|i| (8..16).contains(&i))
                })
                .cloned()
                .collect();
            if picked.len() == 8 {
                picked
            } else {
                tap_names.clone()
            }
        };
        let dec_state_names = decoder
            .inputs()
            .iter()
            .skip(1)
            .map(|i| i.name().to_string())
            .collect();
        let dec_out_state_count = decoder
            .outputs()
            .iter()
            .filter(|o| o.name().starts_with("out_state"))
            .count();

        // init states: KV caches keep declared dims; i64 counters [1]=0;
        // variable buffers (symbolic dims) start EMPTY (dim 0)
        let mk_raw = |dims_decl: &[i64], is_i64: bool, is_bool: bool| -> RawTensor {
            let mut dims: Vec<i64> = Vec::with_capacity(dims_decl.len());
            for d in dims_decl {
                if *d > 0 {
                    dims.push(*d);
                } else if is_i64 {
                    dims.push(1);
                } else {
                    dims.push(0);
                }
            }
            let n: usize = dims.iter().map(|d| (*d).max(0) as usize).product();
            if is_i64 {
                RawTensor::I64(vec![0i64; n], dims)
            } else if is_bool {
                RawTensor::Bool(vec![false; n], dims)
            } else {
                RawTensor::F32(vec![0f32; n], dims)
            }
        };
        let lm_init = lm_main
            .inputs()
            .iter()
            .skip(2)
            .map(|i| mk_raw(&shape_of(i), elem_is_i64(i), elem_is_bool(i)))
            .collect();
        let dec_init = decoder
            .inputs()
            .iter()
            .skip(1)
            .map(|i| mk_raw(&shape_of(i), elem_is_i64(i), elem_is_bool(i)))
            .collect();

        Ok(Self {
            lm_main: RunCell(std::cell::UnsafeCell::new(lm_main)),
            lm_flow: RunCell(std::cell::UnsafeCell::new(lm_flow)),
            encoder: RunCell(std::cell::UnsafeCell::new(encoder)),
            decoder: RunCell(std::cell::UnsafeCell::new(decoder)),
            conditioner: RunCell(std::cell::UnsafeCell::new(conditioner)),
            tokenizer,
            bos,
            lm_state_names,
            lm_out_state_count,
            lm_attn_names,
            dec_state_names,
            dec_out_state_count,
            lm_init,
            dec_init,
        })
    }

    /// Generate speech for `text` in the voice of `reference_audio`
    /// (24 kHz mono), capturing per-step text attention.
    ///
    /// # Errors
    ///
    /// ONNX session or shape failures at any stage.
    #[allow(
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation
    )]
    pub fn generate(
        &mut self,
        text: &str,
        reference_audio: &[f32],
        temperature: f32,
        num_steps: usize,
        max_frames: usize,
    ) -> Result<GeneratedSpeech, String> {
        let err = |e: ort::Error| e.to_string();
        // --- 1. text embeddings
        // phoneme bundles: `|`-separated phoneme words carry the word
        // grouping (needed for attention timings); plain text for Viterbi.
        let (tok_ids, grouped) = match &self.tokenizer {
            AnyTokenizer::WordLevel(t) => {
                // always grouped: `|`-separated words carry explicit
                // grouping; bare phoneme text becomes one word per token
                let (ids, tok2word, words) = t.encode_grouped(text);
                (ids, Some((tok2word, words)))
            }
            AnyTokenizer::Viterbi(_) => (self.tokenizer.encode(text), None),
        };
        if tok_ids.is_empty() {
            return Err("empty text".into());
        }
        let ids_arr = Tensor::from_array((
            vec![1usize, tok_ids.len()],
            tok_ids.iter().map(|t| i64::from(*t)).collect::<Vec<i64>>(),
        ))
        .map_err(err)?;
        let (emb_shape, emb) = {
            let emb_out = unsafe { self.conditioner.get_mut() }
                .run(ort::inputs!["token_ids" => &ids_arr])
                .map_err(err)?;
            let emb_v = emb_out.get("embeddings").ok_or("no embeddings output")?;
            let (shape, data) = emb_v.try_extract_tensor::<f32>().map_err(err)?;
            (
                shape.iter().map(|d| *d as usize).collect::<Vec<usize>>(),
                data.to_vec(),
            )
        };
        let text_len = tok_ids.len();

        // --- 2. voice conditioning
        let audio_arr = Tensor::from_array((
            vec![1usize, 1usize, reference_audio.len()],
            reference_audio.to_vec(),
        ))
        .map_err(err)?;
        let (vshape, voice) = {
            let vout = unsafe { self.encoder.get_mut() }
                .run(ort::inputs![&audio_arr])
                .map_err(err)?;
            let v_v = vout.get("latents").ok_or("no latents output")?;
            let (shape, data) = v_v.try_extract_tensor::<f32>().map_err(err)?;
            (
                shape.iter().map(|d| *d as usize).collect::<Vec<usize>>(),
                data.to_vec(),
            )
        };
        // encoder output is [1, voice_len, 1024]
        let voice_len = if vshape.len() >= 2 {
            vshape[vshape.len() - 2]
        } else {
            0
        };
        // phoneme bundles: prepend the bos_before_voice row ([1,1,1024])
        let (voice, voice_len) = match &self.bos {
            Some(b) => {
                let mut v = Vec::with_capacity(b.len() + voice.len());
                v.extend_from_slice(b);
                v.extend_from_slice(&voice);
                (v, voice_len + 1)
            }
            None => (voice, voice_len),
        };
        let voice_dims = vec![1i64, voice_len as i64, 1024];

        // --- 3. streaming loop
        let lm_names: Vec<String> = lm_out_state_names(self);
        let mut state: Vec<RawTensor> = self.lm_init.clone();
        let seq_empty_data: Vec<f32> = Vec::new();
        // voice conditioning pass
        {
            self.run_lm_step(
                &lm_names,
                (seq_empty_data.clone(), vec![1i64, 0, 32]),
                (&voice, &voice_dims),
                &mut state,
            )?;
        }
        // text conditioning pass
        {
            let emb_dims = vec![
                1i64,
                emb_shape[emb_shape.len().saturating_sub(2)] as i64,
                emb_shape[emb_shape.len() - 1] as i64,
            ];
            self.run_lm_step(
                &lm_names,
                (seq_empty_data.clone(), vec![1i64, 0, 32]),
                (&emb, &emb_dims),
                &mut state,
            )?;
        }
        let _ = text_len;

        // generation loop
        let mut latents: Vec<f32> = Vec::new();
        let mut attn_steps: Vec<Vec<f32>> = Vec::new();
        let mut eos_step: i64 = -1;
        let mut cur = vec![f32::NAN; 32];
        let noise_scale = temperature.sqrt();
        let mut rng: u64 = 0x853c_49e6_748f_ea9b; // splitmix64
        let mut next_f32 = || -> f32 {
            rng = rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = rng;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            let u = (z >> 11) as f64 * (1.0 / (1u64 << 53) as f64);
            (u * 2.0 - 1.0) as f32
        };
        let empty_emb: Vec<f32> = Vec::new();
        // Kyutai-calibrated budget: (tokens/3 + 2s) of frames, hard-capped
        // by the caller. A missed EOS cannot ramble past ~1.3x the estimate.
        let est_frames = ((tok_ids.len() as f32 / 3.0) + 2.0) * 12.5;
        let frame_budget = max_frames.min((est_frames * 1.3).ceil() as usize);
        let mut eos_fired = false;
        for step in 0..frame_budget {
            let (cond, eos, attn) = self.run_lm_step(
                &lm_names,
                (cur.clone(), vec![1i64, 1, 32]),
                (&empty_emb, &[1i64, 0, 1024]),
                &mut state,
            )?;
            if eos_step < 0 && eos > -4.0 {
                eos_step = step as i64;
                eos_fired = true;
            }
            if eos_step >= 0 && step as i64 >= eos_step + 3 {
                break;
            }
            attn_steps.push(attn);
            // Euler flow integration
            let mut x: Vec<f32> = (0..32).map(|_| next_f32() * noise_scale).collect();
            let dt = 1.0f32 / num_steps as f32;
            for i in 0..num_steps {
                let s = i as f32 / num_steps as f32;
                let cond_t =
                    Tensor::from_array((vec![1usize, 1024usize], cond.clone())).map_err(err)?;
                let s_t = Tensor::from_array((vec![1usize, 1usize], vec![s])).map_err(err)?;
                let t_t = Tensor::from_array((vec![1usize, 1usize], vec![s + dt])).map_err(err)?;
                let x_t = Tensor::from_array((vec![1usize, 32usize], x.clone())).map_err(err)?;
                let fo = unsafe { self.lm_flow.get_mut() }
                    .run(ort::inputs!["c" => &cond_t, "s" => &s_t, "t" => &t_t, "x" => &x_t])
                    .map_err(err)?;
                let f: Vec<f32> = fo
                    .into_iter()
                    .next()
                    .map(|(_, v)| v)
                    .ok_or("flow out")?
                    .try_extract_tensor::<f32>()
                    .map_err(err)?
                    .1
                    .to_vec();
                for (xi, fi) in x.iter_mut().zip(f) {
                    *xi += dt * fi;
                }
            }
            cur.clone_from(&x);
            latents.extend_from_slice(&x);
        }

        // slice the attention rows to the target window
        let attn_target: Vec<Vec<f32>> = attn_steps
            .iter()
            .map(|row| {
                row.get(voice_len..voice_len + text_len)
                    .map(<[f32]>::to_vec)
                    .unwrap_or_default()
            })
            .collect();

        // --- 4. chunked mimi decode
        let frame = 32usize;
        let n_frames = latents.len() / frame;
        let chunk = 15usize;
        let mut samples: Vec<f32> = Vec::new();
        let mut dstate: Vec<RawTensor> = self.dec_init.clone();
        let mut i = 0usize;
        while i * chunk < n_frames {
            let this_chunk = chunk.min(n_frames - i * chunk);
            let start = i * chunk * frame;
            let mut chunk_data = Vec::with_capacity(this_chunk * frame);
            chunk_data.extend_from_slice(&latents[start..start + this_chunk * frame]);
            let ct =
                Tensor::from_array((vec![1usize, this_chunk, frame], chunk_data)).map_err(err)?;
            let mut inputs: Vec<(
                std::borrow::Cow<'_, str>,
                ort::session::SessionInputValue<'_>,
            )> = Vec::new();
            inputs.push(("latent".into(), (&ct).into()));
            let mut dstate_in = std::mem::take(&mut dstate);
            for (name, raw) in self.dec_state_names.iter().zip(dstate_in.drain(..)) {
                let t = raw.into_tensor()?;
                inputs.push((name.as_str().into(), t));
            }
            let outs = unsafe { self.decoder.get_mut() }.run(inputs).map_err(err)?;
            let audio: Vec<f32> = outs
                .get("audio_frame")
                .ok_or("no audio_frame")?
                .try_extract_tensor::<f32>()
                .map_err(err)?
                .1
                .to_vec();
            samples.extend_from_slice(&audio);
            let mut new_state = Vec::with_capacity(self.dec_out_state_count);
            for oname in dec_out_state_names(self) {
                if let Some(o) = outs.get(&oname) {
                    new_state.push(RawTensor::from_dyn(o).map_err(err)?);
                }
            }
            dstate = new_state;
            i += 1;
        }

        // Ramble guard: EOS never fired -> the tail past the spoken estimate
        // is looping babble. Trim to ~est duration with a short fade.
        if !eos_fired {
            let est_samples = (est_frames * 0.08 * SAMPLE_RATE as f32) as usize;
            if samples.len() > est_samples {
                let cut = est_samples.min(samples.len());
                let fade = (0.15 * SAMPLE_RATE as f32) as usize;
                for (i, s) in samples[cut.saturating_sub(fade)..cut]
                    .iter_mut()
                    .enumerate()
                {
                    #[allow(clippy::cast_precision_loss)]
                    let g = 1.0 - (i as f32 / fade as f32);
                    *s *= g;
                }
                samples.truncate(cut);
            }
        }

        Ok(GeneratedSpeech {
            samples,
            sample_rate: SAMPLE_RATE,
            text_attention: attn_target,
            target_token_ids: tok_ids,
            voice_len,
            grouped,
        })
    }
}

fn lm_out_state_names(m: &PocketTtsModel) -> Vec<String> {
    m.lm_main
        .outputs()
        .iter()
        .filter(|o| o.name().starts_with("out_state"))
        .map(|o| o.name().to_string())
        .collect()
}

fn dec_out_state_names(m: &PocketTtsModel) -> Vec<String> {
    m.decoder
        .outputs()
        .iter()
        .filter(|o| o.name().starts_with("out_state"))
        .map(|o| o.name().to_string())
        .collect()
}

impl RawTensor {
    fn into_tensor(self) -> Result<ort::session::SessionInputValue<'static>, String> {
        match self {
            RawTensor::F32(data, dims) => Ok(Tensor::from_array((dims, data))
                .map_err(|e| e.to_string())?
                .into()),
            RawTensor::I64(data, dims) => Ok(Tensor::from_array((dims, data))
                .map_err(|e| e.to_string())?
                .into()),
            RawTensor::Bool(data, dims) => Ok(Tensor::from_array((dims, data))
                .map_err(|e| e.to_string())?
                .into()),
        }
    }
}
impl PocketTtsModel {
    #[allow(clippy::too_many_arguments)]
    fn run_lm_step(
        &self,
        lm_out_names: &[String],
        seq: (Vec<f32>, Vec<i64>),
        embeddings: (&[f32], &[i64]),
        state: &mut Vec<RawTensor>,
    ) -> Result<(Vec<f32>, f32, Vec<f32>), String> {
        let err = |e: ort::Error| e.to_string();
        let mut inputs: Vec<(
            std::borrow::Cow<'_, str>,
            ort::session::SessionInputValue<'_>,
        )> = Vec::new();
        let seq_t = Tensor::from_array((seq.1, seq.0)).map_err(err)?;
        inputs.push(("sequence".into(), (&seq_t).into()));
        let emb_t =
            Tensor::from_array((embeddings.1.to_vec(), embeddings.0.to_vec())).map_err(err)?;
        inputs.push(("text_embeddings".into(), (&emb_t).into()));
        let mut state_in = std::mem::take(state);
        for (name, raw) in self.lm_state_names.iter().zip(state_in.drain(..)) {
            let t = raw.into_tensor()?;
            inputs.push((name.as_str().into(), t));
        }
        let outs = unsafe { self.lm_main.get_mut() }.run(inputs).map_err(err)?;
        let mut new_state = Vec::with_capacity(lm_out_names.len());
        for oname in lm_out_names {
            if let Some(o) = outs.get(oname) {
                new_state.push(RawTensor::from_dyn(o).map_err(err)?);
            }
        }
        *state = new_state;
        let cond: Vec<f32> = outs
            .get("conditioning")
            .ok_or("no conditioning")?
            .try_extract_tensor::<f32>()
            .map_err(err)?
            .1
            .to_vec();
        let eos = outs
            .get("eos_logit")
            .ok_or("no eos")?
            .try_extract_tensor::<f32>()
            .map_err(err)?
            .1
            .first()
            .copied()
            .unwrap_or(-99.0);
        // average the selected taps (heads within each, then across taps)
        let mut attn_row = Vec::new();
        let mut taps_used = 0usize;
        for aname in &self.lm_attn_names {
            let Some(o) = outs.get(aname) else { continue };
            let (a_shape, a_data) = o.try_extract_tensor::<f32>().map_err(err)?;
            let dims: Vec<usize> = a_shape.iter().map(|d| *d as usize).collect();
            if dims.len() != 4 {
                continue;
            }
            let heads = dims[1];
            let kv = dims[3];
            if attn_row.len() != kv {
                attn_row = vec![0f32; kv];
            }
            for h in 0..heads {
                for k in 0..kv {
                    attn_row[k] += a_data[h * kv + k];
                }
            }
            taps_used += heads.max(1);
        }
        if taps_used > 0 {
            #[allow(clippy::cast_precision_loss)]
            let n = taps_used as f32;
            for r in &mut attn_row {
                *r /= n;
            }
        }
        Ok((cond, eos, attn_row))
    }
}

impl RawTensor {
    fn from_dyn(v: &ort::value::DynValue) -> Result<Self, ort::Error> {
        let t = v.try_extract_tensor::<f32>();
        if let Ok(t) = t {
            let dims: Vec<i64> = t.0.iter().copied().collect();
            return Ok(RawTensor::F32(t.1.to_vec(), dims));
        }
        let t = v.try_extract_tensor::<i64>();
        if let Ok(t) = t {
            let dims: Vec<i64> = t.0.iter().copied().collect();
            return Ok(RawTensor::I64(t.1.to_vec(), dims));
        }
        let t = v.try_extract_tensor::<bool>();
        if let Ok(t) = t {
            let dims: Vec<i64> = t.0.iter().copied().collect();
            return Ok(RawTensor::Bool(t.1.to_vec(), dims));
        }
        Err(ort::Error::new_with_code(
            ort::ErrorCode::InvalidArgument,
            String::from("unsupported state dtype"),
        ))
    }
}

/// Minimal .npy reader for a flat f32 array (little-endian, C order) —
/// enough for bos_before_voice.npy.
fn read_npy_f32(path: &Path) -> Result<Vec<f32>, String> {
    let b = std::fs::read(path).map_err(|e| e.to_string())?;
    if b.len() < 10 || &b[..6] != b"\x93NUMPY" {
        return Err("not an npy file".into());
    }
    let hlen = u16::from_le_bytes([b[8], b[9]]) as usize;
    let header = std::str::from_utf8(&b[10..10 + hlen]).map_err(|e| e.to_string())?;
    if !header.contains("'<f4'") {
        return Err("npy is not f32".into());
    }
    let data = &b[10 + hlen..];
    Ok(data
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}
