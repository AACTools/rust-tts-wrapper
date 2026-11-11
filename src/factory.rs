//! Engine factory: create engines by ID and list all registered engines.

use crate::engine::TtsEngine;
use crate::error_codes::TtsErrorCode;
use crate::types::EngineDescriptor;
use std::sync::Arc;

/// Structured engine-setup failure (GH #69). Carries a stable code, the
/// offending field key when the problem is a missing/invalid setting,
/// and the engine id — enough for a settings screen to point the user
/// at the exact row to fix.
#[derive(Debug, Clone)]
pub struct EngineSetupError {
    /// Stable classification (see [`TtsErrorCode`]).
    pub code: TtsErrorCode,
    /// Engine the failure belongs to.
    pub engine_id: String,
    /// Offending credential/setup field, when known.
    pub field_key: Option<String>,
    /// Human-readable detail (already redacted by the constructor).
    pub message: String,
}

impl EngineSetupError {
    #[must_use]
    pub fn new(
        code: TtsErrorCode,
        engine_id: impl Into<String>,
        field_key: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            engine_id: engine_id.into(),
            field_key: field_key.map(std::string::ToString::to_string),
            message: crate::error_codes::redact_message(&message.into()),
        }
    }

    /// The "engine compiled out / unknown id" error.
    #[must_use]
    pub fn not_available(engine_id: &str, reason: &str) -> Self {
        Self::new(
            TtsErrorCode::EngineNotAvailable,
            engine_id,
            None,
            format!("Engine '{engine_id}' is not available: {reason}"),
        )
    }
}

impl std::fmt::Display for EngineSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for EngineSetupError {}

impl From<EngineSetupError> for crate::types::TtsError {
    fn from(e: EngineSetupError) -> Self {
        crate::types::TtsError(e.message)
    }
}

/// Parse engine credentials JSON into a string map, coercing JSON scalars
/// (numbers, bools) to their string form. Fixes #58: a single
/// `{"numSteps": 16}` (JSON number) no longer silently empties ALL
/// credentials — it coerces to `"16"` like the engine code expects.
#[must_use]
pub fn parse_credentials(credentials_json: &str) -> std::collections::HashMap<String, String> {
    if credentials_json.is_empty() {
        return std::collections::HashMap::new();
    }
    let parsed: std::collections::HashMap<String, serde_json::Value> =
        match serde_json::from_str(credentials_json) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "factory: credentials JSON is malformed ({e}) — \
                     engine will start with no credentials"
                );
                return std::collections::HashMap::new();
            }
        };
    parsed
        .into_iter()
        .map(|(k, v)| {
            let s = match v {
                serde_json::Value::String(s) => s,
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => String::new(),
                // objects/arrays become their JSON repr (rarely useful,
                // but at least visible in error messages)
                other => other.to_string(),
            };
            (k, s)
        })
        .collect()
}

// The unused-import warning is a false positive — TtsEngine is a trait used as a dyn bound.
#[cfg(all(feature = "avsynth", target_os = "macos"))]
use crate::avsynth_engine::AvSynthEngine;
#[cfg(feature = "cloud")]
use crate::cloud_engine;
#[cfg(feature = "qwen3-local")]
use crate::qwen3_local_engine::Qwen3LocalEngine;
#[cfg(all(feature = "sapi", target_os = "windows"))]
use crate::sapi_engine::SapiEngine;
#[cfg(feature = "sherpaonnx")]
use crate::sherpaonnx_engine::SherpaOnnxEngine;
#[cfg(all(feature = "system", target_os = "linux"))]
use crate::system_engine::SystemEngine;

