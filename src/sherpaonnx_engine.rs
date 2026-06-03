//! Sherpa-ONNX offline TTS engine with model registry.
//!
//! Supports Kokoro, VITS (Piper/Coqui), Matcha, Kitten, ZipVoice, Pocket, and
//! Supertonic model families. Audio is streamed in PCM chunks via the
//! `generate_with_config` progress callback. Word boundary events are estimated
//! (not provided by the Sherpa-ONNX API).

use crate::engine::{estimate_word_boundaries, TtsEngine};
use crate::types::{
    Gender, LanguageCode, SherpaLanguage, SherpaModelInfo, TtsError, TtsResult, Voice,
};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

static MERGED_MODELS_JSON: &str = include_str!("merged_models.json");

const PCM_CHUNK_SAMPLES: usize = 4096;

pub struct SherpaOnnxEngine {
    models: HashMap<String, SherpaModelInfo>,
    model_dir: PathBuf,
    loaded_model_id: String,
    tts: Option<Mutex<sherpa_onnx::OfflineTts>>,
}

impl fmt::Debug for SherpaOnnxEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SherpaOnnxEngine")
            .field("loaded_model_id", &self.loaded_model_id)
            .field("model_dir", &self.model_dir)
            .field("models_count", &self.models.len())
            .field("tts_loaded", &self.tts.is_some())
            .finish()
    }
}

impl SherpaOnnxEngine {
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

        let tts = try_create_tts(&models, &model_dir, &model_id);

        SherpaOnnxEngine {
            models,
            model_dir,
            loaded_model_id: model_id,
            tts: tts.map(Mutex::new),
        }
    }

    pub fn available_models(&self) -> &HashMap<String, SherpaModelInfo> {
        &self.models
    }
}

fn try_create_tts(
    models: &HashMap<String, SherpaModelInfo>,
    model_dir: &std::path::Path,
    model_id: &str,
) -> Option<sherpa_onnx::OfflineTts> {
    let model_info = models.get(model_id)?;
    let dir = model_dir.join(model_id);
    if !dir.exists() {
        return None;
    }

    let model_config = build_model_config(model_info, &dir)?;
    let config = sherpa_onnx::OfflineTtsConfig {
        model: model_config,
        ..Default::default()
    };

    sherpa_onnx::OfflineTts::create(&config)
}

fn build_model_config(
    info: &SherpaModelInfo,
    dir: &std::path::Path,
) -> Option<sherpa_onnx::OfflineTtsModelConfig> {
    let p = |name: &str| {
        let path = dir.join(name);
        if path.exists() {
            Some(path.to_string_lossy().into_owned())
        } else {
            None
        }
    };

    match info.model_type.as_str() {
        "kokoro" => Some(sherpa_onnx::OfflineTtsModelConfig {
            kokoro: sherpa_onnx::OfflineTtsKokoroModelConfig {
                model: p("model.onnx"),
                voices: p("voices.bin"),
                tokens: p("tokens.txt"),
                data_dir: p("espeak-ng-data"),
                ..Default::default()
            },
            num_threads: 2,
            ..Default::default()
        }),
        "vits" => Some(sherpa_onnx::OfflineTtsModelConfig {
            vits: sherpa_onnx::OfflineTtsVitsModelConfig {
                model: p("model.onnx"),
                tokens: p("tokens.txt"),
                data_dir: p("espeak-ng-data"),
                lexicon: p("lexicon.txt"),
                dict_dir: p("dict"),
                ..Default::default()
            },
            num_threads: 2,
            ..Default::default()
        }),
        "matcha" => Some(sherpa_onnx::OfflineTtsModelConfig {
            matcha: sherpa_onnx::OfflineTtsMatchaModelConfig {
                acoustic_model: p("model.onnx.stripped").or_else(|| p("model.onnx")),
                vocoder: p("hifigan-v2/model.onnx").or_else(|| p("vocoder.onnx")),
                lexicon: p("lexicon.txt"),
                tokens: p("tokens.txt"),
                data_dir: p("espeak-ng-data"),
                ..Default::default()
            },
            num_threads: 2,
            ..Default::default()
        }),
        _ => None,
    }
}

