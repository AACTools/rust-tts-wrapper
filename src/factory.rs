//! Engine factory: create engines by ID and list all registered engines.

use crate::engine::TtsEngine;
use crate::types::EngineDescriptor;
use std::sync::Arc;

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
#[must_use]
#[allow(unused_variables)]
#[allow(clippy::too_many_lines)]
pub fn create_engine(engine_id: &str, credentials_json: &str) -> Option<Arc<dyn TtsEngine>> {
    // Detect engines that exist in the full catalogue but were compiled out
    // by disabled features, so callers get a useful error rather than a
    // generic "unknown engine". Only emit these messages when the
    // caller actually asked for one of the gated engines.
    match engine_id {
        "system" => {
            #[cfg(all(feature = "system", target_os = "linux"))]
            {
                return Some(Arc::new(SystemEngine::new()));
            }
            #[cfg(not(all(feature = "system", target_os = "linux")))]
            {
                eprintln!(
                    "Engine 'system' requires the 'system' feature on Linux. \
                     Current build does not satisfy these conditions."
                );
                return None;
            }
        }
        "avsynth" => {
            #[cfg(all(feature = "avsynth", target_os = "macos"))]
            {
                return Some(Arc::new(AvSynthEngine::new()));
            }
            #[cfg(not(all(feature = "avsynth", target_os = "macos")))]
            {
                eprintln!(
                    "Engine 'avsynth' requires the 'avsynth' feature and macOS. \
                     Current build does not satisfy these conditions."
                );
                return None;
            }
        }
        "sapi" => {
            #[cfg(all(feature = "sapi", target_os = "windows"))]
            {
                return Some(Arc::new(SapiEngine::new()));
            }
            #[cfg(not(all(feature = "sapi", target_os = "windows")))]
            {
                eprintln!(
                    "Engine 'sapi' requires the 'sapi' feature and Windows. \
                     Current build does not satisfy these conditions."
                );
                return None;
            }
        }
        "sherpaonnx" => {
            #[cfg(feature = "sherpaonnx")]
            {
                return Some(Arc::new(SherpaOnnxEngine::new(credentials_json)));
            }
            #[cfg(not(feature = "sherpaonnx"))]
            {
                eprintln!(
                    "Engine 'sherpaonnx' is not enabled in this build. \
                     Rebuild with --features sherpaonnx."
                );
                return None;
            }
        }
        "qwen3-local" => {
            #[cfg(feature = "qwen3-local")]
            {
                let creds: std::collections::HashMap<String, String> =
                    if credentials_json.is_empty() {
                        std::collections::HashMap::new()
                    } else {
                        serde_json::from_str(credentials_json).unwrap_or_default()
                    };
                return match Qwen3LocalEngine::new(&creds) {
                    Ok(engine) => Some(Arc::new(engine)),
                    Err(e) => {
                        eprintln!("Engine 'qwen3-local' failed to load: {e}");
                        None
                    }
                };
            }
            #[cfg(not(feature = "qwen3-local"))]
            {
                eprintln!(
                    "Engine 'qwen3-local' is not enabled in this build. \
                     Rebuild with --features qwen3-local."
                );
                return None;
            }
        }
        "floravox" => {
            #[cfg(feature = "floravox")]
            {
                return Some(Arc::new(crate::floravox_engine::FloravoxEngine::new(
                    credentials_json,
                )));
            }
            #[cfg(not(feature = "floravox"))]
            {
                eprintln!(
                    "Engine 'floravox' is not enabled in this build. \
                     Rebuild with --features floravox."
                );
                return None;
            }
        }
        _ => {}
    }

    // Cloud catch-all. If the cloud feature is on we delegate; otherwise the
    // engine id is unknown to this build.
    #[cfg(feature = "cloud")]
    {
        let result = cloud_engine::create_cloud_engine(engine_id, credentials_json);
        if result.is_none() {
            eprintln!(
                "Unknown engine '{engine_id}'. Available engines: {}",
                engine_list()
                    .iter()
                    .map(|e| e.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        result
    }

    #[cfg(not(feature = "cloud"))]
    {
        eprintln!(
            "Unknown engine '{engine_id}' (cloud feature is disabled; only \
             built-in engines are available). Available engines: {}",
            engine_list()
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        None
    }
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
        credential_keys_json: "[]".into(),
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
            ("google", "Google Cloud", true, r#"["apiKey"]"#),
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
