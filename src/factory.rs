//! Engine factory: create engines by ID and list all registered engines.

use crate::engine::TtsEngine;
use crate::types::EngineDescriptor;

// The unused-import warning is a false positive — TtsEngine is a trait used as a dyn bound.
#[cfg(all(feature = "avsynth", target_os = "macos"))]
use crate::avsynth_engine::AvSynthEngine;
#[cfg(feature = "cloud")]
use crate::cloud_engine;
#[cfg(all(feature = "sapi", target_os = "windows"))]
use crate::sapi_engine::SapiEngine;
#[cfg(feature = "sherpaonnx")]
use crate::sherpaonnx_engine::SherpaOnnxEngine;
#[cfg(feature = "system")]
use crate::system_engine::SystemEngine;

/// Create an engine by its string identifier.
///
/// `credentials_json` is a JSON object with engine-specific credentials
/// (e.g. `{"apiKey": "..."}`). Pass `""` for engines that don't need credentials.
#[must_use]
#[allow(unused_variables)]
pub fn create_engine(engine_id: &str, credentials_json: &str) -> Option<Box<dyn TtsEngine>> {
    // Detect engines that exist in the full catalogue but were compiled out
    // by disabled features, so callers get a useful error rather than a
    // generic "unknown engine" (§9 H1). Only emit these messages when the
    // caller actually asked for one of the gated engines.
    match engine_id {
        "system" => {
            #[cfg(feature = "system")]
            {
                return Some(Box::new(SystemEngine::new()));
            }
            #[cfg(not(feature = "system"))]
            {
                eprintln!(
                    "Engine 'system' is not enabled in this build. Rebuild with --features system."
                );
                return None;
            }
        }
        "avsynth" => {
            #[cfg(all(feature = "avsynth", target_os = "macos"))]
            {
                return Some(Box::new(AvSynthEngine::new()));
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
                return Some(Box::new(SapiEngine::new()));
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
                return Some(Box::new(SherpaOnnxEngine::new(credentials_json)));
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
        return result;
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
pub fn engine_list() -> Vec<EngineDescriptor> {
    let mut engines = Vec::new();

    #[cfg(feature = "system")]
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

    #[cfg(feature = "cloud")]
    {
        let cloud = [
            ("openai", "OpenAI", true, r#"["apiKey"]"#),
            ("elevenlabs", "ElevenLabs", true, r#"["apiKey"]"#),
            ("azure", "Azure", true, r#"["subscriptionKey","region"]"#),
            ("google", "Google Cloud", true, r#"["apiKey"]"#),
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
