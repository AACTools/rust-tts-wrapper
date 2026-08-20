//! Sherpa-ONNX offline TTS engine with model registry.

use crate::boundaries::{EstimateFirer, EstimatePlan};
use crate::engine::TtsEngine;
use crate::types::{
    Gender, LanguageCode, SherpaLanguage, SherpaModelInfo, TtsError, TtsResult, Voice,
};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Shared cancellation flag — set by `stop()`, read by the progress callback.
static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);

// The sherpa-onnx generate callback must be 'static, but speak() holds the
// caller's on_audio/on_boundary borrows only for the method body. The C++
// runtime invokes the callback synchronously on the same thread inside the
// generate call, so we stash the callback pointers here for the duration
// (same technique as VISEME_CB in the cloud engine) and clear them after.
// Synthesis per engine instance is serialised by the tts_instance mutex.
type AudioCbPtr = *mut dyn FnMut(&[u8]);
type BoundaryCbPtr = *mut dyn FnMut(&str, f32, f32, i32, i32);

thread_local! {
    static STREAM_AUDIO_CB: std::cell::RefCell<Option<AudioCbPtr>> =
        const { std::cell::RefCell::new(None) };
    static STREAM_BOUNDARY_CB: std::cell::RefCell<Option<BoundaryCbPtr>> =
        const { std::cell::RefCell::new(None) };
}

/// PCM delivery chunk size. Sentence batches arrive whole from the
/// generate progress callback and larger ones are sliced into 8 KB chunks
/// before pushing them through `on_audio` — matching the cloud engines'
/// streamed-chunk shape so callers see the same multi-callback delivery
/// instead of one giant buffer.
const STREAMING_CHUNK_SIZE: usize = 8 * 1024;

/// Maps a 2-letter ISO 639-1 code to its 3-letter ISO 639-3 equivalent for the
/// languages covered by the Sherpa-ONNX model registry. Falls back to the
/// input when no mapping is known.
fn iso639_3(lang_code: &str) -> String {
    let lower = lang_code.to_ascii_lowercase();
    let two = lower.split(['-', '_']).next().unwrap_or(&lower);
    let three = match two {
        "en" => "eng",
        "zh" => "zho",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "ru" => "rus",
        "ar" => "ara",
        "ko" => "kor",
        "ja" => "jpn",
        "it" => "ita",
        "pt" => "por",
        "pl" => "pol",
        "nl" => "nld",
        "tr" => "tur",
        "cs" => "ces",
        "uk" => "ukr",
        "vi" => "vie",
        "th" => "tha",
        "hi" => "hin",
        "bn" => "ben",
        "fa" => "fas",
        "hu" => "hun",
        "el" => "ell",
        "fi" => "fin",
        "sv" => "swe",
        "da" => "dan",
        "no" => "nor",
        "he" => "heb",
        "ms" => "msa",
        "id" => "ind",
        "ro" => "ron",
        "sk" => "slk",
        "bg" => "bul",
        "ca" => "cat",
        "hr" => "hrv",
        "lt" => "lit",
        "lv" => "lav",
        "sr" => "srp",
        "sl" => "slv",
        "et" => "est",
        "tl" => "tgl",
        _ => return lower,
    };
    three.to_string()
}

/// Offline TTS engine using [Sherpa-ONNX](https://github.com/k2-fsa/sherpa-onnx).
pub struct SherpaOnnxEngine {
    models: HashMap<String, SherpaModelInfo>,
    model_dir: PathBuf,
    loaded_model_id: String,
    num_threads: i32,
    /// Supertonic denoising step count (quality knob: 5 low → 12 high,
    /// default 8). Ignored by other model types.
    num_steps: i32,
    /// Optional user-supplied reference clip (path to a 16-bit PCM wav) for
    /// zero-shot voice-cloning models (zipvoice, pocket). When absent the
    /// first bundled `test_wavs/*.wav` is used.
    reference_audio: Option<PathBuf>,
    /// Transcript matching [`Self::reference_audio`] — required by zipvoice
    /// (its bundled test wavs have known transcripts). Ignored by pocket,
    /// which clones from audio alone.
    reference_text: Option<String>,
    provider: Option<String>,
    // Cached ONNX runtime instance. Recreating OfflineTts per speak() is
    // expensive (model loading + ONNX init). Cache it so the first speak()
    // pays the cost and subsequent calls reuse it.
    tts_instance: Mutex<Option<sherpa_onnx::OfflineTts>>,
}

impl fmt::Debug for SherpaOnnxEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SherpaOnnxEngine")
            .field("loaded_model_id", &self.loaded_model_id)
            .field("num_threads", &self.num_threads)
            .field("num_steps", &self.num_steps)
            .field("provider", &self.provider)
            .field(
                "tts_cached",
                &self.tts_instance.lock().is_ok_and(|g| g.is_some()),
            )
            .finish_non_exhaustive()
    }
}

impl SherpaOnnxEngine {
    /// Create a new Sherpa-ONNX engine.
    ///
    /// Credentials JSON keys:
    /// - `modelPath`: directory containing downloaded models (defaults to
    ///   `~/.rust-tts-wrapper/sherpaonnx`)
    /// - `modelId`: id from the registry (e.g. `kokoro-en-v0_19`). Required —
    ///   if absent, no model is loaded and `speak` will return an error rather
    ///   than silently forcing a 305 MB download.
    /// - `numThreads`: ONNX runtime intra-op thread count (default 2).
    /// - `numSteps`: Supertonic denoising steps, 5–12 (default 8). Out-of-range
    ///   values fall back to 8. Ignored by non-Supertonic models.
    /// - `referenceAudio`: path to a 16-bit PCM wav used as the cloning
    ///   reference for zero-shot models (zipvoice, pocket). Defaults to the
    ///   model's bundled `test_wavs/` clip.
    /// - `referenceText`: transcript of `referenceAudio` (zipvoice requires
    ///   it to match the audio exactly; pocket ignores it).
    /// - `provider`: `cpu` (default), `coreml`, `cuda`, `directml`, etc.
    #[must_use]
    pub fn new(credentials_json: &str) -> Self {
        let mut model_dir = default_model_dir();
        let mut model_id = String::new();
        let mut num_threads = 2;
        let mut num_steps = 8;
        let mut provider: Option<String> = None;
        let mut reference_audio: Option<PathBuf> = None;
        let mut reference_text: Option<String> = None;

        if !credentials_json.is_empty() {
            // Numeric values are coerced to strings so callers can write
            // `{"numThreads": 2}` (JSON number) or `"2"` interchangeably —
            // a strict HashMap<String, String> parse would silently drop
            // the whole object on the first number and leave every option
            // at its default with no error.
            if let Ok(creds) =
                serde_json::from_str::<HashMap<String, serde_json::Value>>(credentials_json)
            {
                let creds: HashMap<String, String> = creds
                    .into_iter()
                    .map(|(k, v)| match v {
                        serde_json::Value::String(s) => (k, s),
                        other => (k, other.to_string()),
                    })
                    .collect();
                if let Some(dir) = creds.get("modelPath") {
                    model_dir = PathBuf::from(dir);
                }
                if let Some(id) = creds.get("modelId") {
                    model_id.clone_from(id);
                }
                if let Some(t) = creds.get("numThreads").and_then(|s| s.parse::<i32>().ok()) {
                    if t > 0 {
                        num_threads = t;
                    }
                }
                if let Some(s) = creds.get("numSteps").and_then(|s| s.parse::<i32>().ok()) {
                    // Clamp to Supertonic's supported quality range. Values
                    // outside 5–12 degrade output or waste compute, so fall
                    // back to the default rather than passing them through.
                    if (5..=12).contains(&s) {
                        num_steps = s;
                    }
                }
                if let Some(p) = creds.get("provider") {
                    if !p.is_empty() {
                        provider = Some(p.clone());
                    }
                }
                if let Some(r) = creds.get("referenceAudio") {
                    if !r.is_empty() {
                        reference_audio = Some(PathBuf::from(r));
                    }
                }
                if let Some(t) = creds.get("referenceText") {
                    if !t.is_empty() {
                        reference_text = Some(t.clone());
                    }
                }
            }
        }

        let models = load_models();

        SherpaOnnxEngine {
            models,
            model_dir,
            loaded_model_id: model_id,
            num_threads,
            num_steps,
            provider,
            reference_audio,
            reference_text,
            tts_instance: Mutex::new(None),
        }
    }

    /// Return the map of available models from the registry.
    pub fn available_models(&self) -> &HashMap<String, SherpaModelInfo> {
        &self.models
    }

    /// Resolve the reference clip for zero-shot cloning models: the
    /// user-supplied `referenceAudio` credentials path wins, else the first
    /// wav bundled under the model's `test_wavs/`.
    fn resolve_reference_audio(&self, model_dir: &std::path::Path) -> TtsResult<(Vec<f32>, i32)> {
        let wav = match &self.reference_audio {
            Some(p) => p.clone(),
            None => bundled_reference_wav(model_dir).ok_or_else(|| {
                TtsError(format!(
                    "no reference audio for zero-shot cloning: pass a 'referenceAudio' \
                     path in credentials, or bundle a wav under {}/test_wavs/",
                    model_dir.display()
                ))
            })?,
        };
        read_wav_mono_16bit(&wav)
    }

    /// Resolve the reference transcript (zipvoice only): the
    /// user-supplied `referenceText` credential wins, else the known
    /// transcript for a bundled sherpa-onnx test wav.
    fn resolve_reference_text(&self, model_dir: &std::path::Path) -> TtsResult<String> {
        if let Some(t) = &self.reference_text {
            return Ok(t.clone());
        }
        let wav = self
            .reference_audio
            .clone()
            .or_else(|| bundled_reference_wav(model_dir));
        if let Some(name) = wav
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            if let Some((_, text)) = ZIPVOICE_TEST_WAV_TRANSCRIPTS
                .iter()
                .find(|(known, _)| *known == name)
            {
                return Ok(text.to_string());
            }
        }
        Err(TtsError(
            "zipvoice requires the reference clip's exact transcript: pass \
             'referenceText' in credentials (it must match 'referenceAudio' \
             verbatim — a mismatch audibly degrades the clone)"
                .into(),
        ))
    }
}

/// Default directory for downloaded Sherpa-ONNX models.
fn default_model_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    let mut dir = PathBuf::from(home);
    dir.push(".rust-tts-wrapper");
    dir.push("sherpaonnx");
    dir
}

/// Parse the embedded `models.json` into a hashmap.
fn load_models() -> HashMap<String, SherpaModelInfo> {
    // Convert the typed registry crate into the wrapper's public
    // SherpaModelInfo shape (API-compatible with the old embedded copy).
    sherpa_onnx_models::models()
        .iter()
        .map(|(id, m)| {
            (
                id.clone(),
                SherpaModelInfo {
                    id: m.id.clone(),
                    model_type: m.model_type.clone(),
                    engines: m.engines.clone(),
                    name: m.name.clone(),
                    language: m
                        .language
                        .iter()
                        .map(|l| SherpaLanguage {
                            lang_code: l.lang_code.clone(),
                            language_name: l.language_name.clone(),
                            country: l.country.clone(),
                        })
                        .collect(),
                    sample_rate: m.sample_rate,
                    num_speakers: m.num_speakers,
                    quality: m.quality.clone(),
                    url: m.url.clone(),
                    compression: m.compression,
                    filesize_mb: m.filesize_mb,
                    license: m.license.clone(),
                    license_url: m.license_url.clone(),
                },
            )
        })
        .collect()
}

