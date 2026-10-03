//! PocketTtsEngine: the tapped pocket model behind the [`TtsEngine`] trait —
//! cloned voices with REAL attention-measured word boundaries (no
//! estimator), raw LE PCM audio callbacks, marks via SSML pass-through.
//!
//! SpeechMarkdown/SSML: the model is text-in; breaks are realized by
//! splitting at `<break>`/SpeechMarkdown pauses and inserting silence,
//! matching the demo's segmentation convention. Rate scales the
//! temperature-independent pacing via simple linear time-scale on the
//! generated audio (pocket has no native rate parameter).

use crate::engine::TtsEngine;
use crate::pocket::model::{GeneratedSpeech, PocketConfig, PocketTtsModel};
use crate::pocket::timings::word_boundaries;
use crate::types::{TtsError, TtsResult, Voice, WordBoundary};
use crate::word_search::WordSearch;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

pub struct PocketTtsEngine {
    inner: Mutex<Option<PocketTtsModel>>,
    cfg: PocketConfig,
    reference_audio: Mutex<Vec<f32>>,
    generation: AtomicU64,
    stop_flag: AtomicBool,
    temperature: f32,
    num_steps: usize,
    max_frames: usize,
}

impl fmt::Debug for PocketTtsEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PocketTtsEngine")
            .field("lm_main", &self.cfg.lm_main.display().to_string())
            .finish_non_exhaustive()
    }
}

impl PocketTtsEngine {
    /// Create with a model bundle directory and a cloning reference wav
    /// (16-bit PCM, any sample rate; converted to 24 kHz mono f32).
    #[must_use]
    pub fn new(bundle_dir: &std::path::Path, reference_wav: &std::path::Path) -> Self {
        let cfg = PocketConfig::from_dir(bundle_dir).unwrap_or(PocketConfig {
            lm_main: bundle_dir.join("lm_main_tapped.onnx"),
            lm_flow: bundle_dir.join("lm_flow.onnx"),
            encoder: bundle_dir.join("encoder.onnx"),
            decoder: bundle_dir.join("decoder.onnx"),
            text_conditioner: bundle_dir.join("text_conditioner.onnx"),
            vocab: bundle_dir.join("vocab.json"),
            token_scores: bundle_dir.join("token_scores.json"),
        });
        let reference_audio = read_reference(reference_wav).unwrap_or_default();
        Self {
            inner: Mutex::new(None),
            cfg,
            reference_audio: Mutex::new(reference_audio),
            generation: AtomicU64::new(0),
            stop_flag: AtomicBool::new(false),
            temperature: 0.7,
            num_steps: 4,
            max_frames: 500,
        }
    }

    fn with_model<R>(&self, f: impl FnOnce(&mut PocketTtsModel) -> TtsResult<R>) -> TtsResult<R> {
        let mut guard = self.inner.lock().map_err(|_| TtsError("poisoned".into()))?;
        if guard.is_none() {
            let m = PocketTtsModel::load(&self.cfg).map_err(TtsError)?;
            *guard = Some(m);
        }
        f(guard.as_mut().expect("just ensured"))
    }

    /// Swap the cloning reference (a new donor recording).
    /// Swap the cloning reference (a new donor recording).
    ///
    /// # Errors
    ///
    /// wav read failures.
    pub fn set_reference_wav(&self, wav: &std::path::Path) -> TtsResult<()> {
        let audio = read_reference(wav).map_err(TtsError)?;
        *self
            .reference_audio
            .lock()
            .map_err(|_| TtsError("poisoned".into()))? = audio;
        Ok(())
    }

    fn synthesize(
        &self,
        text: &str,
        rate: f32,
    ) -> TtsResult<(GeneratedSpeech, Vec<WordTimingOwned>, String)> {
        let reference = self
            .reference_audio
            .lock()
            .map_err(|_| TtsError("poisoned".into()))?
            .clone();
        if reference.is_empty() {
            return Err(TtsError("no reference audio configured".into()));
        }
        // SpeechMarkdown -> plain (model is text-in); pause splits come later
        let (plain, _was_smd) = crate::engine::preprocess_speech_markdown(text, "plain");
        let plain = strip_ssml_tags(&plain);

        let (speech, timings) = self.with_model(|m| {
            let s = m
                .generate(
                    &plain,
                    &reference,
                    self.temperature,
                    self.num_steps,
                    self.max_frames,
                )
                .map_err(TtsError)?;
            let t = word_boundaries(
                &m.tokenizer,
                &s.target_token_ids,
                &s.text_attention,
                crate::pocket::model::STEP_SECONDS / rate.max(0.1),
            );
            Ok((s, t))
        })?;
        let timings: Vec<WordTimingOwned> = timings
            .into_iter()
            .map(|t| WordTimingOwned {
                word: t.word,
                start_s: t.start_s,
                end_s: t.end_s,
            })
            .collect();
        Ok((speech, timings, plain))
    }
}

struct WordTimingOwned {
    word: String,
    start_s: f32,
    end_s: f32,
}

