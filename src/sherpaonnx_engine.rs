//! Sherpa-ONNX offline TTS engine with model registry.

use crate::engine::{estimate_word_boundaries, TtsEngine};
use crate::types::{
    Gender, LanguageCode, SherpaLanguage, SherpaModelInfo, TtsError, TtsResult, Voice,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Embedded model registry compiled from `merged_models.json`.
static MERGED_MODELS_JSON: &str = include_str!("merged_models.json");

/// Shared cancellation flag — set by `stop()`, read by the progress callback.
static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);

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
#[derive(Debug)]
pub struct SherpaOnnxEngine {
    models: HashMap<String, SherpaModelInfo>,
    model_dir: PathBuf,
    loaded_model_id: String,
    num_threads: i32,
    provider: Option<String>,
}

impl SherpaOnnxEngine {
    /// Create a new Sherpa-ONNX engine.
    ///
    /// Credentials JSON keys:
    /// - `modelPath`: directory containing downloaded models (defaults to
    ///   `~/.rust-tts-wrapper/sherpaonnx`)
    /// - `modelId`: id from the registry (e.g. `kokoro-en-en-19`). Required —
    ///   if absent, no model is loaded and `speak` will return an error rather
    ///   than silently forcing a 305 MB download.
    /// - `numThreads`: ONNX runtime intra-op thread count (default 2).
    /// - `provider`: `cpu` (default), `coreml`, `cuda`, `directml`, etc.
    pub fn new(credentials_json: &str) -> Self {
        let mut model_dir = default_model_dir();
        let mut model_id = String::new();
        let mut num_threads = 2;
        let mut provider: Option<String> = None;

        if !credentials_json.is_empty() {
            if let Ok(creds) = serde_json::from_str::<HashMap<String, String>>(credentials_json) {
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
                if let Some(p) = creds.get("provider") {
                    if !p.is_empty() {
                        provider = Some(p.clone());
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
            provider,
        }
    }

    /// Return the map of available models from the registry.
    pub fn available_models(&self) -> &HashMap<String, SherpaModelInfo> {
        &self.models
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

/// Parse the embedded `merged_models.json` into a hashmap.
fn load_models() -> HashMap<String, SherpaModelInfo> {
    let raw: HashMap<String, serde_json::Value> = match serde_json::from_str(MERGED_MODELS_JSON) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    let mut models = HashMap::new();
    for (key, val) in raw {
        if let Some(info) = parse_model(&key, &val) {
            models.insert(key, info);
        }
    }
    models
}

/// Parse a single model entry from the JSON registry.
fn parse_model(id: &str, val: &serde_json::Value) -> Option<SherpaModelInfo> {
    let obj = val.as_object()?;
    Some(SherpaModelInfo {
        id: id.to_string(),
        model_type: obj.get("model_type")?.as_str()?.to_string(),
        name: obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        language: obj
            .get("language")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| {
                        let lo = l.as_object()?;
                        Some(SherpaLanguage {
                            lang_code: lo.get("lang_code")?.as_str()?.to_string(),
                            language_name: lo.get("language_name")?.as_str()?.to_string(),
                            country: lo
                                .get("country")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        sample_rate: obj
            .get("sample_rate")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(24000) as u32,
        num_speakers: obj
            .get("num_speakers")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1) as u32,
        url: obj
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        compression: obj
            .get("compression")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        filesize_mb: obj
            .get("filesize_mb")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
    })
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

        // Dispatch model config by model_type. Each branch honours the file
        // layout documented in the Sherpa-ONNX model registry.
        let model_config = match model_info.model_type.as_str() {
            "kokoro" => sherpa_onnx::OfflineTtsModelConfig {
                kokoro: sherpa_onnx::OfflineTtsKokoroModelConfig {
                    model: Some(model_dir.join("model.onnx").to_string_lossy().to_string()),
                    voices: Some(model_dir.join("voices.bin").to_string_lossy().to_string()),
                    tokens: Some(model_dir.join("tokens.txt").to_string_lossy().to_string()),
                    data_dir: Some(
                        model_dir
                            .join("espeak-ng-data")
                            .to_string_lossy()
                            .to_string(),
                    ),
                    // length_scale is left at default — rate is applied via
                    // GenerationConfig.speed to avoid the double-rate bug.
                    ..Default::default()
                },
                num_threads: self.num_threads,
                debug: false,
                provider: self.provider.clone(),
                ..Default::default()
            },
            // VITS, MMS (Facebook Massively Multilingual Speech), and unknown
            // model types all use the VITS config family. The merged_models
            // registry has ~1143 MMS entries that omit `model_type`, so we
            // fall through to VITS rather than reject them.
            "vits" | "mms" | "unknown" | "" => {
                let lexicon = model_dir.join("lexicon.txt");
                let dict_dir = model_dir.join("dict");
                let espeak_data = model_dir.join("espeak-ng-data");
                sherpa_onnx::OfflineTtsModelConfig {
                    vits: sherpa_onnx::OfflineTtsVitsModelConfig {
                        model: Some(model_dir.join("model.onnx").to_string_lossy().to_string()),
                        tokens: Some(model_dir.join("tokens.txt").to_string_lossy().to_string()),
                        lexicon: if lexicon.exists() {
                            Some(lexicon.to_string_lossy().to_string())
                        } else {
                            None
                        },
                        data_dir: if espeak_data.exists() {
                            Some(espeak_data.to_string_lossy().to_string())
                        } else {
                            None
                        },
                        dict_dir: if dict_dir.exists() {
                            Some(dict_dir.to_string_lossy().to_string())
                        } else {
                            None
                        },
                        ..Default::default()
                    },
                    num_threads: self.num_threads,
                    debug: false,
                    provider: self.provider.clone(),
                    ..Default::default()
                }
            }
            "matcha" => {
                // Matcha models use acoustic-model.onnx + a vocoder (hifigan_v2.onnx).
                // Try the common naming variants seen in the registry.
                let acoustic = first_existing(
                    &model_dir,
                    &["acoustic-model.onnx", "model-steps-3.onnx", "model.onnx"],
                );
                let vocoder = first_existing(
                    &model_dir,
                    &["hifigan_v2.onnx", "hifigan_v2_en_zh.onnx", "vocoder.onnx"],
                );
                let tokens = model_dir.join("tokens.txt");
                let dict_dir = model_dir.join("dict");
                sherpa_onnx::OfflineTtsModelConfig {
                    matcha: sherpa_onnx::OfflineTtsMatchaModelConfig {
                        acoustic_model: Some(
                            acoustic
                                .map(|p| p.to_string_lossy().to_string())
                                .ok_or_else(|| {
                                    TtsError("Matcha acoustic model not found".into())
                                })?,
                        ),
                        vocoder: vocoder.as_ref().map(|p| p.to_string_lossy().to_string()),
                        lexicon: None,
                        tokens: Some(tokens.to_string_lossy().to_string()),
                        data_dir: None,
                        dict_dir: if dict_dir.exists() {
                            Some(dict_dir.to_string_lossy().to_string())
                        } else {
                            None
                        },
                        ..Default::default()
                    },
                    num_threads: self.num_threads,
                    debug: false,
                    provider: self.provider.clone(),
                    ..Default::default()
                }
            }
            "kitten" => sherpa_onnx::OfflineTtsModelConfig {
                kitten: sherpa_onnx::OfflineTtsKittenModelConfig {
                    model: Some(model_dir.join("model.onnx").to_string_lossy().to_string()),
                    voices: Some(model_dir.join("voices.bin").to_string_lossy().to_string()),
                    tokens: Some(model_dir.join("tokens.txt").to_string_lossy().to_string()),
                    data_dir: Some(
                        model_dir
                            .join("espeak-ng-data")
                            .to_string_lossy()
                            .to_string(),
                    ),
                    ..Default::default()
                },
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
            ..Default::default()
        };

        let tts = sherpa_onnx::OfflineTts::create(&config)
            .ok_or_else(|| TtsError("Failed to create SherpaOnnx TTS engine".into()))?;

        let sid = voice.and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
        let gen_config = sherpa_onnx::GenerationConfig {
            sid,
            speed: rate.max(0.1),
            ..Default::default()
        };

        // Reset cancellation flag before synthesis.
        CANCEL_REQUESTED.store(false, Ordering::SeqCst);

        let audio = tts
            .generate_with_config(
                text,
                &gen_config,
                Some(|_samples: &[f32], _progress: f32| -> bool {
                    // Return false to stop in-progress synthesis when stop() was called.
                    !CANCEL_REQUESTED.load(Ordering::SeqCst)
                }),
            )
            .ok_or_else(|| TtsError("SherpaOnnx synthesis returned no audio".into()))?;

        // Post-process samples for volume and pitch since the underlying
        // models don't natively expose these controls.
        let samples = audio.samples();
        let volume_factor = volume.clamp(0.0, 4.0);
        let pitch_factor = pitch.clamp(0.25, 4.0);
        let processed: Vec<f32> = if (volume_factor - 1.0).abs() > f32::EPSILON
            || (pitch_factor - 1.0).abs() > f32::EPSILON
        {
            apply_volume_and_pitch(samples, volume_factor, pitch_factor)
        } else {
            samples.to_vec()
        };

        if let Some(cb) = on_audio.as_mut() {
            let mut pcm_bytes = Vec::with_capacity(processed.len() * 2);
            for &s in &processed {
                let s16 = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                pcm_bytes.extend_from_slice(&s16.to_ne_bytes());
            }
            cb(&pcm_bytes);
        } else {
            let filename = std::env::temp_dir().join("rust-tts-wrapper-sherpa.wav");
            if write_wav(&filename, &processed, audio.sample_rate()) {
                play_wav_file(&filename);
            }
        }

        if let Some(cb) = on_boundary.as_mut() {
            let estimated = estimate_word_boundaries(text);
            for b in &estimated {
                #[allow(clippy::cast_precision_loss)]
                let start = b.offset as f32 / 1000.0;
                #[allow(clippy::cast_precision_loss)]
                let end = (b.offset + b.duration) as f32 / 1000.0;
                cb(&b.text, start, end);
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
    ) -> TtsResult<()> {
        self.speak(text, voice, rate, pitch, volume, on_audio, on_boundary)
    }

    fn stop(&self) -> TtsResult<()> {
        // The progress callback reads this flag on every chunk and aborts
        // synthesis when set.
        CANCEL_REQUESTED.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        let model_info = self.models.get(&self.loaded_model_id);
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
                    display: lang.clone(),
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

/// Return the first existing file from `dir` matching one of `names`.
fn first_existing(dir: &std::path::Path, names: &[&str]) -> Option<std::path::PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.exists())
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
    let block_align: u32 = 2;
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
        &16u16.to_le_bytes(), // bits per sample
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