impl TtsEngine for SherpaOnnxEngine {
    #[allow(clippy::too_many_lines)]
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        mut on_audio: Option<crate::engine::OnAudioCallback>,
        mut on_boundary: Option<crate::engine::OnBoundaryCallback>,
        _on_mark: Option<crate::engine::OnMarkCallback>,
    ) -> TtsResult<()> {
        if self.loaded_model_id.is_empty() {
            return Err(TtsError(
                "No SherpaOnnx modelId configured. Pass modelId in credentials JSON. \
                 See available_models() for the registry."
                    .into(),
            ));
        }

        let model_info = self.models.get(&self.loaded_model_id).ok_or_else(|| {
            TtsError(format!(
                "Model '{}' not found in registry ({} models available)",
                self.loaded_model_id,
                self.models.len()
            ))
        })?;

        let model_dir = self.model_dir.join(&self.loaded_model_id);
        if !model_dir.exists() {
            return Err(TtsError(format!(
                "Model directory not found: {}. Download from: {}",
                model_dir.display(),
                model_info.url
            )));
        }

        // Dispatch model config by model_type. The branches below mirror the
        // file-layout conventions used by js-tts-wrapper and dotnet-tts-wrapper:
        //
        //   kokoro  → model.onnx + voices.bin + tokens.txt + espeak-ng-data/
        //   matcha  → acoustic-model.onnx + vocoder.onnx + tokens.txt
        //             (vocoder may be hifigan_v2.onnx, vocos-22khz-univ.onnx,
        //              or live in a shared base dir)
        //   vits    → model.onnx + tokens.txt + (lexicon.txt | espeak-ng-data/)
        //             Piper / GitHub models prefer espeak-ng-data and ignore
        //             dict_dir; Chinese models want a dict/ directory for
        //             jieba segmentation.
        //   zipvoice→ encoder/decoder .onnx (int8 variants preferred) +
        //             tokens.txt + lexicon.txt + espeak-ng-data/ + a shared
        //             vocos_24khz.onnx vocoder (base dir). Zero-shot cloning:
        //             speak() needs a reference clip + its transcript.
        //   pocket  → lm_flow/lm_main/encoder/decoder/text_conditioner .onnx
        //             (int8 where shipped) + vocab.json + token_scores.json.
        //             Zero-shot cloning from a reference clip alone.
        //   mms /   → MMS models use the VITS config but typically have no
        //   unknown   espeak-ng-data; they ship just model.onnx + tokens.txt
        //             + lexicon.txt.
        //
        // The registry has ~1143 MMS entries that omit
        // `model_type`, so empty/unknown falls through to VITS handling.
        let id_lower = self.loaded_model_id.to_ascii_lowercase();
        let is_piper_or_github = is_piper_or_github_model(&id_lower);
        let is_chinese = id_lower.starts_with("vits-icefall-zh")
            || id_lower.contains("cantonese")
            || id_lower.starts_with("mms_zho")
            || id_lower.starts_with("mms_cmn");

        // Piper and GitHub archives often extract to a nested subdirectory
        // (e.g. vits-piper-en_US-amy-low/en_US-amy-low.onnx). If the model
        // dir has no top-level model files, descend into the single child
        // directory (mirrors VoiceGarden's ResolveModelScanDir).
        let model_dir = resolve_model_scan_dir(&model_dir);

        let model_config = match model_info.model_type.as_str() {
            "kokoro" => sherpa_onnx::OfflineTtsModelConfig {
                kokoro: build_kokoro_config(&model_dir),
                num_threads: self.num_threads,
                debug: false,
                provider: self.provider.clone(),
                ..Default::default()
            },
            "matcha" => sherpa_onnx::OfflineTtsModelConfig {
                matcha: build_matcha_config(&model_dir, &self.model_dir)?,
                num_threads: self.num_threads,
                debug: false,
                provider: self.provider.clone(),
                ..Default::default()
            },
            "kitten" => sherpa_onnx::OfflineTtsModelConfig {
                kitten: sherpa_onnx::OfflineTtsKittenModelConfig {
                    model: Some(model_dir.join("model.onnx").to_string_lossy().to_string()),
                    voices: Some(model_dir.join("voices.bin").to_string_lossy().to_string()),
                    tokens: Some(model_dir.join("tokens.txt").to_string_lossy().to_string()),
                    data_dir: existing_path(&model_dir, "espeak-ng-data"),
                    ..Default::default()
                },
                num_threads: self.num_threads,
                debug: false,
                provider: self.provider.clone(),
                ..Default::default()
            },
            "supertonic" => sherpa_onnx::OfflineTtsModelConfig {
                supertonic: build_supertonic_config(&model_dir),
                num_threads: self.num_threads,
                debug: false,
                provider: self.provider.clone(),
                ..Default::default()
            },
            "zipvoice" => sherpa_onnx::OfflineTtsModelConfig {
                zipvoice: build_zipvoice_config(&model_dir, &self.model_dir)?,
                num_threads: self.num_threads,
                debug: false,
                provider: self.provider.clone(),
                ..Default::default()
            },
            "pocket" => sherpa_onnx::OfflineTtsModelConfig {
                pocket: build_pocket_config(&model_dir),
                num_threads: self.num_threads,
                debug: false,
                provider: self.provider.clone(),
                ..Default::default()
            },
            // VITS, MMS (Facebook Massively Multilingual Speech), and unknown
            // model types all use the VITS config family.
            "vits" | "mms" | "unknown" | "" => sherpa_onnx::OfflineTtsModelConfig {
                vits: build_vits_config(&model_dir, is_piper_or_github, is_chinese),
                num_threads: self.num_threads,
                debug: false,
                provider: self.provider.clone(),
                ..Default::default()
            },
            other => {
                return Err(TtsError(format!(
                    "Unsupported SherpaOnnx model_type '{other}' for model '{}'",
                    self.loaded_model_id
                )));
            }
        };

        let config = sherpa_onnx::OfflineTtsConfig {
            model: model_config,
            // Single-sentence mode matches the reference implementations and
            // avoids extra allocations when the input is short.
            max_num_sentences: 1,
            ..Default::default()
        };

        // Use cached OfflineTts instance if available; create on first call.
        // The Mutex guards the Option; we hold it for the entire synthesis
        // since OfflineTts::generate_with_config needs &self.
        let mut tts_guard = self.tts_instance.lock().unwrap();
        if tts_guard.is_none() {
            let tts = sherpa_onnx::OfflineTts::create(&config)
                .ok_or_else(|| TtsError("Failed to create SherpaOnnx TTS engine".into()))?;
            *tts_guard = Some(tts);
        }
        let tts = tts_guard.as_ref().expect("tts was just initialised");

        let sid = voice.and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
        // Supertonic needs both a speaker id and a language; the language is
        // delivered through GenerationConfig.extra. We encode both in the
        // voice id as "sid:lang" (produced by get_voices), and fall back to a
        // bare integer + default "en" for backwards compatibility.
        //
        // Zipvoice / pocket are zero-shot cloning models: instead of a preset
        // speaker they need a reference clip (and, for zipvoice, its exact
        // transcript) delivered through GenerationConfig.
        let gen_config = if model_info.model_type == "supertonic" {
            let (sid, lang) = parse_supertonic_voice(voice);
            let mut extra = HashMap::new();
            extra.insert("lang".to_string(), serde_json::Value::String(lang));
            sherpa_onnx::GenerationConfig {
                sid,
                speed: rate.max(0.1),
                num_steps: self.num_steps,
                extra: Some(extra),
                ..Default::default()
            }
        } else if model_info.model_type == "zipvoice" || model_info.model_type == "pocket" {
            let (ref_samples, ref_rate) = self.resolve_reference_audio(&model_dir)?;
            let reference_text = if model_info.model_type == "zipvoice" {
                Some(self.resolve_reference_text(&model_dir)?)
            } else {
                None
            };
            // Flow-matching step counts from the sherpa-onnx docs: zipvoice
            // defaults to 4, pocket to 2 (higher = better quality, slower).
            let num_steps = if model_info.model_type == "zipvoice" {
                4
            } else {
                2
            };
            sherpa_onnx::GenerationConfig {
                speed: rate.max(0.1),
                num_steps,
                reference_audio: Some(ref_samples),
                reference_sample_rate: ref_rate,
                reference_text,
                ..Default::default()
            }
        } else {
            sherpa_onnx::GenerationConfig {
                sid,
                speed: rate.max(0.1),
                ..Default::default()
            }
        };

        let volume_factor = volume.clamp(0.0, 4.0);
        let pitch_factor = pitch.clamp(0.25, 4.0);
        let wants_callback = on_audio.is_some();

        // Reset cancellation flag before synthesis.
        CANCEL_REQUESTED.store(false, Ordering::SeqCst);

        // Streaming delivery: sherpa-onnx's generate callback fires after
        // each sentence batch (max_num_sentences=1 above) with the batch's
        // NEWLY generated samples — not cumulative — so audio is delivered
        // through `on_audio` while later sentences are still synthesising.
        // Volume/pitch are applied per batch (batches are sentence-aligned,
        // so per-batch resampling for pitch doesn't seam mid-speech).
        //
        // Word-boundary estimates (150-wpm baseline) fire progressively,
        // anchored to delivered samples and scaled by 1/speed so they track
        // the audio actually emitted; the reported times stay on the
        // rate-1.0 baseline (existing callers, e.g. VoiceGarden-SPD and the
        // SAPI adapter, compensate for rate themselves).
        //
        // The generate callback must be `'static`, so it cannot capture the
        // method-lifetime `on_audio`/`on_boundary` borrows directly. The C++
        // runtime invokes the callback synchronously on this same thread
        // while the borrows are live, so the pointers are stashed in
        // thread-locals for the duration of the call (the same technique as
        // VISEME_CB above) and cleared afterwards.
        #[allow(clippy::cast_precision_loss)]
        let time_scale = 1.0 / rate.max(0.1);
        let sample_rate_out = model_info.sample_rate.max(1);

        if wants_callback {
            let plan = on_boundary.is_some().then(|| EstimatePlan::build(text));

            // SAFETY (stash): the raw pointers are only dereferenced inside
            // the generate callback, which sherpa-onnx calls synchronously
            // on this thread between the stash and the clear below, while
            // `on_audio`/`on_boundary` are still borrowed. Synthesis on a
            // given engine instance is serialised by the `tts_instance`
            // mutex held for the whole call.
            // ptr→ptr transmute + explicit borrow-to-pointer are the
            // standard lifetime-erasure idioms for synchronous callback
            // stashing; the safety argument is documented above.
            #[allow(clippy::transmute_ptr_to_ptr)]
            let audio_ptr: Option<AudioCbPtr> = on_audio.as_mut().map(|cb| {
                // SAFETY: the fat pointer's layout is identical; only the
                // lifetime is erased. See the thread-local docs for why
                // use is confined to this call.
                unsafe {
                    std::mem::transmute::<*mut (dyn FnMut(&[u8]) + '_), AudioCbPtr>(
                        std::ptr::from_mut(&mut **cb),
                    )
                }
            });
            #[allow(clippy::transmute_ptr_to_ptr)]
            let boundary_ptr: Option<BoundaryCbPtr> = on_boundary.as_mut().map(|cb| {
                // SAFETY: as above.
                unsafe {
                    std::mem::transmute::<
                        *mut (dyn FnMut(&str, f32, f32, i32, i32) + '_),
                        BoundaryCbPtr,
                    >(std::ptr::from_mut(&mut **cb))
                }
            });
            STREAM_AUDIO_CB.with(|c| *c.borrow_mut() = audio_ptr);
            STREAM_BOUNDARY_CB.with(|c| *c.borrow_mut() = boundary_ptr);

            // The firer owns the plan and is shared with the 'static
            // callback via Arc<Mutex<..>>, so the outer scope can flush
            // the remainder after generation ends.
            let firer = plan.map(|p| Arc::new(Mutex::new(EstimateFirer::new(p, time_scale))));
            let firer_for_cb = firer.clone();

            // A `None` result means the engine rejected the config outright;
            // cancellation mid-stream still returns Some(audio-so-far),
            // which we ignore — every batch was already streamed.
            let result = tts.generate_with_config(
                text,
                &gen_config,
                Some(move |batch: &[f32], _progress: f32| -> bool {
                    if CANCEL_REQUESTED.load(Ordering::SeqCst) {
                        return false;
                    }
                    let processed = apply_volume_and_pitch(batch, volume_factor, pitch_factor);
                    let pcm = samples_to_le_bytes(&processed);
                    STREAM_AUDIO_CB.with(|c| {
                        if let Some(ptr) = *c.borrow() {
                            // SAFETY (stash): see the thread-local docs.
                            // Chunk to keep the documented 8 KB multi-
                            // callback delivery shape.
                            for chunk in pcm.chunks(STREAMING_CHUNK_SIZE) {
                                unsafe { (*ptr)(chunk) };
                            }
                        }
                    });
                    if let Some(f) = firer_for_cb.as_ref() {
                        if let Ok(mut guard) = f.lock() {
                            guard.on_samples(
                                batch.len() as u64,
                                Some(sample_rate_out),
                                &mut |ev| {
                                    STREAM_BOUNDARY_CB.with(|c| {
                                        if let Some(ptr) = *c.borrow() {
                                            // SAFETY (stash): see above.
                                            unsafe {
                                                (*ptr)(
                                                    &ev.word,
                                                    ev.start_s,
                                                    ev.end_s,
                                                    ev.char_offset,
                                                    ev.char_len,
                                                );
                                            }
                                        }
                                    });
                                },
                            );
                        }
                    }
                    !CANCEL_REQUESTED.load(Ordering::SeqCst)
                }),
            );

            STREAM_AUDIO_CB.with(|c| *c.borrow_mut() = None);
            STREAM_BOUNDARY_CB.with(|c| *c.borrow_mut() = None);

            if result.is_none() {
                return Err(TtsError("SherpaOnnx synthesis returned no audio".into()));
            }

            // Fire any estimates whose audio never arrived (short audio,
            // cancellation): the boundary set is now closed.
            if let (Some(f), Some(cb)) = (firer.as_ref(), on_boundary.as_mut()) {
                if let Ok(mut f) = f.lock() {
                    f.flush(&mut |ev| {
                        cb(&ev.word, ev.start_s, ev.end_s, ev.char_offset, ev.char_len);
                    });
                }
            }
        } else {
            let audio = tts
                .generate_with_config(
                    text,
                    &gen_config,
                    Some(|_s: &[f32], _p: f32| -> bool {
                        !CANCEL_REQUESTED.load(Ordering::SeqCst)
                    }),
                )
                .ok_or_else(|| TtsError("SherpaOnnx synthesis returned no audio".into()))?;
            let processed = apply_volume_and_pitch(audio.samples(), volume_factor, pitch_factor);
            let filename = std::env::temp_dir().join("rust-tts-wrapper-sherpa.wav");
            if write_wav(&filename, &processed, audio.sample_rate()) {
                play_wav_file(&filename);
            }

            // No streaming took place; estimates fire as before.
            if let Some(cb) = on_boundary.as_mut() {
                let plan = EstimatePlan::build(text);
                for i in 0..plan.len() {
                    let ev = plan.event(i).expect("in range");
                    cb(&ev.word, ev.start_s, ev.end_s, ev.char_offset, ev.char_len);
                }
            }
        }

        Ok(())
    }

    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<crate::engine::OnAudioCallback>,
        on_boundary: Option<crate::engine::OnBoundaryCallback>,
        on_mark: Option<crate::engine::OnMarkCallback>,
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
        // The progress callback reads this flag on every chunk and aborts
        // synthesis when set.
        CANCEL_REQUESTED.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        let model_info = self.models.get(&self.loaded_model_id);

        // Supertonic is addressed by (speaker, language) rather than a bare
        // speaker id. Emit one voice per speaker × language pair, encoding
        // both in the id as "sid:lang" so speak() can route the language
        // through GenerationConfig.extra.
        if let Some(info) = model_info {
            if info.model_type == "supertonic" {
                return Ok(supertonic_voices(info));
            }
        }

        let num_speakers = model_info.map_or(1, |m| m.num_speakers);
        let lang = model_info
            .and_then(|m| m.language.first())
            .map(|l| l.language_name.clone())
            .unwrap_or_default();
        let lang_code = model_info
            .and_then(|m| m.language.first())
            .map(|l| l.lang_code.clone())
            .unwrap_or_default();
        let iso639 = iso639_3(&lang_code);
        let mut voices = Vec::new();
        for i in 0..num_speakers {
            voices.push(Voice {
                id: format!("{i}"),
                name: format!("Speaker {i}"),
                gender: Gender::Unknown,
                provider: "sherpaonnx".to_string(),
                language_codes: vec![LanguageCode {
                    bcp47: lang.clone(),
                    iso639_3: iso639.clone(),
                    display: crate::types::locale_display_name(&lang),
                }],
            });
        }
        Ok(voices)
    }

    fn engine_id(&self) -> &'static str {
        "sherpaonnx"
    }
}

