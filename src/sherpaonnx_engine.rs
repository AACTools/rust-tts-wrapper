//! Sherpa-ONNX offline TTS engine with 191-model registry.

use crate::engine::TtsEngine;
use crate::types::{SherpaLanguage, SherpaModelInfo, TtsError, TtsResult, Voice};
use std::collections::HashMap;
use std::path::PathBuf;

/// Embedded model registry compiled from `merged_models.json`.
static MERGED_MODELS_JSON: &str = include_str!("merged_models.json");

/// Offline TTS engine using [Sherpa-ONNX](https://github.com/k2-fsa/sherpa-onnx).
///
/// Models are looked up from the compiled-in registry and loaded from
/// `~/.rust-tts-wrapper/sherpaonnx/<model_id>/`. Audio is synthesised
/// offline and played via `aplay` (Linux).
#[derive(Debug)]
pub struct SherpaOnnxEngine {
    models: HashMap<String, SherpaModelInfo>,
    model_dir: PathBuf,
    loaded_model_id: String,
}

impl SherpaOnnxEngine {
    /// Create a new Sherpa-ONNX engine.
    ///
    /// `credentials_json` may contain `"modelPath"` and `"modelId"` keys.
    /// If empty, defaults to `~/.rust-tts-wrapper/sherpaonnx/` and the
    /// `kokoro-en-en-19` model.
    pub fn new(credentials_json: &str) -> Self {
        let mut model_dir = default_model_dir();
        let mut model_id = String::new();

        if !credentials_json.is_empty() {
            if let Ok(creds) = serde_json::from_str::<HashMap<String, String>>(credentials_json) {
                if let Some(dir) = creds.get("modelPath") {
                    model_dir = PathBuf::from(dir);
                }
                if let Some(id) = creds.get("modelId") {
                    model_id.clone_from(id);
                }
            }
        }

        let models = load_models();

        if model_id.is_empty() && !models.is_empty() {
            model_id = "kokoro-en-en-19".to_string();
        }

        SherpaOnnxEngine {
            models,
            model_dir,
            loaded_model_id: model_id,
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
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        _volume: f32,
        mut on_audio: Option<crate::engine::OnAudioCallback>,
        _on_boundary: Option<crate::engine::OnBoundaryCallback>,
    ) -> TtsResult<()> {
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

        let kokoro = sherpa_onnx::OfflineTtsKokoroModelConfig {
            model: Some(model_dir.join("model.onnx").to_string_lossy().to_string()),
            voices: Some(model_dir.join("voices.bin").to_string_lossy().to_string()),
            tokens: Some(model_dir.join("tokens.txt").to_string_lossy().to_string()),
            data_dir: Some(
                model_dir
                    .join("espeak-ng-data")
                    .to_string_lossy()
                    .to_string(),
            ),
            length_scale: 1.0 / rate.max(0.1),
            ..Default::default()
        };

        let model_config = sherpa_onnx::OfflineTtsModelConfig {
            kokoro,
            vits: sherpa_onnx::OfflineTtsVitsModelConfig::default(),
            matcha: sherpa_onnx::OfflineTtsMatchaModelConfig::default(),
            kitten: sherpa_onnx::OfflineTtsKittenModelConfig::default(),
            zipvoice: sherpa_onnx::OfflineTtsZipvoiceModelConfig::default(),
            pocket: sherpa_onnx::OfflineTtsPocketModelConfig::default(),
            supertonic: sherpa_onnx::OfflineTtsSupertonicModelConfig::default(),
            num_threads: 2,
            debug: false,
            provider: None,
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

        let audio = tts
            .generate_with_config(
                text,
                &gen_config,
                Some(|_samples: &[f32], _progress: f32| -> bool { true }),
            )
            .ok_or_else(|| TtsError("SherpaOnnx synthesis returned no audio".into()))?;

        if let Some(cb) = on_audio.as_mut() {
            // SherpaOnnx C API does not currently easily support streaming inside the progress callback without
            // borrowing issues, because the closure requires `'static`.
            // So we stream all at once right after generation, to still simulate stream interface.
            let samples = audio.samples();
            let mut pcm_bytes = Vec::with_capacity(samples.len() * 2);
            for &s in samples {
                let s16 = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                pcm_bytes.extend_from_slice(&s16.to_ne_bytes());
            }
            cb(&pcm_bytes);
        } else {
            let filename = std::env::temp_dir().join("rust-tts-wrapper-sherpa.wav");
            if audio.save(filename.to_string_lossy().as_ref()) {
                play_wav_file(&filename);
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
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        let model_info = self.models.get(&self.loaded_model_id);
        let num_speakers = model_info.map_or(1, |m| m.num_speakers);
        let lang = model_info
            .and_then(|m| m.language.first())
            .map(|l| l.language_name.clone())
            .unwrap_or_default();
        let mut voices = Vec::new();
        for i in 0..num_speakers {
            voices.push(Voice {
                id: format!("{i}"),
                name: format!("Speaker {i}"),
                language: lang.clone(),
                gender: String::new(),
                engine: "sherpaonnx".to_string(),
            });
        }
        Ok(voices)
    }

    fn engine_id(&self) -> &'static str {
        "sherpaonnx"
    }
}

/// Play a WAV file using `aplay` (Linux).
fn play_wav_file(path: &std::path::Path) {
    let _ = std::process::Command::new("aplay")
        .arg("-q")
        .arg(path)
        .spawn()
        .map(|mut c| {
            let _ = c.wait();
        });
}
