//! The tapped pocket model: loads the five ONNX graphs (lm_main PATCHED to
//! expose attention softmaxes — see floravox SPRINTS.md for the patch).
//!
//! The streaming loop follows the validated Python reference
//! (`run_tapped.py`): voice conditioning -> text conditioning -> per-step
//! lm_main (capturing attention over the text tokens) -> Euler-integrated
//! flow -> chunked mimi decode.

use crate::pocket::tokenizer::PocketTokenizer;
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

/// Audio seconds per latent step (measured on the Python reference:
/// 1.44s audio / 18 steps; 2.56s / 32 steps — both ~0.08).
pub const STEP_SECONDS: f32 = 0.08;
pub const SAMPLE_RATE: u32 = 24_000;

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
    #[must_use]
    pub fn from_dir(dir: &Path) -> Option<Self> {
        let probe = |d: &Path| {
            d.join("lm_main_tapped.onnx").is_file() && d.join("vocab.json").is_file()
                || d.join("lm_main.onnx").is_file() && d.join("vocab.json").is_file()
        };
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
            vocab: base.join("vocab.json"),
            token_scores: base.join("token_scores.json"),
        })
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
    pub tokenizer: PocketTokenizer,
    lm_state_names: Vec<String>,
    lm_out_state_count: usize,
    lm_attn_name: Option<String>,
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
    pub fn load(cfg: &PocketConfig) -> Result<Self, String> {
        let err = |e: ort::Error| e.to_string();
        let lm_main = Session::builder()
            .map_err(err)?
            .commit_from_file(&cfg.lm_main)
            .map_err(err)?;
        let lm_flow = Session::builder()
            .map_err(err)?
            .commit_from_file(&cfg.lm_flow)
            .map_err(err)?;
        let encoder = Session::builder()
            .map_err(err)?
            .commit_from_file(&cfg.encoder)
            .map_err(err)?;
        let decoder = Session::builder()
            .map_err(err)?
            .commit_from_file(&cfg.decoder)
            .map_err(err)?;
        let conditioner = Session::builder()
            .map_err(err)?
            .commit_from_file(&cfg.text_conditioner)
            .map_err(err)?;
        let tokenizer = PocketTokenizer::load(&cfg.vocab, &cfg.token_scores)?;

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
        let lm_attn_name = lm_main
            .outputs()
            .iter()
            .find(|o| o.name().contains("layers.3") && o.name().contains("Softmax"))
            .map(|o| o.name().to_string());
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
            lm_state_names,
            lm_out_state_count,
            lm_attn_name,
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
        let tok_ids = self.tokenizer.encode(text);
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
        for step in 0..max_frames {
            let (cond, eos, attn) = self.run_lm_step(
                &lm_names,
                (cur.clone(), vec![1i64, 1, 32]),
                (&empty_emb, &[1i64, 0, 1024]),
                &mut state,
            )?;
            if eos_step < 0 && eos > -4.0 {
                eos_step = step as i64;
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
            for (name, raw) in self.dec_state_names.iter().zip(dstate.iter()) {
                let t = raw.to_tensor()?;
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

        Ok(GeneratedSpeech {
            samples,
            sample_rate: SAMPLE_RATE,
            text_attention: attn_target,
            target_token_ids: tok_ids,
            voice_len,
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
    fn to_tensor(&self) -> Result<ort::session::SessionInputValue<'static>, String> {
        match self {
            RawTensor::F32(data, dims) => Ok(Tensor::from_array((dims.clone(), data.clone()))
                .map_err(|e| e.to_string())?
                .into()),
            RawTensor::I64(data, dims) => Ok(Tensor::from_array((dims.clone(), data.clone()))
                .map_err(|e| e.to_string())?
                .into()),
            RawTensor::Bool(data, dims) => Ok(Tensor::from_array((dims.clone(), data.clone()))
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
        for (name, raw) in self.lm_state_names.iter().zip(state.iter()) {
            let t = raw.to_tensor()?;
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
        let mut attn_row = Vec::new();
        if let Some(aname) = &self.lm_attn_name {
            if let Some(o) = outs.get(aname) {
                let (a_shape, a_data) = o.try_extract_tensor::<f32>().map_err(err)?;
                let dims: Vec<usize> = a_shape.iter().map(|d| *d as usize).collect();
                if dims.len() == 4 {
                    let heads = dims[1];
                    let kv = dims[3];
                    let mut row = vec![0f32; kv];
                    for h in 0..heads {
                        for k in 0..kv {
                            row[k] += a_data[h * kv + k];
                        }
                    }
                    for r in &mut row {
                        #[allow(clippy::cast_precision_loss)]
                        let hn = heads as f32;
                        *r /= hn;
                    }
                    attn_row = row;
                }
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