/// Apply volume scaling and pitch shifting to a buffer of f32 samples.
///
/// Volume is a straightforward linear scale. Pitch shifting uses simple
/// linear-interpolation resampling — it does change duration slightly, but it
/// is the cheapest DSP approach that doesn't pull in an FFT dependency. The
/// shift is a no-op when both factors are 1.0.
fn apply_volume_and_pitch(samples: &[f32], volume: f32, pitch: f32) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    // First resample for pitch (changes length).
    let resampled: Vec<f32> = if (pitch - 1.0).abs() > f32::EPSILON {
        #[allow(clippy::cast_precision_loss)]
        let out_len = ((samples.len() as f32) / pitch).round().max(1.0) as usize;
        let mut out = Vec::with_capacity(out_len);
        #[allow(clippy::cast_precision_loss)]
        let step = (samples.len() as f32) / out_len as f32;
        let mut idx = 0.0f32;
        while (idx as usize) < samples.len() {
            let i = idx as usize;
            #[allow(clippy::cast_precision_loss)]
            let frac = idx - i as f32;
            let next = samples.get(i + 1).copied().unwrap_or(samples[i]);
            let v = samples[i] * (1.0 - frac) + next * frac;
            out.push(v);
            idx += step;
        }
        out
    } else {
        samples.to_vec()
    };
    // Then scale amplitude for volume.
    if (volume - 1.0).abs() > f32::EPSILON {
        resampled.iter().map(|&s| s * volume).collect()
    } else {
        resampled
    }
}

/// Convert f32 samples to little-endian PCM16 bytes (one allocation).
#[allow(clippy::cast_possible_truncation)]
fn samples_to_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let s16 = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        pcm.extend_from_slice(&s16.to_le_bytes());
    }
    pcm
}

/// Write a 16-bit PCM mono WAV file. Returns `false` on I/O error.
fn write_wav(path: &std::path::Path, samples: &[f32], sample_rate: i32) -> bool {
    use std::io::Write;
    let Ok(mut f) = std::fs::File::create(path) else {
        return false;
    };
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let s16 = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        pcm.extend_from_slice(&s16.to_ne_bytes());
    }
    let data_len = pcm.len() as u32;
    let sample_rate = sample_rate as u32;
    let byte_rate = sample_rate * 2; // 16-bit mono
                                     // WAV PCM header: block_align and bits_per_sample are u16, NOT u32 —
                                     // emitting them as 4 bytes shifts every subsequent field by 2 bytes
                                     // and produces a technically malformed header (most players tolerate
                                     // it but it's wrong per the RIFF spec).
    let block_align: u16 = 2;
    let bits_per_sample: u16 = 16;
    let riff_len = 36 + data_len;
    let header = [
        b"RIFF".as_slice(),
        &riff_len.to_le_bytes(),
        b"WAVE",
        b"fmt ",
        &16u32.to_le_bytes(), // PCM chunk size
        &1u16.to_le_bytes(),  // PCM format
        &1u16.to_le_bytes(),  // mono
        &sample_rate.to_le_bytes(),
        &byte_rate.to_le_bytes(),
        &block_align.to_le_bytes(),
        &bits_per_sample.to_le_bytes(),
        b"data",
        &data_len.to_le_bytes(),
    ]
    .concat();
    if f.write_all(&header).is_err() || f.write_all(&pcm).is_err() {
        return false;
    }
    true
}

/// Play a WAV file using a platform-appropriate command.
///
/// - Linux: `aplay`
/// - macOS: `afplay`
/// - Windows: PowerShell `(New-Object Media.SoundPlayer).PlaySync()`
///
/// Failures are swallowed because playback is best-effort (audio has already
/// been rendered to a file the caller can locate).
fn play_wav_file(path: &std::path::Path) {
    let result = if cfg!(target_os = "linux") {
        std::process::Command::new("aplay")
            .arg("-q")
            .arg(path)
            .spawn()
            .map(|mut c| c.wait())
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("afplay")
            .arg(path)
            .spawn()
            .map(|mut c| c.wait())
    } else if cfg!(target_os = "windows") {
        let p = path.to_string_lossy().replace('\'', "''");
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(New-Object Media.SoundPlayer '{p}').PlaySync()"),
            ])
            .spawn()
            .map(|mut c| c.wait())
    } else {
        return;
    };
    let _ = result;
}

// ===== Model-config builders =====
//
// These helpers encapsulate the file-layout differences between Kokoro,
// Matcha, and the various VITS flavours (Piper, MMS, Coqui, Chinese, ...).
// They mirror the per-model logic in js-tts-wrapper / dotnet-tts-wrapper.

/// If `dir` has no top-level model files but has exactly one subdirectory,
/// return that subdirectory. Mirrors VoiceGarden's `ResolveModelScanDir`.
fn resolve_model_scan_dir(dir: &std::path::Path) -> std::path::PathBuf {
    let has_top = dir.join("tokens.txt").exists()
        || dir.join("model.onnx").exists()
        || dir.join("voices.bin").exists()
        || dir.join("espeak-ng-data").exists()
        || std::fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .filter_map(Result::ok)
                .any(|e| e.path().extension().is_some_and(|ext| ext == "onnx"))
        });
    if has_top {
        return dir.to_path_buf();
    }
    // No top-level files — look for a single subdirectory.
    if let Ok(entries) = std::fs::read_dir(dir) {
        let subdirs: Vec<_> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .collect();
        if subdirs.len() == 1 {
            return subdirs[0].path();
        }
    }
    dir.to_path_buf()
}

/// Find the primary model .onnx in a directory. Prefers `model.onnx`,
/// then falls back to the first .onnx that isn't an acoustic model or
/// vocoder. Mirrors VoiceGarden's `FindPrimaryModelOnnx`.
fn find_primary_model_onnx(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let model_onnx = dir.join("model.onnx");
    if model_onnx.exists() {
        return Some(model_onnx);
    }
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"))
            {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_ascii_lowercase)
                    .unwrap_or_default();
                // Skip acoustic models and vocoders.
                if !name.starts_with("model-steps")
                    && !name.starts_with("vocos")
                    && !name.starts_with("vocoder")
                    && !name.starts_with("hifigan")
                {
                    return Some(path);
                }
            }
            None
        })
}

