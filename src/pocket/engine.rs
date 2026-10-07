//! PocketTtsEngine: the tapped pocket model behind the [`TtsEngine`] trait —
//! cloned voices with REAL attention-measured word boundaries (no
//! estimator), raw LE PCM audio callbacks.
//!
//! SpeechMarkdown/SSML: compiled to a floravox-ssml plan; `<break>`
//! pauses become real silence between synthesized segments, `<mark>`
//! events fire at their exact inter-segment positions.

use crate::engine::TtsEngine;
use crate::pocket::model::{GeneratedSpeech, PocketConfig, PocketTtsModel};
use crate::pocket::timings::{word_boundaries, word_boundaries_grouped};
use crate::types::{TtsError, TtsResult, Voice, WordBoundary};
use crate::word_search::WordSearch;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// A mark event with its position in the assembled audio.
pub struct MarkTiming {
    pub name: String,
    pub t_s: f32,
}

/// The fully-assembled result: audio + word timings + marks.
pub struct SynthesisResult {
    pub speech: GeneratedSpeech,
    pub words: Vec<WordTimingOwned>,
    pub marks: Vec<MarkTiming>,
    /// the spoken text (SSML stripped) — for char-mapping callbacks
    pub spoken_text: String,
}

pub struct WordTimingOwned {
    pub word: String,
    pub start_s: f32,
    pub end_s: f32,
}

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
    pub fn new(bundle_dir: &Path, reference_wav: &Path) -> Self {
        let cfg = PocketConfig::from_dir(bundle_dir).unwrap_or(PocketConfig {
            lm_main: bundle_dir.join("lm_main_tapped.onnx"),
            lm_flow: bundle_dir.join("lm_flow.onnx"),
            encoder: bundle_dir.join("encoder.onnx"),
            decoder: bundle_dir.join("decoder.onnx"),
            text_conditioner: bundle_dir.join("text_conditioner.onnx"),
            vocab: Some(bundle_dir.join("vocab.json")),
            token_scores: Some(bundle_dir.join("token_scores.json")),
            tokenizer_json: None,
            bos: None,
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
    ///
    /// # Errors
    ///
    /// wav read failures.
    pub fn set_reference_wav(&self, wav: &Path) -> TtsResult<()> {
        let audio = read_reference(wav).map_err(TtsError)?;
        *self
            .reference_audio
            .lock()
            .map_err(|_| TtsError("poisoned".into()))? = audio;
        Ok(())
    }

    /// Generate with SSML semantics: split at breaks, insert real silence,
    /// fire marks at exact positions. Rate scales the per-step duration.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    #[allow(clippy::too_many_lines, clippy::items_after_statements)]
    fn synthesize(&self, text: &str, rate: f32) -> TtsResult<SynthesisResult> {
        let reference = self
            .reference_audio
            .lock()
            .map_err(|_| TtsError("poisoned".into()))?
            .clone();
        if reference.is_empty() {
            return Err(TtsError("no reference audio configured".into()));
        }

        // Raw phoneme input uses '|' as a word-group separator; the SSML
        // tokenizer would shred it into bare phones (losing grouping for
        // word timings). Convert pipe groups to <phoneme> elements first,
        // unwrapping/re-wrapping any <speak> shell so tags survive intact.
        let pipe_converted;
        let text: &str = if text.contains('|') {
            let inner = text
                .trim()
                .trim_start_matches("<speak>")
                .trim_end_matches("</speak>")
                .trim();
            let mut out = String::from("<speak>");
            for part in inner.split('|') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let safe = part.replace('"', "");
                out.push_str("<phoneme ph=\"");
                out.push_str(&safe);
                out.push_str("\"/> ");
            }
            out.push_str("</speak>");
            pipe_converted = out;
            &pipe_converted
        } else {
            text
        };

        // SpeechMarkdown -> W3C SSML; plain text wrapped for uniform parsing
        let (ssml, _) = crate::engine::preprocess_speech_markdown(text, "plain");
        let ssml = if ssml.trim_start().to_ascii_lowercase().starts_with("<speak") {
            ssml
        } else {
            format!("<speak>{ssml}</speak>")
        };
        let doc = floravox_ssml::parse(&ssml).map_err(|e| TtsError(format!("SSML: {}", e.0)))?;

        // plan: word segments separated by breaks; marks recorded between them
        struct Seg {
            words: Vec<String>,
        }
        impl Seg {
            fn text(&self) -> String {
                self.words.join(" ")
            }
        }
        let mut segs: Vec<Seg> = vec![Seg { words: Vec::new() }];
        let mut pause_after: Vec<u32> = Vec::new(); // per-segment pause in ms
        let mut pending_marks: Vec<String> = Vec::new(); // marks before next segment

        for seg in &doc.segments {
            match seg {
                floravox_ssml::Segment::Words { words } => {
                    for w in words {
                        // `<phoneme ph="...">`: speak the override symbols
                        // (each Vec element is one IPA symbol)
                        let spoken: String = match &w.phonemes {
                            Some(ph) if !ph.is_empty() => ph.join(" "),
                            _ if w.spoken.is_empty() => w.text.clone(),
                            _ => w.spoken.clone(),
                        };
                        // raw phoneme input may carry '|' word groups; the
                        // SSML tokenizer only splits whitespace, so pipes
                        // arrive glued to phones — re-split into groups
                        if spoken.contains('|') {
                            for part in spoken.split('|') {
                                let part = part.trim();
                                if !part.is_empty() {
                                    segs.last_mut()
                                        .expect("always one")
                                        .words
                                        .push(part.to_string());
                                }
                            }
                        } else {
                            segs.last_mut().expect("always one").words.push(spoken);
                        }
                    }
                }
                floravox_ssml::Segment::Break { ms, .. } => {
                    pause_after.push(*ms);
                    segs.push(Seg { words: Vec::new() });
                }
                floravox_ssml::Segment::Mark { name, .. } => {
                    pending_marks.push(name.clone());
                }
                _ => {}
            }
        }

        // Long inputs make the model ramble past the text (a 9-word fox
        // sentence rendered 20s): sub-segment so each generate() call stays
        // in the length range where EOS is reliable. Phoneme mode: ~15
        // tokens per chunk (a bathroom-length phrase); orthographic: ~8-12
        // words, preferring sentence-punctuation cuts.
        let phoneme_mode = self.with_model(|m| {
            Ok(matches!(
                m.tokenizer,
                crate::pocket::tokenizer::AnyTokenizer::WordLevel(_)
            ))
        })?;
        const PHONEME_TOKENS_PER_CHUNK: usize = 10;
        const TARGET_WORDS: usize = 8;
        const MAX_WORDS: usize = 12;
        {
            let words_tokens = |w: &str| w.split_whitespace().count().max(1);
            let mut chunked: Vec<Seg> = Vec::new();
            let mut pauses: Vec<u32> = Vec::new();
            for (si, seg) in segs.iter().enumerate() {
                let pause = pause_after.get(si).copied().unwrap_or(0);
                let (target, max): (usize, usize) = if phoneme_mode {
                    (PHONEME_TOKENS_PER_CHUNK, PHONEME_TOKENS_PER_CHUNK + 1)
                } else {
                    (TARGET_WORDS, MAX_WORDS)
                };
                let seg_units: usize = if phoneme_mode {
                    seg.words.iter().map(|w| words_tokens(w)).sum()
                } else {
                    seg.words.len()
                };
                if seg_units <= max {
                    chunked.push(Seg {
                        words: seg.words.clone(),
                    });
                    pauses.push(pause);
                    continue;
                }
                let mut cur: Vec<String> = Vec::new();
                let mut cur_units = 0usize;
                for w in &seg.words {
                    cur.push(w.clone());
                    cur_units += if phoneme_mode { words_tokens(w) } else { 1 };
                    let sentence_end =
                        !phoneme_mode && w.chars().last().is_some_and(|c| ".!?;:,".contains(c));
                    if (cur_units >= target && sentence_end) || cur_units >= max {
                        chunked.push(Seg {
                            words: std::mem::take(&mut cur),
                        });
                        pauses.push(0); // flow-continuous within one sentence
                        cur_units = 0;
                    }
                }
                if !cur.is_empty() {
                    chunked.push(Seg { words: cur });
                    pauses.push(pause); // the explicit break belongs to the last chunk
                }
            }
            segs = chunked;
            pause_after = pauses;
        }

        // synthesize each non-empty segment; concatenate with silence
        let sr = crate::pocket::model::SAMPLE_RATE;
        let mut samples: Vec<f32> = Vec::new();
        let mut all_words: Vec<WordTimingOwned> = Vec::new();
        let mut marks: Vec<MarkTiming> = Vec::new();
        let mut spoken_text = String::new();
        let step_dur = crate::pocket::model::STEP_SECONDS / rate.max(0.1);

        for (si, seg) in segs.iter().enumerate() {
            let seg_text = seg.text();
            // fire any marks queued before this segment (or at the end)
            for name in &pending_marks {
                marks.push(MarkTiming {
                    name: name.clone(),
                    t_s: f64::from(samples.len() as u32) as f32 / f64::from(sr) as f32,
                });
            }
            pending_marks.clear();
            if seg_text.is_empty() {
                continue;
            }
            let (speech, seg_words) = self.with_model(|m| {
                // phoneme bundles: words carry `|` separators so the
                // tokenizer keeps word grouping for attention timings
                let synth_text = match &m.tokenizer {
                    crate::pocket::tokenizer::AnyTokenizer::WordLevel(_) => seg.words.join("|"),
                    crate::pocket::tokenizer::AnyTokenizer::Viterbi(_) => seg_text.clone(),
                };
                let s = m
                    .generate(
                        &synth_text,
                        &reference,
                        self.temperature,
                        self.num_steps,
                        self.max_frames,
                    )
                    .map_err(TtsError)?;
                let t = match &s.grouped {
                    Some((tok2word, words)) => {
                        word_boundaries_grouped(words, tok2word, &s.text_attention, step_dur)
                    }
                    None => match &m.tokenizer {
                        crate::pocket::tokenizer::AnyTokenizer::Viterbi(vt) => {
                            word_boundaries(vt, &s.target_token_ids, &s.text_attention, step_dur)
                        }
                        crate::pocket::tokenizer::AnyTokenizer::WordLevel(wt) => {
                            // plain (ungrouped) phoneme input: one "word" per token
                            let words: Vec<String> = s
                                .target_token_ids
                                .iter()
                                .map(|id| wt.id_to_piece(*id))
                                .collect();
                            let tok2word: Vec<usize> = (0..words.len()).collect();
                            word_boundaries_grouped(&words, &tok2word, &s.text_attention, step_dur)
                        }
                    },
                };
                Ok((s, t))
            })?;
            let seg_start_s = f64::from(samples.len() as u32) as f32 / f64::from(sr) as f32;
            samples.extend_from_slice(&speech.samples);
            for w in seg_words {
                all_words.push(WordTimingOwned {
                    word: w.word,
                    start_s: seg_start_s + w.start_s,
                    end_s: seg_start_s + w.end_s,
                });
            }
            if !spoken_text.is_empty() {
                spoken_text.push(' ');
            }
            spoken_text.push_str(&seg_text);
            // silence after this segment (break), except after the last
            if si < segs.len() - 1 {
                let ms = pause_after.get(si).copied().unwrap_or(0);
                let silence = ((f64::from(ms) / 1000.0) * f64::from(sr)) as usize;
                samples.resize(samples.len() + silence, 0.0);
            }
        }
        // trailing marks (after the final segment)
        for name in &pending_marks {
            marks.push(MarkTiming {
                name: name.clone(),
                t_s: f64::from(samples.len() as u32) as f32 / f64::from(sr) as f32,
            });
        }

        Ok(SynthesisResult {
            speech: GeneratedSpeech {
                samples,
                sample_rate: sr,
                grouped: None,
                text_attention: Vec::new(),
                target_token_ids: Vec::new(),
                voice_len: 0,
            },
            words: all_words,
            marks,
            spoken_text,
        })
    }
}

#[allow(clippy::cast_precision_loss)]
fn read_reference(wav: &Path) -> Result<Vec<f32>, String> {
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

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
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

fn speech_to_pcm16(samples: &[f32], sample_rate: u32, volume: f32) -> Vec<u8> {
    let vol = volume.clamp(0.0, 4.0);
    let mut pcm = Vec::with_capacity(samples.len() * 2 + 44);
    write_wav_header(&mut pcm, samples.len(), sample_rate);
    for s in samples {
        let v = ((*s) * vol).clamp(-1.0, 1.0);
        let p = (v * 32767.0).round() as i16;
        pcm.extend_from_slice(&p.to_le_bytes());
    }
    pcm
}

fn write_wav_header(out: &mut Vec<u8>, n_samples: usize, sample_rate: u32) {
    let data_len = (n_samples * 2) as u32;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
}

/// Fire boundary + mark callbacks and produce typed boundaries.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn fire_callbacks(
    text: &str,
    result: &SynthesisResult,
    mut on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
    mut on_mark: Option<crate::engine::OnMarkCallback<'_>>,
) -> Vec<WordBoundary> {
    let mut search = WordSearch::new(text);
    let mut out = Vec::with_capacity(result.words.len());
    for t in &result.words {
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
    for m in &result.marks {
        if let Some(cb) = on_mark.as_mut() {
            let (char_offset, _len) = search.find_next("");
            cb(&m.name, m.t_s, m.t_s, char_offset.max(0));
        }
    }
    out
}

impl TtsEngine for PocketTtsEngine {
    #[allow(clippy::too_many_arguments, clippy::used_underscore_binding)]
    fn speak(
        &self,
        text: &str,
        _voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        volume: f32,
        on_audio: Option<crate::engine::OnAudioCallback<'_>>,
        on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
        on_mark: Option<crate::engine::OnMarkCallback<'_>>,
    ) -> TtsResult<()> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst);
        self.stop_flag.store(false, Ordering::SeqCst);
        let result = self.synthesize(text, rate)?;
        if self.stop_flag.load(Ordering::SeqCst)
            || self.generation.load(Ordering::SeqCst) != generation + 1
        {
            return Ok(());
        }
        let pcm = speech_to_pcm16(&result.speech.samples, result.speech.sample_rate, volume);
        if let Some(cb) = on_audio {
            cb(&pcm);
        }
        fire_callbacks(&result.spoken_text, &result, on_boundary, on_mark);
        Ok(())
    }

    #[allow(clippy::too_many_arguments, clippy::used_underscore_binding)]
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
        let result = self.synthesize(text, rate)?;
        Ok(speech_to_pcm16(
            &result.speech.samples,
            result.speech.sample_rate,
            volume,
        ))
    }

    fn synth_with_boundaries(
        &self,
        text: &str,
        _voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        volume: f32,
    ) -> TtsResult<(Vec<u8>, Vec<WordBoundary>)> {
        let result = self.synthesize(text, rate)?;
        let pcm = speech_to_pcm16(&result.speech.samples, result.speech.sample_rate, volume);
        let boundaries = fire_callbacks(&result.spoken_text, &result, None, None);
        Ok((pcm, boundaries))
    }
}
