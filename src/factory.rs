//! Engine factory: create engines by ID and list all registered engines.

use crate::engine::TtsEngine;
use crate::types::EngineDescriptor;

// The unused-import warning is a false positive — TtsEngine is a trait used as a dyn bound.
#[cfg(feature = "cloud")]
use crate::cloud_engine;
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
    match engine_id {
        #[cfg(feature = "system")]
        "system" => Some(Box::new(SystemEngine::new())),

        #[cfg(feature = "sherpaonnx")]
        "sherpaonnx" => Some(Box::new(SherpaOnnxEngine::new(credentials_json))),

        #[cfg(feature = "cloud")]
        id => cloud_engine::create_cloud_engine(id, credentials_json),

        #[cfg(not(feature = "cloud"))]
        _ => None,
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
        name: "System".into(),
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