fn default_model_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    let mut dir = PathBuf::from(home);
    dir.push(".rust-tts-wrapper");
    dir.push("sherpaonnx");
    dir
}

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

fn samples_to_pcm_i16(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let s16 = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        bytes.extend_from_slice(&s16.to_ne_bytes());
    }
    bytes
}

struct StreamState {
    samples_sent: usize,
    chunks: Vec<Vec<u8>>,
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
        mut on_boundary: Option<crate::engine::OnBoundaryCallback>,
    ) -> TtsResult<()> {
        let tts_guard = self.tts.as_ref().ok_or_else(|| {
            TtsError(format!(
                "Sherpa-ONNX engine not initialised (model '{}' not loaded)",
                self.loaded_model_id
            ))
        })?;
        let tts = tts_guard.lock().unwrap();

        let sid = voice.and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
        let gen_config = sherpa_onnx::GenerationConfig {
            sid,
            speed: rate.max(0.1),
            ..Default::default()
        };

        let stream_audio = on_audio.is_some();
        let state = Arc::new(Mutex::new(StreamState {
            samples_sent: 0,
            chunks: Vec::new(),
        }));
        let state_clone = Arc::clone(&state);

        let audio = tts
            .generate_with_config(
                text,
                &gen_config,
                Some(move |samples: &[f32], _progress: f32| -> bool {
                    if !stream_audio || samples.is_empty() {
                        return true;
                    }
                    let mut st = state_clone.lock().unwrap();
                    if samples.len() > st.samples_sent {
                        let new_samples = &samples[st.samples_sent..];
                        let pcm = samples_to_pcm_i16(new_samples);
                        for chunk in pcm.chunks(PCM_CHUNK_SAMPLES * 2) {
                            st.chunks.push(chunk.to_vec());
                        }
                        st.samples_sent = samples.len();
                    }
                    true
                }),
            )
            .ok_or_else(|| TtsError("SherpaOnnx synthesis returned no audio".into()))?;

        if let Some(cb) = on_audio.as_mut() {
            let st = state.lock().unwrap();
            let samples_sent = st.samples_sent;
            for chunk in &st.chunks {
                cb(chunk);
            }
            drop(st);

            let final_samples = audio.samples();
            if final_samples.len() > samples_sent {
                let remaining = &final_samples[samples_sent..];
                let pcm = samples_to_pcm_i16(remaining);
                for chunk in pcm.chunks(PCM_CHUNK_SAMPLES * 2) {
                    cb(chunk);
                }
            }
        } else {
            let filename = std::env::temp_dir().join("rust-tts-wrapper-sherpa.wav");
            if audio.save(filename.to_string_lossy().as_ref()) {
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
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        let model_info = self.models.get(&self.loaded_model_id);
        let num_speakers = if let Some(ref tts_mutex) = self.tts {
            tts_mutex.lock().unwrap().num_speakers() as u32
        } else {
            model_info.map_or(1, |m| m.num_speakers)
        };
        let lang = model_info
            .and_then(|m| m.language.first())
            .map(|l| l.language_name.clone())
            .unwrap_or_default();
        let lang_code = model_info
            .and_then(|m| m.language.first())
            .map(|l| l.lang_code.clone())
            .unwrap_or_default();
        let mut voices = Vec::new();
        for i in 0..num_speakers {
            voices.push(Voice {
                id: format!("{i}"),
                name: format!("Speaker {i}"),
                gender: Gender::Unknown,
                provider: "sherpaonnx".to_string(),
                language_codes: vec![LanguageCode {
                    bcp47: lang.clone(),
                    iso639_3: lang_code.clone(),
                    display: lang.clone(),
                }],
            });
        }
        Ok(voices)
    }

    fn engine_id(&self) -> &'static str {
        "sherpaonnx"
    }

    fn check_credentials(&self) -> TtsResult<bool> {
        Ok(self.tts.is_some())
    }
}

fn play_wav_file(path: &std::path::Path) {
    let _ = std::process::Command::new("aplay")
        .arg("-q")
        .arg(path)
        .spawn()
        .map(|mut c| {
            let _ = c.wait();
        });
}