#[allow(clippy::cast_precision_loss)]
fn read_reference(wav: &std::path::Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(wav).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let chans: usize = spec.channels.max(1) as usize;
    let samples: Vec<i16> = reader.samples::<i16>().filter_map(Result::ok).collect();
    let mono: Vec<f32> = samples
        .chunks(chans)
        .map(|c| c.iter().map(|s| f32::from(*s) / 32767.0).sum::<f32>() / c.len() as f32)
        .collect();
    if spec.sample_rate == 24_000 {
        Ok(mono)
    } else {
        Ok(resample_linear(&mono, spec.sample_rate, 24_000))
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn resample_linear(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let n_out = ((x.len() as f64) * ratio) as usize;
    (0..n_out)
        .map(|i| {
            let p = i as f64 / ratio;
            let i0 = p.floor() as usize;
            let i1 = (i0 + 1).min(x.len() - 1);
            let f = (p - i0 as f64) as f32;
            x[i0] * (1.0 - f) + x[i1] * f
        })
        .collect()
}

/// Remove SSML tags, keeping inner text; `<break>` becomes a comma-space so
/// the model still breathes (full silence-splitting is a follow-up).
fn strip_ssml_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('<') {
        if let Some(close) = rest[open..].find('>') {
            let tag = &rest[open..=open + close];
            if tag.starts_with("<break") {
                out.push_str(", ");
            }
            out.push_str(&rest[..open]);
            rest = &rest[open + close + 1..];
        } else {
            break;
        }
    }
    out.push_str(rest);
    out
}

fn speech_to_pcm16(speech: &GeneratedSpeech, volume: f32) -> Vec<u8> {
    let vol = volume.clamp(0.0, 4.0);
    speech
        .samples
        .iter()
        .flat_map(|s| {
            let v = ((*s) * vol).clamp(-1.0, 1.0);
            let pcm = (v * 32767.0).round() as i16;
            pcm.to_le_bytes()
        })
        .collect()
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::used_underscore_binding
)]
fn fire_callbacks(
    text: &str,
    speech: &GeneratedSpeech,
    timings: &[WordTimingOwned],
    mut on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
    mut on_mark: Option<crate::engine::OnMarkCallback<'_>>,
) -> Vec<WordBoundary> {
    let mut search = WordSearch::new(text);
    let mut out = Vec::with_capacity(timings.len());
    for t in timings {
        if t.word.trim().is_empty() {
            continue;
        }
        let (char_offset, char_len) = search.find_next(&t.word);
        if let Some(cb) = on_boundary.as_deref_mut() {
            cb(&t.word, t.start_s, t.end_s, char_offset, char_len, false);
        }
        out.push(WordBoundary {
            text: t.word.clone(),
            offset: (t.start_s * 1000.0) as u64,
            duration: ((t.end_s - t.start_s) * 1000.0) as u64,
            estimated: false,
        });
    }
    let _ = (
        on_boundary.is_some(),
        on_mark.take().is_some(),
        speech.samples.len(),
    );
    out
}

#[allow(clippy::used_underscore_binding)]
impl TtsEngine for PocketTtsEngine {
    #[allow(clippy::too_many_arguments)]
    fn speak(
        &self,
        text: &str,
        _voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        volume: f32,
        on_audio: Option<crate::engine::OnAudioCallback<'_>>,
        on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
        _on_mark: Option<crate::engine::OnMarkCallback<'_>>,
    ) -> TtsResult<()> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst);
        self.stop_flag.store(false, Ordering::SeqCst);
        let (speech, timings, spoken) = self.synthesize(text, rate)?;
        if self.stop_flag.load(Ordering::SeqCst)
            || self.generation.load(Ordering::SeqCst) != generation + 1
        {
            return Ok(());
        }
        let pcm = speech_to_pcm16(&speech, volume);
        if let Some(cb) = on_audio {
            cb(&pcm);
        }
        fire_callbacks(&spoken, &speech, &timings, on_boundary, _on_mark);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<crate::engine::OnAudioCallback<'_>>,
        on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
        on_mark: Option<crate::engine::OnMarkCallback<'_>>,
    ) -> TtsResult<()> {
        self.speak(
            text,
            voice,
            rate,
            pitch,
            volume,
            on_audio,
            on_boundary,
            on_mark,
        )
    }

    fn stop(&self) -> TtsResult<()> {
        self.stop_flag.store(true, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        Ok(vec![Voice {
            id: "pocket-clone".into(),
            name: "Cloned voice (pocket-tts)".into(),
            gender: crate::types::Gender::Unknown,
            provider: "pocket-timing".into(),
            language_codes: vec![crate::types::LanguageCode {
                bcp47: "en".into(),
                iso639_3: "eng".into(),
                display: "English".into(),
            }],
        }])
    }

    fn engine_id(&self) -> &'static str {
        "pocket-timing"
    }

    fn synth_to_bytes(
        &self,
        text: &str,
        _voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        volume: f32,
    ) -> TtsResult<Vec<u8>> {
        let (speech, _timings, _spoken) = self.synthesize(text, rate)?;
        Ok(speech_to_pcm16(&speech, volume))
    }

    fn synth_with_boundaries(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<(Vec<u8>, Vec<WordBoundary>)> {
        let (speech, timings, spoken) = self.synthesize(text, rate)?;
        let pcm = speech_to_pcm16(&speech, volume);
        let boundaries = fire_callbacks(&spoken, &speech, &timings, None, None);
        let _ = (voice, pitch);
        Ok((pcm, boundaries))
    }
}