/// Return `Some(path)` only when `dir/name` exists on disk; otherwise `None`.
fn existing_path(dir: &std::path::Path, name: &str) -> Option<String> {
    let p = dir.join(name);
    if p.exists() {
        Some(p.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Walk `dir` and return the path of the first child matching `name`, if any.
fn find_file(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == name) {
                Some(path)
            } else {
                None
            }
        })
}

/// Return the first existing file under `dir` matching one of `names`.
fn first_existing(dir: &std::path::Path, names: &[&str]) -> Option<std::path::PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.exists())
}

/// Heuristic: is this a Piper voice or another "GitHub-style" archive model
/// (Coqui / icefall / mimic3 / melo / vctk / ljs / cantonese / zh / kokoro)?
/// These layouts ship `espeak-ng-data/` rather than a lexicon and shouldn't
/// be configured with `dict_dir` (jieba would otherwise warn on every call).
fn is_piper_or_github_model(model_id: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "piper-",
        "coqui-",
        "icefall-",
        "mimic3-",
        "melo-",
        "vctk-",
        "zh-",
        "ljs-",
        "cantonese-",
        "kokoro-",
    ];
    PREFIXES.iter().any(|p| model_id.starts_with(p))
}

/// Collect lexicon paths for a Kokoro model. Multilingual releases (e.g.
/// `kokoro-zh_en-int8`) ship several `lexicon-*.txt` files which
/// sherpa-onnx accepts as a comma-separated list; English-only Kokoro has
/// none and relies on `espeak-ng-data/` instead. Returns `None` when no
/// lexicon files are present so the field is left unset.
fn collect_lexicons(dir: &std::path::Path) -> Option<String> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n == "lexicon.txt" || n.starts_with("lexicon-"))
        })
        .collect();
    if paths.is_empty() {
        return None;
    }
    // Sort for deterministic ordering across platforms / readdir order.
    paths.sort();
    Some(
        paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Locate the dictionary directory for Chinese text normalisation. Kokoro zh
/// models keep `*.fst` files (date-zh.fst, number-zh.fst, phone-zh.fst) at the
/// model root; some VITS models nest a `dict/` folder. Returns `None` when no
/// FST/dict layout is present.
fn find_dict_dir(dir: &std::path::Path) -> Option<String> {
    let dict_sub = dir.join("dict");
    if dict_sub.is_dir() {
        return Some(dict_sub.to_string_lossy().to_string());
    }
    let has_fst = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .any(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("fst"))
        });
    if has_fst {
        Some(dir.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Kokoro config: model.onnx + voices.bin + tokens.txt + (espeak-ng-data/ |
/// lexicon-*.txt + dict_dir). Quantised releases ship `model.int8.onnx`;
/// `find_primary_model_onnx` resolves both layouts.
fn build_kokoro_config(model_dir: &std::path::Path) -> sherpa_onnx::OfflineTtsKokoroModelConfig {
    // Prefer the canonical model.onnx; fall back to a directory scan that
    // skips vocoders and matcha acoustic models (handles model.int8.onnx).
    let model = find_primary_model_onnx(model_dir).unwrap_or_else(|| model_dir.join("model.onnx"));
    sherpa_onnx::OfflineTtsKokoroModelConfig {
        model: Some(model.to_string_lossy().to_string()),
        voices: existing_path(model_dir, "voices.bin"),
        tokens: Some(model_dir.join("tokens.txt").to_string_lossy().to_string()),
        data_dir: existing_path(model_dir, "espeak-ng-data"),
        // Multilingual Kokoro (e.g. zh_en) ships lexicon-*.txt and Chinese FST
        // normalisation; English-only Kokoro uses espeak-ng-data instead, in
        // which case these resolve to None.
        lexicon: collect_lexicons(model_dir),
        dict_dir: find_dict_dir(model_dir),
        // length_scale left at default — rate is applied via GenerationConfig.speed.
        ..Default::default()
    }
}

/// Supertonic config: four ONNX sessions (`duration_predictor`,
/// `text_encoder`, `vector_estimator`, `vocoder`) plus `tts.json`,
/// `unicode_indexer.bin`, and `voice.bin`. The sherpa-onnx release ships
/// int8-quantised onnx files (`<name>.int8.onnx`); fall back to the
/// unquantised `<name>.onnx` if the int8 variant is absent. Missing files
/// surface as `None` and sherpa-onnx will fail at `OfflineTts::create` with a
/// clear message — matching how the Kokoro builder handles an absent
/// `voices.bin`.
fn build_supertonic_config(
    model_dir: &std::path::Path,
) -> sherpa_onnx::OfflineTtsSupertonicModelConfig {
    sherpa_onnx::OfflineTtsSupertonicModelConfig {
        duration_predictor: first_existing(
            model_dir,
            &["duration_predictor.int8.onnx", "duration_predictor.onnx"],
        )
        .map(|p| p.to_string_lossy().to_string()),
        text_encoder: first_existing(model_dir, &["text_encoder.int8.onnx", "text_encoder.onnx"])
            .map(|p| p.to_string_lossy().to_string()),
        vector_estimator: first_existing(
            model_dir,
            &["vector_estimator.int8.onnx", "vector_estimator.onnx"],
        )
        .map(|p| p.to_string_lossy().to_string()),
        vocoder: first_existing(model_dir, &["vocoder.int8.onnx", "vocoder.onnx"])
            .map(|p| p.to_string_lossy().to_string()),
        tts_json: existing_path(model_dir, "tts.json"),
        unicode_indexer: existing_path(model_dir, "unicode_indexer.bin"),
        voice_style: existing_path(model_dir, "voice.bin"),
    }
}

/// Zipvoice config: encoder + decoder (int8 variants preferred) + tokens +
/// lexicon + espeak-ng-data, plus the shared `vocos_24khz.onnx` vocoder.
///
/// The vocoder is *not* bundled in the zipvoice archive — it lives in the
/// separate `vocoder-models` release, so it's resolved from the model dir
/// first (custom layouts) then the user's base model dir (the same
/// shared-vocoder convention as Matcha). A missing vocoder is a hard error
/// with the download URL rather than a sherpa-onnx create() panic.
fn build_zipvoice_config(
    model_dir: &std::path::Path,
    base_dir: &std::path::Path,
) -> TtsResult<sherpa_onnx::OfflineTtsZipvoiceModelConfig> {
    let quant_onnx = |stem: &str| {
        first_existing(
            model_dir,
            &[&format!("{stem}.int8.onnx"), &format!("{stem}.onnx")],
        )
        .map(|p| p.to_string_lossy().to_string())
    };

    let vocoder = first_existing(model_dir, &["vocos_24khz.onnx", "vocoder.onnx"])
        .or_else(|| first_existing(base_dir, &["vocos_24khz.onnx", "vocoder.onnx"]));
    let vocoder = vocoder.ok_or_else(|| {
        TtsError(
            "Zipvoice requires the vocos_24khz.onnx vocoder, which is not bundled \
             with the model. Download it into {} from:\n  \
             https://github.com/k2-fsa/sherpa-onnx/releases/download/vocoder-models/vocos_24khz.onnx"
                .replace("{}", &base_dir.display().to_string()),
        )
    })?;

    Ok(sherpa_onnx::OfflineTtsZipvoiceModelConfig {
        tokens: existing_path(model_dir, "tokens.txt"),
        encoder: quant_onnx("encoder"),
        decoder: quant_onnx("decoder"),
        vocoder: Some(vocoder.to_string_lossy().to_string()),
        data_dir: existing_path(model_dir, "espeak-ng-data"),
        lexicon: existing_path(model_dir, "lexicon.txt"),
        // feat_scale / t_shift / target_rms / guidance_scale: 0.0 makes the
        // C API fall back to the model's trained defaults (same behaviour as
        // the sherpa-onnx CLI, which leaves them unset).
        ..Default::default()
    })
}

/// Pocket config: five ONNX components + two JSON sidecars. Only `decoder`,
/// `lm_flow`, and `lm_main` ship int8 variants in the quantised build —
/// `encoder` and `text_conditioner` are always plain `.onnx`.
fn build_pocket_config(model_dir: &std::path::Path) -> sherpa_onnx::OfflineTtsPocketModelConfig {
    let component = |stems: &[&str]| {
        stems
            .iter()
            .find_map(|s| first_existing(model_dir, &[s]))
            .map(|p| p.to_string_lossy().to_string())
    };
    sherpa_onnx::OfflineTtsPocketModelConfig {
        lm_flow: component(&["lm_flow.int8.onnx", "lm_flow.onnx"]),
        lm_main: component(&["lm_main.int8.onnx", "lm_main.onnx"]),
        encoder: component(&["encoder.int8.onnx", "encoder.onnx"]),
        decoder: component(&["decoder.int8.onnx", "decoder.onnx"]),
        text_conditioner: component(&["text_conditioner.int8.onnx", "text_conditioner.onnx"]),
        vocab_json: existing_path(model_dir, "vocab.json"),
        token_scores_json: existing_path(model_dir, "token_scores.json"),
        ..Default::default()
    }
}

/// Exact transcripts for the reference wavs bundled in the sherpa-onnx
/// zero-shot archives (from the sherpa-onnx docs — zipvoice requires the
/// transcript to match the audio, a mismatch audibly degrades the clone).
const ZIPVOICE_TEST_WAV_TRANSCRIPTS: &[(&str, &str)] = &[(
    "leijun-1.wav",
    "那还是三十六年前, 一九八七年. 我呢考上了武汉大学的计算机系.",
)];

fn bundled_reference_wav(model_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for sub in ["test_wavs", "."] {
        let dir = model_dir.join(sub);
        if dir.is_dir() {
            let mut wavs: Vec<_> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")))
                .collect();
            wavs.sort();
            if let Some(first) = wavs.first() {
                return Some(first.clone());
            }
        }
    }
    None
}

/// Minimal RIFF/PCM wav reader for reference clips: 16-bit PCM mono only
/// (the sherpa-onnx test wavs and typical cloning references are 16-bit
/// mono; anything else errors clearly rather than being silently mangled).
#[allow(clippy::cast_precision_loss)] // stereo downmix averages samples
fn read_wav_mono_16bit(path: &std::path::Path) -> TtsResult<(Vec<f32>, i32)> {
    let bytes = std::fs::read(path)
        .map_err(|e| TtsError(format!("cannot read reference wav {}: {e}", path.display())))?;
    let rd = |off: usize| -> Option<u32> {
        bytes
            .get(off..off + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(TtsError(format!(
            "{} is not a RIFF/WAVE file",
            path.display()
        )));
    }
    // Walk chunks for fmt (format + rate) and data (samples).
    let (mut format, mut channels, mut rate, mut bits) = (0u16, 0u16, 0i32, 0u16);
    let mut samples: Vec<f32> = Vec::new();
    let mut off = 12;
    while off + 8 <= bytes.len() {
        let id = &bytes[off..off + 4];
        let len = rd(off + 4).unwrap_or(0) as usize;
        let body = off + 8;
        match id {
            b"fmt " if len >= 16 => {
                format = u16::from_le_bytes(bytes[body..body + 2].try_into().unwrap());
                channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
                rate = rd(body + 4).unwrap_or(0) as i32;
                bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
            }
            b"data" => {
                let end = (body + len).min(bytes.len());
                samples = bytes[body..end]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| f32::from(i16::from_le_bytes(*c)) / 32768.0)
                    .collect();
            }
            _ => {}
        }
        off = body + len + (len & 1); // chunks are word-aligned
    }
    if format != 1 || bits != 16 {
        return Err(TtsError(format!(
            "{}: only 16-bit PCM reference wavs are supported (got format {format}, {bits}-bit). \
             Convert with e.g. `ffmpeg -i in.mp3 -ac 1 -ar 24000 -c:a pcm_s16le out.wav`.",
            path.display()
        )));
    }
    if channels > 1 {
        // Downmix to mono by averaging channels (cloning references are
        // effectively mono anyway; sherpa-onnx expects a single channel).
        let frame_len = channels as usize;
        samples = samples
            .chunks_exact(frame_len)
            .map(|fr| fr.iter().sum::<f32>() / frame_len as f32)
            .collect();
    }
    if samples.is_empty() || rate == 0 {
        return Err(TtsError(format!(
            "{}: wav has no usable samples (rate {rate}, {} samples)",
            path.display(),
            samples.len()
        )));
    }
    Ok((samples, rate))
}

/// Parse a Supertonic voice id of the form `"sid:lang"` (e.g. `"6:ja"`)
/// into a `(speaker_id, language_code)` pair. A bare integer (`"6"`) is
/// accepted for backwards compatibility and defaults the language to `"en"`;
/// `None`/empty yields speaker 0 + `"en"`.
fn parse_supertonic_voice(voice: Option<&str>) -> (i32, String) {
    const DEFAULT_LANG: &str = "en";
    match voice {
        None | Some("") => (0, DEFAULT_LANG.to_string()),
        Some(v) => match v.split_once(':') {
            Some((sid_str, lang)) => {
                let sid = sid_str.parse::<i32>().unwrap_or(0);
                let lang = if lang.is_empty() { DEFAULT_LANG } else { lang };
                (sid, lang.to_string())
            }
            None => (v.parse::<i32>().unwrap_or(0), DEFAULT_LANG.to_string()),
        },
    }
}

/// Build one [`Voice`] per `(speaker, language)` pair for a Supertonic model.
/// Each voice id is `"sid:lang"` so [`SherpaOnnxEngine::speak`] can recover
/// both. With 10 preset speakers × 31 languages this yields 310 voices; the
/// list is deliberately exhaustive so callers can discover every combination.
fn supertonic_voices(info: &SherpaModelInfo) -> Vec<Voice> {
    let mut voices = Vec::new();
    for lang in &info.language {
        let iso639 = iso639_3(&lang.lang_code);
        let display = crate::types::locale_display_name(&lang.lang_code);
        for sid in 0..info.num_speakers {
            voices.push(Voice {
                id: format!("{sid}:{}", lang.lang_code),
                name: format!("Speaker {sid} ({})", lang.language_name),
                gender: Gender::Unknown,
                provider: "sherpaonnx".to_string(),
                language_codes: vec![LanguageCode {
                    bcp47: lang.lang_code.clone(),
                    iso639_3: iso639.clone(),
                    display: display.clone(),
                }],
            });
        }
    }
    voices
}

/// Matcha config: acoustic-model.onnx + vocoder.onnx + tokens.txt.
///
/// Matcha tarballs ship the acoustic model only — the vocoder is a separate
/// download (commonly `hifigan_v2.onnx`, or `vocos-22khz-univ.onnx` for newer
/// models). We look for the vocoder in the model directory first, then fall
/// back to the user's base model dir so a single shared vocoder can be reused
/// across Matcha models.
fn build_matcha_config(
    model_dir: &std::path::Path,
    base_dir: &std::path::Path,
) -> TtsResult<sherpa_onnx::OfflineTtsMatchaModelConfig> {
    // Acoustic model: try the canonical names in order of prevalence.
    let acoustic = first_existing(
        model_dir,
        &[
            "acoustic-model.onnx",
            "model-steps-3.onnx",
            "model-steps-1000.onnx",
            "model.onnx",
        ],
    )
    .ok_or_else(|| TtsError("Matcha acoustic model not found".into()))?;

    // Vocoder: prefer co-located; fall back to shared in base_dir.
    let vocoder = first_existing(
        model_dir,
        &[
            "hifigan_v2.onnx",
            "hifigan_v2_en_zh.onnx",
            "hifigan_vitimator_v2.onnx",
            "vocos-22khz-univ.onnx",
            "vocoder.onnx",
        ],
    )
    .or_else(|| {
        first_existing(
            base_dir,
            &["vocos-22khz-univ.onnx", "hifigan_v2.onnx", "vocoder.onnx"],
        )
    });

    Ok(sherpa_onnx::OfflineTtsMatchaModelConfig {
        acoustic_model: Some(acoustic.to_string_lossy().to_string()),
        vocoder: vocoder.as_ref().map(|p| p.to_string_lossy().to_string()),
        lexicon: existing_path(model_dir, "lexicon.txt"),
        tokens: Some(model_dir.join("tokens.txt").to_string_lossy().to_string()),
        data_dir: existing_path(model_dir, "espeak-ng-data"),
        dict_dir: existing_path(model_dir, "dict"),
        ..Default::default()
    })
}

/// VITS-family config. The right combination of lexicon / data_dir / dict_dir
/// depends on where the model came from:
///
/// - Piper / GitHub models → prefer `espeak-ng-data/`, never `dict_dir`.
/// - Chinese/Cantonese models → use `dict/` for jieba segmentation.
/// - MMS and other VITS → `lexicon.txt` if present, else nothing.
fn build_vits_config(
    model_dir: &std::path::Path,
    is_piper_or_github: bool,
    is_chinese: bool,
) -> sherpa_onnx::OfflineTtsVitsModelConfig {
    // Try the canonical name first, then scan for any non-acoustic .onnx
    // (handles Piper's en_US-amy-low.onnx naming convention).
    let model = find_primary_model_onnx(model_dir)
        .or_else(|| first_existing(model_dir, &["vits-model.onnx", "generator.onnx"]))
        .unwrap_or_else(|| model_dir.join("model.onnx"));

    // Pick the right phonetic back-end.
    let (data_dir, dict_dir) = if is_piper_or_github {
        // Piper & friends ship espeak-ng-data; jieba would just complain.
        (existing_path(model_dir, "espeak-ng-data"), None)
    } else if is_chinese {
        // Chinese voices need jieba — point dict_dir at the bundled `dict/`.
        let dict = existing_path(model_dir, "dict").or_else(|| {
            // Some archives nest the dict directory under a child folder.
            std::fs::read_dir(model_dir).ok().and_then(|entries| {
                entries.filter_map(Result::ok).find_map(|e| {
                    let p = e.path();
                    if p.is_dir() && p.join("dict.txt").exists() {
                        Some(p.to_string_lossy().to_string())
                    } else {
                        None
                    }
                })
            })
        });
        (existing_path(model_dir, "espeak-ng-data"), dict)
    } else {
        // MMS / vanilla VITS: use espeak-ng-data if present, fall back to
        // a sibling dict/ directory only when lexicon.txt is absent.
        let has_lexicon = model_dir.join("lexicon.txt").exists();
        let dict = if has_lexicon {
            None
        } else {
            existing_path(model_dir, "dict")
        };
        (existing_path(model_dir, "espeak-ng-data"), dict)
    };

    sherpa_onnx::OfflineTtsVitsModelConfig {
        model: Some(model.to_string_lossy().to_string()),
        tokens: Some(model_dir.join("tokens.txt").to_string_lossy().to_string()),
        lexicon: existing_path(model_dir, "lexicon.txt"),
        data_dir,
        dict_dir,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_volume_and_pitch_identity() {
        let samples = [0.0_f32, 0.5, -0.5, 1.0, -1.0];
        let out = apply_volume_and_pitch(&samples, 1.0, 1.0);
        assert_eq!(out.len(), samples.len());
        for (a, b) in samples.iter().zip(out.iter()) {
            assert!((a - b).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_apply_volume_scales_amplitude() {
        let samples = [0.5_f32, -0.5];
        let out = apply_volume_and_pitch(&samples, 2.0, 1.0);
        assert!((out[0] - 1.0).abs() < f32::EPSILON);
        assert!((out[1] - (-1.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn test_apply_volume_zero_silences() {
        let samples = [0.5_f32, -0.25, 0.8];
        let out = apply_volume_and_pitch(&samples, 0.0, 1.0);
        for s in &out {
            assert!(s.abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_apply_pitch_changes_length() {
        // Pitch > 1.0 shortens the buffer (fewer samples); pitch < 1.0 lengthens.
        let samples = vec![0.5_f32; 100];
        let shorter = apply_volume_and_pitch(&samples, 1.0, 2.0);
        let longer = apply_volume_and_pitch(&samples, 1.0, 0.5);
        assert!(shorter.len() < samples.len());
        assert!(longer.len() > samples.len());
    }

    #[test]
    fn test_apply_volume_and_pitch_empty_input() {
        assert!(apply_volume_and_pitch(&[], 1.0, 1.0).is_empty());
        assert!(apply_volume_and_pitch(&[], 2.0, 0.5).is_empty());
    }

    #[test]
    fn test_is_piper_or_github_model_known_piper_prefix() {
        assert!(is_piper_or_github_model("piper-en-amy-medium"));
        assert!(is_piper_or_github_model("coqui-en-ljspeech"));
        assert!(is_piper_or_github_model("icefall-tts"));
        assert!(is_piper_or_github_model("mimic3-en"));
        assert!(is_piper_or_github_model("melo-en"));
        assert!(is_piper_or_github_model("vctk-en"));
        assert!(is_piper_or_github_model("zh-cantonese"));
        assert!(is_piper_or_github_model("ljs-en"));
        assert!(is_piper_or_github_model("cantonese-yue-xiaomaiiwn"));
        assert!(is_piper_or_github_model("kokoro-en-v0_19"));
    }

    #[test]
    fn test_is_piper_or_github_model_other_returns_false() {
        assert!(!is_piper_or_github_model("mms-en"));
        assert!(!is_piper_or_github_model("vits-en"));
        assert!(!is_piper_or_github_model("matcha-en"));
    }

    #[test]
    fn test_iso639_3_known_codes() {
        assert_eq!(iso639_3("en-US"), "eng");
        assert_eq!(iso639_3("es-ES"), "spa");
        assert_eq!(iso639_3("fr"), "fra");
        assert_eq!(iso639_3("de-DE"), "deu");
        assert_eq!(iso639_3("zh-CN"), "zho");
    }

    #[test]
    fn test_iso639_3_unknown_returns_input_lowercased() {
        // Unknown codes are returned lowercased (not "unknown") so callers
        // can still distinguish them in voice listings.
        assert_eq!(iso639_3("xx-XX"), "xx-xx");
        assert_eq!(iso639_3("Unknown-Lang"), "unknown-lang");
    }

    #[test]
    fn test_iso639_3_handles_underscore_separator() {
        // Some providers use BCP-47 with underscores; treat both.
        assert_eq!(iso639_3("en_US"), "eng");
        assert_eq!(iso639_3("pt_BR"), "por");
    }

    #[test]
    fn test_write_wav_round_trip_header() {
        // Write a known buffer, read the header back, validate.
        let dir = std::env::temp_dir();
        let path = dir.join("rtw_test_write_wav.wav");
        let samples = vec![0.0_f32, 0.5, -0.5, 1.0, -1.0];
        assert!(write_wav(&path, &samples, 22050));

        let bytes = std::fs::read(&path).expect("wav written");
        assert!(bytes.len() > 44, "WAV must include 44-byte header");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        // PCM format tag = 1, mono channels = 1.
        assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 1);
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1);
        // Sample rate little-endian.
        assert_eq!(
            u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            22050
        );
        // 16-bit per sample.
        assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 16);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_write_wav_clamps_samples() {
        // Samples outside [-1.0, 1.0] must clamp rather than wrap. The
        // writer scales by 32767 (not 32768) so the clamped min is -32767.
        let dir = std::env::temp_dir();
        let path = dir.join("rtw_test_write_wav_clamp.wav");
        let samples = vec![5.0_f32, -5.0]; // way out of range
        assert!(write_wav(&path, &samples, 16000));

        let bytes = std::fs::read(&path).expect("wav written");
        // PCM data starts at byte 44.
        let first_sample = i16::from_le_bytes([bytes[44], bytes[45]]);
        let second_sample = i16::from_le_bytes([bytes[46], bytes[47]]);
        assert_eq!(first_sample, 32767);
        assert_eq!(second_sample, -32767);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_first_existing_returns_first_match() {
        let dir = std::env::temp_dir();
        let a = dir.join("rtw_first_a.txt");
        let b = dir.join("rtw_first_b.txt");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"y").unwrap();

        let r = first_existing(&dir, &["rtw_first_a.txt", "rtw_first_b.txt"]).unwrap();
        assert_eq!(r.file_name().unwrap().to_str().unwrap(), "rtw_first_a.txt");

        // Order matters — first one wins.
        let r = first_existing(&dir, &["missing.txt", "rtw_first_b.txt"]).unwrap();
        assert_eq!(r.file_name().unwrap().to_str().unwrap(), "rtw_first_b.txt");

        // No matches.
        assert!(first_existing(&dir, &["nope.txt", "alsonope.txt"]).is_none());

        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn test_existing_path_only_when_present() {
        let dir = std::env::temp_dir();
        let p = dir.join("rtw_existing.txt");
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(
            existing_path(&dir, "rtw_existing.txt").as_deref(),
            Some(p.to_str().unwrap())
        );
        assert!(existing_path(&dir, "rtw_missing.txt").is_none());
        let _ = std::fs::remove_file(&p);
    }

    // ===== Model-config builders (one test per model family) =====
    //
    // These exercise the per-type dispatch in speak() without needing a real
    // sherpa-onnx runtime. Each test fakes the on-disk file layout for a
    // model family and verifies the resulting OfflineTts*ModelConfig points
    // at the expected paths.

    use std::fs;

    /// Build a temp directory resembling an extracted Kokoro archive.
    fn fake_kokoro_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tmp");
        fs::write(d.path().join("model.onnx"), b"x").unwrap();
        fs::write(d.path().join("voices.bin"), b"x").unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();
        fs::create_dir(d.path().join("espeak-ng-data")).unwrap();
        d
    }

    #[test]
    fn test_build_kokoro_config_points_at_canonical_files() {
        let d = fake_kokoro_dir();
        let cfg = build_kokoro_config(d.path());
        assert_eq!(
            cfg.model.as_deref(),
            Some(d.path().join("model.onnx").to_str().unwrap())
        );
        assert_eq!(
            cfg.voices.as_deref(),
            Some(d.path().join("voices.bin").to_str().unwrap())
        );
        assert_eq!(
            cfg.tokens.as_deref(),
            Some(d.path().join("tokens.txt").to_str().unwrap())
        );
        assert_eq!(
            cfg.data_dir.as_deref(),
            Some(d.path().join("espeak-ng-data").to_str().unwrap())
        );
    }

    #[test]
    fn test_build_kokoro_config_missing_files_are_none() {
        // Voices/data_dir are optional — their absence must surface as None
        // rather than a path to a nonexistent file.
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("model.onnx"), b"x").unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();
        // Intentionally no voices.bin or espeak-ng-data/.
        let cfg = build_kokoro_config(d.path());
        assert!(cfg.model.is_some());
        assert!(cfg.tokens.is_some());
        assert!(cfg.voices.is_none());
        assert!(cfg.data_dir.is_none());
    }

    fn fake_matcha_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("acoustic-model.onnx"), b"x").unwrap();
        fs::write(d.path().join("hifigan_v2.onnx"), b"x").unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();
        fs::write(d.path().join("lexicon.txt"), b"x").unwrap();
        d
    }

    #[test]
    fn test_build_matcha_config_finds_acoustic_and_vocoder() {
        let d = fake_matcha_dir();
        let base = tempfile::tempdir().unwrap();
        let cfg = build_matcha_config(d.path(), base.path()).expect("matcha config");
        assert!(cfg
            .acoustic_model
            .as_deref()
            .unwrap()
            .ends_with("acoustic-model.onnx"));
        assert!(cfg.vocoder.as_deref().unwrap().ends_with("hifigan_v2.onnx"));
        assert!(cfg.lexicon.is_some());
        assert!(cfg.tokens.is_some());
    }

    #[test]
    fn test_build_matcha_config_accepts_legacy_acoustic_names() {
        // Matcha archives have shipped several acoustic-model names; the
        // builder must accept any of them in priority order.
        for name in ["model-steps-3.onnx", "model-steps-1000.onnx", "model.onnx"] {
            let d = tempfile::tempdir().unwrap();
            fs::write(d.path().join(name), b"x").unwrap();
            fs::write(d.path().join("hifigan_v2.onnx"), b"x").unwrap();
            fs::write(d.path().join("tokens.txt"), b"x").unwrap();
            let base = tempfile::tempdir().unwrap();
            let cfg = build_matcha_config(d.path(), base.path()).expect("matcha config");
            assert!(cfg.acoustic_model.is_some(), "failed for acoustic {name}");
        }
    }

    #[test]
    fn test_build_matcha_config_vocoder_fallback_to_base_dir() {
        // Co-located vocoder is missing — fall back to a shared one in base.
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("acoustic-model.onnx"), b"x").unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();

        let base = tempfile::tempdir().unwrap();
        fs::write(base.path().join("vocos-22khz-univ.onnx"), b"x").unwrap();

        let cfg = build_matcha_config(d.path(), base.path()).expect("matcha config");
        assert!(cfg
            .vocoder
            .as_deref()
            .unwrap()
            .ends_with("vocos-22khz-univ.onnx"));
    }

    #[test]
    fn test_build_matcha_config_errors_without_acoustic() {
        let d = tempfile::tempdir().unwrap();
        // Only vocoder + tokens, no acoustic.
        fs::write(d.path().join("hifigan_v2.onnx"), b"x").unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();
        let base = tempfile::tempdir().unwrap();
        assert!(build_matcha_config(d.path(), base.path()).is_err());
    }

    // ===== Supertonic builder / voice parsing tests =====

    /// Build a temp directory resembling the extracted sherpa-onnx Supertonic
    /// int8 bundle: four `<name>.int8.onnx` files + tts.json +
    /// unicode_indexer.bin + voice.bin.
    fn fake_supertonic_int8_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tmp");
        for name in [
            "duration_predictor.int8.onnx",
            "text_encoder.int8.onnx",
            "vector_estimator.int8.onnx",
            "vocoder.int8.onnx",
        ] {
            fs::write(d.path().join(name), b"x").unwrap();
        }
        fs::write(d.path().join("tts.json"), b"{}").unwrap();
        fs::write(d.path().join("unicode_indexer.bin"), b"x").unwrap();
        fs::write(d.path().join("voice.bin"), b"x").unwrap();
        d
    }

    #[test]
    fn test_build_supertonic_config_resolves_all_int8_files() {
        let d = fake_supertonic_int8_dir();
        let cfg = build_supertonic_config(d.path());
        // All seven fields must resolve — sherpa-onnx rejects the config if
        // any required path is None.
        for (field, val) in [
            ("duration_predictor", cfg.duration_predictor.as_deref()),
            ("text_encoder", cfg.text_encoder.as_deref()),
            ("vector_estimator", cfg.vector_estimator.as_deref()),
            ("vocoder", cfg.vocoder.as_deref()),
        ] {
            assert!(
                val.is_some_and(|p| p.ends_with(&format!("{field}.int8.onnx"))),
                "{field} should resolve to its int8 onnx, got {val:?}"
            );
        }
        assert!(cfg.tts_json.is_some_and(|p| p.ends_with("tts.json")));
        assert!(cfg
            .unicode_indexer
            .is_some_and(|p| p.ends_with("unicode_indexer.bin")));
        assert!(cfg.voice_style.is_some_and(|p| p.ends_with("voice.bin")));
    }

    #[test]
    fn test_build_supertonic_config_falls_back_to_plain_onnx() {
        // When the int8 variants are absent, the builder must pick the plain
        // `<name>.onnx` files (some custom Supertonic layouts ship unquantised).
        let d = tempfile::tempdir().unwrap();
        for name in [
            "duration_predictor.onnx",
            "text_encoder.onnx",
            "vector_estimator.onnx",
            "vocoder.onnx",
        ] {
            fs::write(d.path().join(name), b"x").unwrap();
        }
        let cfg = build_supertonic_config(d.path());
        assert!(cfg
            .duration_predictor
            .as_deref()
            .unwrap()
            .ends_with("duration_predictor.onnx"));
        assert!(cfg.vocoder.as_deref().unwrap().ends_with("vocoder.onnx"));
    }

    #[test]
    fn test_build_supertonic_config_missing_files_are_none() {
        // An empty dir must produce None for every field rather than a path
        // to a nonexistent file (sherpa-onnx's create() handles the error).
        let d = tempfile::tempdir().unwrap();
        let cfg = build_supertonic_config(d.path());
        assert!(cfg.duration_predictor.is_none());
        assert!(cfg.text_encoder.is_none());
        assert!(cfg.vector_estimator.is_none());
        assert!(cfg.vocoder.is_none());
        assert!(cfg.tts_json.is_none());
        assert!(cfg.unicode_indexer.is_none());
        assert!(cfg.voice_style.is_none());
    }

    // ===== Zipvoice / pocket config builders =====

    /// Temp dir resembling the extracted zipvoice distill-int8 archive,
    /// plus a base dir holding the shared vocos vocoder.
    fn fake_zipvoice_dirs() -> (tempfile::TempDir, tempfile::TempDir) {
        let model = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        for name in [
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "encoder.onnx",
            "decoder.onnx",
            "tokens.txt",
            "lexicon.txt",
        ] {
            std::fs::write(model.path().join(name), b"x").unwrap();
        }
        std::fs::create_dir(model.path().join("espeak-ng-data")).unwrap();
        std::fs::write(base.path().join("vocos_24khz.onnx"), b"x").unwrap();
        (model, base)
    }

    #[test]
    fn test_build_zipvoice_config_prefers_int8_and_shared_vocoder() {
        let (model, base) = fake_zipvoice_dirs();
        let cfg = build_zipvoice_config(model.path(), base.path()).expect("config");
        assert!(cfg
            .encoder
            .as_deref()
            .is_some_and(|p| p.ends_with("encoder.int8.onnx")));
        assert!(cfg
            .decoder
            .as_deref()
            .is_some_and(|p| p.ends_with("decoder.int8.onnx")));
        assert!(cfg
            .vocoder
            .as_deref()
            .is_some_and(|p| p.starts_with(base.path().to_str().unwrap())));
        assert!(cfg
            .tokens
            .as_deref()
            .is_some_and(|p| p.ends_with("tokens.txt")));
        assert!(cfg
            .lexicon
            .as_deref()
            .is_some_and(|p| p.ends_with("lexicon.txt")));
        assert!(cfg
            .data_dir
            .as_deref()
            .is_some_and(|p| p.ends_with("espeak-ng-data")));
    }

    #[test]
    fn test_build_zipvoice_config_falls_back_to_plain_onnx() {
        // Unquantised archive (fp32 build): plain encoder/decoder must win.
        let (model, base) = fake_zipvoice_dirs();
        std::fs::remove_file(model.path().join("encoder.int8.onnx")).unwrap();
        std::fs::remove_file(model.path().join("decoder.int8.onnx")).unwrap();
        let cfg = build_zipvoice_config(model.path(), base.path()).expect("config");
        assert!(cfg
            .encoder
            .as_deref()
            .is_some_and(|p| p.ends_with("encoder.onnx")));
        assert!(cfg
            .decoder
            .as_deref()
            .is_some_and(|p| p.ends_with("decoder.onnx")));
    }

    #[test]
    fn test_build_zipvoice_config_missing_vocoder_errors_with_url() {
        // The vocos vocoder is never bundled — a clear error beats a
        // sherpa-onnx create() failure deep in the C++ runtime.
        let (model, base) = fake_zipvoice_dirs();
        std::fs::remove_file(base.path().join("vocos_24khz.onnx")).unwrap();
        let err = build_zipvoice_config(model.path(), base.path()).unwrap_err();
        assert!(err.0.contains("vocos_24khz.onnx"));
        assert!(err.0.contains("vocoder-models"));
    }

    /// Temp dir resembling the pocket-tts int8 archive: only decoder,
    /// lm_flow, and lm_main are quantised; encoder + text_conditioner are
    /// plain .onnx even in the int8 build.
    fn fake_pocket_int8_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for name in [
            "lm_flow.int8.onnx",
            "lm_main.int8.onnx",
            "encoder.onnx",
            "decoder.int8.onnx",
            "text_conditioner.onnx",
            "vocab.json",
            "token_scores.json",
        ] {
            std::fs::write(d.path().join(name), b"x").unwrap();
        }
        d
    }

    #[test]
    fn test_build_pocket_config_int8_layout() {
        let d = fake_pocket_int8_dir();
        let cfg = build_pocket_config(d.path());
        assert!(cfg
            .lm_flow
            .as_deref()
            .is_some_and(|p| p.ends_with("lm_flow.int8.onnx")));
        assert!(cfg
            .lm_main
            .as_deref()
            .is_some_and(|p| p.ends_with("lm_main.int8.onnx")));
        assert!(cfg
            .decoder
            .as_deref()
            .is_some_and(|p| p.ends_with("decoder.int8.onnx")));
        // Never quantised upstream — the plain files must be picked.
        assert!(cfg
            .encoder
            .as_deref()
            .is_some_and(|p| p.ends_with("encoder.onnx") && !p.contains("int8")));
        assert!(cfg
            .text_conditioner
            .as_deref()
            .is_some_and(|p| p.ends_with("text_conditioner.onnx")));
        assert!(cfg
            .vocab_json
            .as_deref()
            .is_some_and(|p| p.ends_with("vocab.json")));
        assert!(cfg
            .token_scores_json
            .as_deref()
            .is_some_and(|p| p.ends_with("token_scores.json")));
    }

    #[test]
    fn test_build_pocket_config_fp32_layout() {
        let d = tempfile::tempdir().unwrap();
        for name in [
            "lm_flow.onnx",
            "lm_main.onnx",
            "encoder.onnx",
            "decoder.onnx",
            "text_conditioner.onnx",
        ] {
            std::fs::write(d.path().join(name), b"x").unwrap();
        }
        let cfg = build_pocket_config(d.path());
        assert!(cfg
            .lm_flow
            .as_deref()
            .is_some_and(|p| p.ends_with("lm_flow.onnx")));
        assert!(cfg
            .decoder
            .as_deref()
            .is_some_and(|p| p.ends_with("decoder.onnx")));
    }

    // ===== Reference-audio / wav reader =====

    #[test]
    fn test_read_wav_mono_16bit_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("ref.wav");
        let samples = vec![0.0_f32, 0.25, -0.25, 0.5, -0.5];
        write_wav(&p, &samples, 24000);
        let (read, rate) = read_wav_mono_16bit(&p).expect("reads back");
        assert_eq!(rate, 24000);
        assert_eq!(read.len(), samples.len());
        for (a, b) in read.iter().zip(samples.iter()) {
            assert!((a - b).abs() < 0.001, "sample drifted: {a} vs {b}");
        }
    }

    #[test]
    fn test_read_wav_rejects_non_pcm16() {
        // Hand-roll a fmt chunk claiming 32-bit float (format 3).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&36u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&24000u32.to_le_bytes());
        bytes.extend_from_slice(&48000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes()); // 32-bit
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f32.wav");
        std::fs::write(&p, bytes).unwrap();
        let err = read_wav_mono_16bit(&p).unwrap_err();
        assert!(err.0.contains("16-bit PCM"));
    }

    #[test]
    fn test_bundled_reference_wav_prefers_test_wavs_sorted() {
        let d = tempfile::tempdir().unwrap();
        assert!(bundled_reference_wav(d.path()).is_none());
        std::fs::create_dir(d.path().join("test_wavs")).unwrap();
        std::fs::write(d.path().join("test_wavs/b.wav"), b"x").unwrap();
        std::fs::write(d.path().join("test_wavs/a.wav"), b"x").unwrap();
        std::fs::write(d.path().join("loose.wav"), b"x").unwrap();
        let wav = bundled_reference_wav(d.path()).expect("found");
        // Sorted order: a.wav before b.wav, and test_wavs/ wins over loose.
        assert!(wav.ends_with("test_wavs/a.wav"));
    }

    #[test]
    fn test_resolve_reference_text_bundled_transcript_and_overrides() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("test_wavs")).unwrap();
        std::fs::write(d.path().join("test_wavs/leijun-1.wav"), b"x").unwrap();

        // Known bundled wav → known transcript, no credentials needed.
        let engine = SherpaOnnxEngine::new(r#"{"modelId":"zipvoice-zh_en-emilia-distill-int8"}"#);
        let text = engine.resolve_reference_text(d.path()).expect("transcript");
        assert!(text.contains("武汉大学"));

        // User transcript wins.
        let engine = SherpaOnnxEngine::new(r#"{"modelId":"x","referenceText":"custom words"}"#);
        assert_eq!(
            engine.resolve_reference_text(d.path()).expect("override"),
            "custom words"
        );

        // Unknown wav + no override → actionable error.
        std::fs::write(d.path().join("test_wavs/mystery.wav"), b"x").unwrap();
        std::fs::remove_file(d.path().join("test_wavs/leijun-1.wav")).unwrap();
        let engine = SherpaOnnxEngine::new(r#"{"modelId":"x"}"#);
        let err = engine.resolve_reference_text(d.path()).unwrap_err();
        assert!(err.0.contains("referenceText"));
    }

    #[test]
    fn test_parse_supertonic_voice_sid_lang_form() {
        assert_eq!(parse_supertonic_voice(Some("6:ja")), (6, "ja".to_string()));
        assert_eq!(parse_supertonic_voice(Some("0:en")), (0, "en".to_string()));
        assert_eq!(parse_supertonic_voice(Some("9:ko")), (9, "ko".to_string()));
    }

    #[test]
    fn test_parse_supertonic_voice_bare_integer_defaults_en() {
        // Backwards compatibility: a plain speaker id picks the default lang.
        assert_eq!(parse_supertonic_voice(Some("6")), (6, "en".to_string()));
        assert_eq!(parse_supertonic_voice(Some("0")), (0, "en".to_string()));
    }

    #[test]
    fn test_parse_supertonic_voice_none_and_empty() {
        assert_eq!(parse_supertonic_voice(None), (0, "en".to_string()));
        assert_eq!(parse_supertonic_voice(Some("")), (0, "en".to_string()));
    }

    #[test]
    fn test_parse_supertonic_voice_empty_lang_after_colon() {
        // "6:" → sid 6, empty lang defaults to "en" rather than forwarding an
        // empty string that sherpa-onnx would reject.
        assert_eq!(parse_supertonic_voice(Some("6:")), (6, "en".to_string()));
    }

    #[test]
    fn test_parse_supertonic_voice_non_numeric_sid_falls_back_to_zero() {
        // A malformed sid must not panic — fall back to 0.
        assert_eq!(
            parse_supertonic_voice(Some("abc:ja")),
            (0, "ja".to_string())
        );
    }

    #[test]
    fn test_supertonic_voices_one_per_speaker_language_pair() {
        // 2 speakers × 3 languages = 6 voices, ids encoded as "sid:lang".
        let info = SherpaModelInfo {
            id: "test".into(),
            model_type: "supertonic".into(),
            engines: "sherpa-onnx".into(),
            name: "test".into(),
            language: vec![
                SherpaLanguage {
                    lang_code: "en".into(),
                    language_name: "English".into(),
                    country: String::new(),
                },
                SherpaLanguage {
                    lang_code: "ja".into(),
                    language_name: "Japanese".into(),
                    country: String::new(),
                },
                SherpaLanguage {
                    lang_code: "ko".into(),
                    language_name: "Korean".into(),
                    country: String::new(),
                },
            ],
            sample_rate: 24000,
            num_speakers: 2,
            quality: String::new(),
            url: String::new(),
            compression: false,
            filesize_mb: 0.0,
            license: String::new(),
            license_url: String::new(),
        };
        let voices = supertonic_voices(&info);
        assert_eq!(voices.len(), 6);
        // Each combination is present, and ids round-trip back to (sid, lang).
        let ids: Vec<&str> = voices.iter().map(|v| v.id.as_str()).collect();
        assert!(ids.contains(&"0:en"));
        assert!(ids.contains(&"1:ko"));
        assert!(ids.contains(&"0:ja"));
        // Voice carries the matching language code.
        let ja_voice = voices
            .iter()
            .find(|v| v.id == "1:ja")
            .expect("1:ja voice exists");
        assert_eq!(ja_voice.language_codes[0].bcp47, "ja");
        assert_eq!(ja_voice.provider, "sherpaonnx");
    }

    #[test]
    fn test_engine_get_voices_supertonic_enumerates_speakers_times_languages() {
        // Integration: the real registry entry must produce 10 speakers ×
        // 31 languages = 310 voices. This also verifies the JSON parses and
        // model_type "supertonic" routes through the expansion branch.
        let engine = SherpaOnnxEngine::new(r#"{"modelId":"supertonic-3-multilingual"}"#);
        if !engine.models.contains_key("supertonic-3-multilingual") {
            eprintln!(
                "skipping: 'supertonic-3-multilingual' missing from registry; \
                 check src/models.json"
            );
            return;
        }
        let voices = engine.get_voices().expect("voices");
        assert_eq!(voices.len(), 10 * 31);
        // Spot-check a known pair and the id encoding.
        assert!(voices.iter().any(|v| v.id == "6:ja"));
        // Every voice carries the sherpaonnx provider tag.
        assert!(voices.iter().all(|v| v.provider == "sherpaonnx"));
    }

    #[test]
    fn test_engine_num_steps_default_and_valid_override() {
        // No numSteps → default 8.
        assert_eq!(SherpaOnnxEngine::new("{}").num_steps, 8);
        // Valid in-range value passes through.
        assert_eq!(SherpaOnnxEngine::new(r#"{"numSteps":"10"}"#).num_steps, 10);
        assert_eq!(SherpaOnnxEngine::new(r#"{"numSteps":"5"}"#).num_steps, 5);
        assert_eq!(SherpaOnnxEngine::new(r#"{"numSteps":"12"}"#).num_steps, 12);
    }

    #[test]
    fn test_engine_num_steps_out_of_range_clamps_to_default() {
        // Below 5 and above 12 are outside Supertonic's supported range.
        assert_eq!(SherpaOnnxEngine::new(r#"{"numSteps":"3"}"#).num_steps, 8);
        assert_eq!(SherpaOnnxEngine::new(r#"{"numSteps":"20"}"#).num_steps, 8);
    }

    #[test]
    fn test_engine_num_steps_non_numeric_keeps_default() {
        assert_eq!(SherpaOnnxEngine::new(r#"{"numSteps":"fast"}"#).num_steps, 8);
    }

    fn fake_piper_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        // Piper archives use a domain-specific .onnx name rather than model.onnx.
        fs::write(d.path().join("en_US-amy-low.onnx"), b"x").unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();
        fs::create_dir(d.path().join("espeak-ng-data")).unwrap();
        d
    }

    #[test]
    fn test_build_vits_config_piper_uses_espeak_data_no_dict() {
        let d = fake_piper_dir();
        let cfg = build_vits_config(d.path(), true, false);
        // The model is found by scanning for the first non-vocoder .onnx.
        assert!(cfg
            .model
            .as_deref()
            .unwrap()
            .ends_with("en_US-amy-low.onnx"));
        assert!(cfg.data_dir.is_some(), "Piper needs espeak-ng-data");
        assert!(
            cfg.dict_dir.is_none(),
            "Piper must NOT set dict_dir (jieba would warn)"
        );
    }

    #[test]
    fn test_build_vits_config_chinese_uses_dict_dir() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("model.onnx"), b"x").unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();
        // Chinese models ship a dict/ directory for jieba segmentation.
        let dict_dir = d.path().join("dict");
        fs::create_dir(&dict_dir).unwrap();

        let cfg = build_vits_config(d.path(), false, true);
        assert_eq!(
            cfg.dict_dir.as_deref(),
            Some(dict_dir.to_str().unwrap()),
            "Chinese models must point dict_dir at bundled dict/"
        );
    }

    #[test]
    fn test_build_vits_config_mms_with_lexicon_no_dict() {
        // MMS-style: lexicon.txt present, no dict/, no espeak-ng-data.
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("model.onnx"), b"x").unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();
        fs::write(d.path().join("lexicon.txt"), b"x").unwrap();

        let cfg = build_vits_config(d.path(), false, false);
        assert!(cfg.lexicon.is_some());
        assert!(
            cfg.dict_dir.is_none(),
            "dict_dir must not be set when lexicon.txt is present"
        );
    }

    #[test]
    fn test_build_vits_config_mms_without_lexicon_uses_dict_fallback() {
        // MMS without lexicon.txt → fall back to dict/ if present.
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("model.onnx"), b"x").unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();
        fs::create_dir(d.path().join("dict")).unwrap();

        let cfg = build_vits_config(d.path(), false, false);
        assert!(cfg.dict_dir.is_some());
    }

    #[test]
    fn test_find_primary_model_onnx_prefers_canonical_name() {
        let d = tempfile::tempdir().unwrap();
        // Both model.onnx and a stray .onnx present — canonical wins.
        fs::write(d.path().join("model.onnx"), b"x").unwrap();
        fs::write(d.path().join("vits-en-foo.onnx"), b"x").unwrap();
        let r = find_primary_model_onnx(d.path()).expect("found");
        assert!(r.to_str().unwrap().ends_with("model.onnx"));
    }

    #[test]
    fn test_find_primary_model_onnx_skips_vocoders_and_acoustic_steps() {
        let d = tempfile::tempdir().unwrap();
        // Only vocoder/acoustic-steps files — none should match.
        fs::write(d.path().join("model-steps-3.onnx"), b"x").unwrap();
        fs::write(d.path().join("vocos-22khz-univ.onnx"), b"x").unwrap();
        fs::write(d.path().join("hifigan_v2.onnx"), b"x").unwrap();
        fs::write(d.path().join("vocoder.onnx"), b"x").unwrap();
        assert!(find_primary_model_onnx(d.path()).is_none());
    }

    #[test]
    fn test_find_primary_model_onnx_picks_first_unmatched() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("en_US-amy-low.onnx"), b"x").unwrap();
        let r = find_primary_model_onnx(d.path()).expect("found");
        assert!(r.to_str().unwrap().ends_with("en_US-amy-low.onnx"));
    }

    #[test]
    fn test_resolve_model_scan_dir_uses_top_when_files_present() {
        // If the top dir has tokens.txt or any .onnx, return it as-is.
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("tokens.txt"), b"x").unwrap();
        let r = resolve_model_scan_dir(d.path());
        assert_eq!(r, d.path());
    }

    #[test]
    fn test_resolve_model_scan_dir_descends_into_single_subdir() {
        // GitHub archives often extract to <name>/<name>/. If the outer dir
        // is empty except for a single subdir with the actual model, descend.
        let d = tempfile::tempdir().unwrap();
        let inner = d.path().join("vits-piper-en_US-amy-low");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join("model.onnx"), b"x").unwrap();

        let r = resolve_model_scan_dir(d.path());
        assert!(r.ends_with("vits-piper-en_US-amy-low"));
    }

    #[test]
    fn test_resolve_model_scan_dir_no_descent_when_multiple_subdirs() {
        // Ambiguous layout — don't guess, return the original.
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("a")).unwrap();
        fs::create_dir(d.path().join("b")).unwrap();
        let r = resolve_model_scan_dir(d.path());
        assert_eq!(r, d.path());
    }

    #[test]
    fn test_find_file_locates_named_child() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("foo.txt"), b"x").unwrap();
        let r = find_file(d.path(), "foo.txt").expect("found");
        assert!(r.to_str().unwrap().ends_with("foo.txt"));
        assert!(find_file(d.path(), "missing.txt").is_none());
    }

    // ===== SherpaOnnxEngine public-API tests (no model download) =====
    //
    // Construct engines with various modelId values and verify registry
    // lookup, voice enumeration, and graceful failure paths. None of these
    // need an actual model on disk because they exit before generate().

    #[test]
    fn test_engine_construction_with_model_id_does_not_load_yet() {
        // Setting a modelId is lazy — actual load happens on speak(). So
        // construction must succeed even when the model isn't downloaded.
        let engine = SherpaOnnxEngine::new(r#"{"modelId":"piper-en_US-amy-low"}"#);
        assert_eq!(engine.loaded_model_id, "piper-en_US-amy-low");
    }

    #[test]
    fn test_engine_construction_accepts_numeric_credential_values() {
        // JSON numbers must be coerced to strings, not silently dropped
        // (a strict HashMap<String, String> parse rejects the whole object).
        let engine = SherpaOnnxEngine::new(
            r#"{"modelId":"piper-en_US-amy-low","numThreads":4,"numSteps":10}"#,
        );
        assert_eq!(engine.loaded_model_id, "piper-en_US-amy-low");
        assert_eq!(engine.num_threads, 4);
        assert_eq!(engine.num_steps, 10);
    }

    #[test]
    fn test_engine_speak_without_model_id_errors_clearly() {
        let engine = SherpaOnnxEngine::new("");
        let err = engine
            .speak("hi", None, 1.0, 1.0, 1.0, None, None, None)
            .unwrap_err();
        assert!(
            err.to_string().contains("modelId"),
            "missing-model error should mention modelId: {err}"
        );
    }

    #[test]
    fn test_registry_crate_supplies_the_models() {
        // The registry now ships as the sherpa-onnx-models crate; the old
        // parse_model JSON path is gone. What used to be per-field parse
        // tests is now: registry is large, typed, and defaults sane.
        let models = load_models();
        assert!(models.len() > 1700, "registry shrank: {}", models.len());
        // engines covers every model and matches the family rule
        for m in models.values() {
            let expected = if matches!(m.model_type.as_str(), "vits" | "mms" | "matcha" | "kokoro")
            {
                "floravox"
            } else {
                "sherpa-onnx"
            };
            assert_eq!(m.engines, expected, "{}", m.id);
        }
        // a known MMS entry (no explicit model_type in raw data) parses
        // with the vits-family default
        let mms = models
            .values()
            .find(|m| m.id.starts_with("mms_"))
            .expect("MMS entries present");
        assert_eq!(mms.engines, "floravox");
    }

    #[test]
    fn test_engine_speak_with_unknown_model_id_errors_with_count() {
        let engine = SherpaOnnxEngine::new(r#"{"modelId":"not-a-real-model"}"#);
        let err = engine
            .speak("hi", None, 1.0, 1.0, 1.0, None, None, None)
            .unwrap_err();
        // Error message should hint at how many models ARE available so the
        // caller can pick a valid one.
        assert!(err.to_string().contains("not found in registry"));
        assert!(err.to_string().contains("models available"));
    }

    #[test]
    fn test_engine_get_voices_multi_speaker_enumeration() {
        // Pick a known multi-speaker model from the registry and verify
        // get_voices() enumerates `num_speakers` voice ids without needing
        // the actual model files (it reads from the registry only).
        let engine = SherpaOnnxEngine::new(r#"{"modelId":"coqui-en-vctk"}"#);
        // If this particular id isn't in the registry, skip loudly so the
        // test output shows the skip rather than passing vacuously. A
        // silent `return` here previously masked the test becoming a no-op
        // when the model id was renamed.
        let known = engine.models.contains_key("coqui-en-vctk");
        if !known {
            eprintln!(
                "skipping: 'coqui-en-vctk' is no longer in the registry; \
                 update the model id in this test"
            );
            return;
        }
        let voices = engine.get_voices().expect("voices");
        assert!(
            !voices.is_empty(),
            "expected at least 1 voice for multi-speaker model"
        );
        // All voices must carry the sherpaonnx provider tag.
        assert!(voices.iter().all(|v| v.provider == "sherpaonnx"));
    }

    #[test]
    fn test_engine_get_voices_single_speaker_returns_one() {
        // kokoro-en-v0_19 is actually an 11-speaker model (the registry now
        // carries the real count), so use a genuinely single-speaker voice.
        let engine = SherpaOnnxEngine::new(r#"{"modelId":"piper-en_US-amy-low"}"#);
        let voices = engine.get_voices().expect("voices");
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "0");
        assert_eq!(voices[0].name, "Speaker 0");
    }

    #[test]
    fn test_engine_get_voices_for_unloaded_model_id_returns_one_default() {
        // Unknown modelId: get_voices() still returns one voice (the
        // default speaker 0) rather than panicking.
        let engine = SherpaOnnxEngine::new(r#"{"modelId":"doesnt-matter"}"#);
        let voices = engine.get_voices().expect("voices");
        assert_eq!(voices.len(), 1);
    }

    #[test]
    fn test_engine_num_threads_parsed_from_credentials() {
        let engine = SherpaOnnxEngine::new(r#"{"numThreads":"4","provider":"cpu"}"#);
        assert_eq!(engine.num_threads, 4);
        assert_eq!(engine.provider.as_deref(), Some("cpu"));
    }

    #[test]
    fn test_engine_num_threads_invalid_falls_back_to_default() {
        let engine = SherpaOnnxEngine::new(r#"{"numThreads":"not-a-number"}"#);
        assert_eq!(engine.num_threads, 2); // default
    }

    #[test]
    fn test_engine_num_threads_zero_falls_back_to_default() {
        // 0 would cause sherpa-onnx to use no threads — clamp to default.
        let engine = SherpaOnnxEngine::new(r#"{"numThreads":"0"}"#);
        assert_eq!(engine.num_threads, 2);
    }

    #[test]
    fn test_engine_model_path_override() {
        let engine =
            SherpaOnnxEngine::new(r#"{"modelPath":"/tmp/custom-model-dir","modelId":"foo"}"#);
        assert_eq!(
            engine.model_dir,
            std::path::PathBuf::from("/tmp/custom-model-dir")
        );
        assert_eq!(engine.loaded_model_id, "foo");
    }
}