/// Create an engine by its string identifier.
///
/// `credentials_json` is a JSON object with engine-specific credentials
/// (e.g. `{"apiKey": "..."}`). Pass `""` for engines that don't need credentials.
///
/// # Errors
/// Returns [`EngineSetupError`] with a stable code (GH #69):
/// `EngineNotAvailable` for unknown/compiled-out ids,
/// `MissingConfiguration` (with the field key) when a required local
/// setup path is absent, and engine-specific codes for load failures.
#[allow(unused_variables)]
#[allow(clippy::too_many_lines)]
pub fn create_engine(
    engine_id: &str,
    credentials_json: &str,
) -> Result<Arc<dyn TtsEngine>, EngineSetupError> {
    // Detect engines that exist in the full catalogue but were compiled out
    // by disabled features, so callers get a useful error rather than a
    // generic "unknown engine". Only emit these messages when the
    // caller actually asked for one of the gated engines.
    match engine_id {
        "system" => {
            #[cfg(all(feature = "system", target_os = "linux"))]
            {
                return Ok(Arc::new(SystemEngine::new()));
            }
            #[cfg(not(all(feature = "system", target_os = "linux")))]
            {
                return Err(EngineSetupError::not_available(
                    engine_id,
                    "requires the 'system' feature on Linux",
                ));
            }
        }
        "avsynth" => {
            #[cfg(all(feature = "avsynth", target_os = "macos"))]
            {
                return Ok(Arc::new(AvSynthEngine::new()));
            }
            #[cfg(not(all(feature = "avsynth", target_os = "macos")))]
            {
                return Err(EngineSetupError::not_available(
                    engine_id,
                    "requires the 'avsynth' feature and macOS",
                ));
            }
        }
        "sapi" => {
            #[cfg(all(feature = "sapi", target_os = "windows"))]
            {
                return Ok(Arc::new(SapiEngine::new()));
            }
            #[cfg(not(all(feature = "sapi", target_os = "windows")))]
            {
                return Err(EngineSetupError::not_available(
                    engine_id,
                    "requires the 'sapi' feature and Windows",
                ));
            }
        }
        "sherpaonnx" => {
            #[cfg(feature = "sherpaonnx")]
            {
                return Ok(Arc::new(SherpaOnnxEngine::new(credentials_json)));
            }
            #[cfg(not(feature = "sherpaonnx"))]
            {
                return Err(EngineSetupError::not_available(
                    engine_id,
                    "not enabled in this build — rebuild with --features sherpaonnx",
                ));
            }
        }
        "qwen3-local" => {
            #[cfg(feature = "qwen3-local")]
            {
                let creds = parse_credentials(credentials_json);
                return match Qwen3LocalEngine::new(&creds) {
                    Ok(engine) => Ok(Arc::new(engine)),
                    Err(e) => Err(fielded_setup_error(engine_id, &creds, e.0, "modelsDir")),
                };
            }
            #[cfg(not(feature = "qwen3-local"))]
            {
                return Err(EngineSetupError::not_available(
                    engine_id,
                    "not enabled in this build — rebuild with --features qwen3-local",
                ));
            }
        }
        "omnivoice-local" => {
            #[cfg(feature = "omnivoice-local")]
            {
                use crate::omnivoice_local_engine::{OmniVoiceLocalEngine, OmniVoiceLocalParams};
                let creds = parse_credentials(credentials_json);
                // Name the first missing path so the error carries the
                // exact field key (GH #69).
                for key in ["modelPath", "codecPath"] {
                    if creds.get(key).map_or(true, String::is_empty) {
                        return Err(EngineSetupError::new(
                            TtsErrorCode::MissingConfiguration,
                            engine_id,
                            Some(key),
                            format!("omnivoice-local: {key} is required"),
                        ));
                    }
                }
                let params = OmniVoiceLocalParams {
                    model_path: creds.get("modelPath").cloned().unwrap_or_default(),
                    codec_path: creds.get("codecPath").cloned().unwrap_or_default(),
                    num_steps: creds
                        .get("numSteps")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(32),
                    seed: creds.get("seed").and_then(|s| s.parse().ok()).unwrap_or(42),
                    instruct: creds.get("instruct").cloned(),
                    language: creds.get("language").cloned().unwrap_or_default(),
                    ..Default::default()
                };
                return match OmniVoiceLocalEngine::new(params) {
                    Ok(engine) => Ok(Arc::new(engine)),
                    Err(e) => Err(fielded_setup_error(engine_id, &creds, e.0, "modelPath")),
                };
            }
            #[cfg(not(feature = "omnivoice-local"))]
            {
                return Err(EngineSetupError::not_available(
                    engine_id,
                    "not enabled in this build — rebuild with --features omnivoice-local",
                ));
            }
        }
        "floravox" => {
            #[cfg(feature = "floravox")]
            {
                return Ok(Arc::new(crate::floravox_engine::FloravoxEngine::new(
                    credentials_json,
                )));
            }
            #[cfg(not(feature = "floravox"))]
            {
                return Err(EngineSetupError::not_available(
                    engine_id,
                    "not enabled in this build — rebuild with --features floravox",
                ));
            }
        }
        _ => {}
    }

    // Cloud catch-all. If the cloud feature is on we delegate; otherwise the
    // engine id is unknown to this build.
    #[cfg(feature = "cloud")]
    {
        match cloud_engine::create_cloud_engine(engine_id, credentials_json) {
            Some(engine) => Ok(engine),
            None => Err(EngineSetupError::new(
                TtsErrorCode::EngineNotAvailable,
                engine_id,
                None,
                format!(
                    "Unknown engine '{engine_id}'. Available engines: {}",
                    engine_list()
                        .iter()
                        .map(|e| e.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }

    #[cfg(not(feature = "cloud"))]
    {
        Err(EngineSetupError::new(
            TtsErrorCode::EngineNotAvailable,
            engine_id,
            None,
            format!(
                "Unknown engine '{engine_id}' (cloud feature is disabled; only \
                 built-in engines are available). Available engines: {}",
                engine_list()
                    .iter()
                    .map(|e| e.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))
    }
}

/// Build a setup error for a local-engine load failure, attaching the
/// first missing required field when the credentials make the cause
/// unambiguous (GH #69).
fn fielded_setup_error(
    engine_id: &str,
    creds: &std::collections::HashMap<String, String>,
    raw_message: String,
    primary_field: &str,
) -> EngineSetupError {
    let missing = crate::engine_schema::first_missing_required_field(engine_id, creds);
    let field = missing.or_else(|| Some(primary_field.to_string()));
    let code = crate::error_codes::classify_error_message(&raw_message);
    EngineSetupError::new(code, engine_id, field.as_deref(), raw_message)
}

/// Return the number of registered engines.
#[must_use]
pub fn engine_count() -> usize {
    engine_list().len()
}

/// Return a list of all registered engine descriptors.
#[must_use]
#[allow(clippy::vec_init_then_push)]
#[allow(clippy::too_many_lines)]
pub fn engine_list() -> Vec<EngineDescriptor> {
    #[allow(unused_mut)]
    let mut engines = Vec::new();

    #[cfg(all(feature = "system", target_os = "linux"))]
    engines.push(EngineDescriptor {
        id: "system".into(),
        name: "System (Speech Dispatcher)".into(),
        needs_credentials: false,
        credential_keys_json: "[]".into(),
    });

    #[cfg(all(feature = "avsynth", target_os = "macos"))]
    engines.push(EngineDescriptor {
        id: "avsynth".into(),
        name: "macOS AVSpeechSynthesizer".into(),
        needs_credentials: false,
        credential_keys_json: "[]".into(),
    });

    #[cfg(all(feature = "sapi", target_os = "windows"))]
    engines.push(EngineDescriptor {
        id: "sapi".into(),
        name: "Windows SAPI".into(),
        needs_credentials: false,
        credential_keys_json: "[]".into(),
    });

    #[cfg(feature = "sherpaonnx")]
    engines.push(EngineDescriptor {
        id: "sherpaonnx".into(),
        name: "Sherpa-ONNX".into(),
        needs_credentials: false,
        credential_keys_json: r#"["modelPath","modelId","numThreads","numSteps","provider","referenceAudio","referenceText"]"#.into(),
    });

    #[cfg(feature = "qwen3-local")]
    engines.push(EngineDescriptor {
        id: "qwen3-local".into(),
        name: "Qwen3-TTS local (qwen3-tts.cpp)".into(),
        needs_credentials: true,
        credential_keys_json: r#"["modelsDir","threads","temperature","topK","maxTokens"]"#.into(),
    });

    #[cfg(feature = "floravox")]
    engines.push(EngineDescriptor {
        id: "floravox".into(),
        name: "floravox".into(),
        needs_credentials: true,
        credential_keys_json: r#"["modelsDir","modelId","misaki","chars","speaker"]"#.into(),
    });

    #[cfg(feature = "omnivoice-local")]
    engines.push(EngineDescriptor {
        id: "omnivoice-local".into(),
        name: "OmniVoice local (600+ languages, offline)".into(),
        needs_credentials: true,
        credential_keys_json:
            r#"["modelPath","codecPath","numSteps","seed","instruct","language"]"#.into(),
    });

    #[cfg(feature = "pocket-timing")]
    engines.push(EngineDescriptor {
        id: "pocket".into(),
        name: "Pocket TTS (attention-timed, phoneme-driven)".into(),
        needs_credentials: true,
        credential_keys_json: r#"["bundleDir","reference"]"#.into(),
    });

    #[cfg(feature = "cloud")]
    {
        let cloud = [
            ("openai", "OpenAI", true, r#"["apiKey"]"#),
            (
                "elevenlabs",
                "ElevenLabs",
                true,
                r#"["apiKey","modelId","voiceId","language"]"#,
            ),
            ("azure", "Azure", true, r#"["subscriptionKey","region"]"#),
            ("edge", "Microsoft Edge (Read Aloud)", false, "[]"),
            (
                "google",
                "Google Cloud",
                true,
                r#"["apiKey","keyFilePath"]"#,
            ),
            (
                "gemini",
                "Google Gemini (3.8 TTS)",
                true,
                r#"["apiKey","modelId","voice","style"]"#,
            ),
            (
                "qwen",
                "Qwen (Alibaba Cloud Model Studio)",
                true,
                r#"["apiKey","modelId","region","instruction","wsUrl","customizationUrl"]"#,
            ),
            (
                "polly",
                "Amazon Polly",
                true,
                r#"["accessKeyId","secretAccessKey","region"]"#,
            ),
            ("cartesia", "Cartesia", true, r#"["apiKey"]"#),
            ("deepgram", "Deepgram", true, r#"["apiKey"]"#),
            ("playht", "PlayHT", true, r#"["apiKey","userId"]"#),
            ("fishaudio", "Fish Audio", true, r#"["apiKey"]"#),
            ("hume", "Hume AI", true, r#"["apiKey"]"#),
            ("mistral", "Mistral", true, r#"["apiKey"]"#),
            ("murf", "Murf", true, r#"["apiKey"]"#),
            ("resemble", "Resemble AI", true, r#"["apiKey"]"#),
            ("unrealspeech", "Unreal Speech", true, r#"["apiKey"]"#),
            ("upliftai", "UpliftAI", true, r#"["apiKey"]"#),
            (
                "watson",
                "IBM Watson",
                true,
                r#"["apiKey","region","instanceId"]"#,
            ),
            ("witai", "Wit.ai", true, r#"["token"]"#),
            ("xai", "xAI", true, r#"["apiKey"]"#),
            ("modelslab", "ModelsLab", true, r#"["apiKey"]"#),
        ];
        for (id, name, creds, keys) in &cloud {
            engines.push(EngineDescriptor {
                id: (*id).into(),
                name: (*name).into(),
                needs_credentials: *creds,
                credential_keys_json: (*keys).into(),
            });
        }
    }

    engines
}

#[cfg(test)]
mod credentials_tests {
    use super::parse_credentials;

    #[test]
    fn json_numbers_coerce_to_strings() {
        let creds = parse_credentials(r#"{"modelPath":"/m.gguf","numSteps":16,"seed":42}"#);
        assert_eq!(creds.get("modelPath").map(String::as_str), Some("/m.gguf"));
        assert_eq!(creds.get("numSteps").map(String::as_str), Some("16"));
        assert_eq!(creds.get("seed").map(String::as_str), Some("42"));
    }

    #[test]
    fn json_bools_coerce() {
        let creds = parse_credentials(r#"{"verbose":true,"fast":false}"#);
        assert_eq!(creds.get("verbose").map(String::as_str), Some("true"));
        assert_eq!(creds.get("fast").map(String::as_str), Some("false"));
    }

    #[test]
    fn malformed_json_warns_not_silences() {
        let creds = parse_credentials(r#"{"broken": tru}"#);
        assert!(creds.is_empty());
    }

    #[test]
    fn empty_input_is_empty_map() {
        assert!(parse_credentials("").is_empty());
    }

    #[test]
    fn mixed_types_all_coerce() {
        let creds =
            parse_credentials(r#"{"path":"/a/b","count":3,"ratio":0.5,"flag":true,"name":"test"}"#);
        assert_eq!(creds.len(), 5);
        assert_eq!(creds.get("count").map(String::as_str), Some("3"));
        assert_eq!(creds.get("ratio").map(String::as_str), Some("0.5"));
    }
}
