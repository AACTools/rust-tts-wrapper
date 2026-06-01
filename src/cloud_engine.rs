//! Generic cloud TTS engine supporting 19 providers via HTTP APIs.

use crate::engine::TtsEngine;
use crate::types::{TtsError, TtsResult, Voice};
use std::collections::HashMap;

/// Configuration for a single cloud TTS provider.
#[derive(Debug, Clone, Default)]
struct CloudConfig {
    synth_url: String,
    auth_header: String,
    auth_prefix: String,
    voice_param: String,
    model_param: Option<String>,
    model_default: Option<String>,
    default_voice: Option<String>,
    text_field: String,
    extra_body: HashMap<String, serde_json::Value>,
}

/// A TTS engine that synthesises speech by calling a cloud HTTP API.
#[derive(Debug)]
pub struct CloudEngine {
    config: CloudConfig,
    api_key: String,
    client: reqwest::blocking::Client,
}

impl CloudEngine {
    /// Create a cloud engine for the given provider `id`.
    ///
    /// Returns `None` if `id` is not a recognised cloud provider.
    pub fn new(id: &str, credentials: &HashMap<String, String>) -> Option<Self> {
        let config = build_config(id, credentials)?;
        let api_key = credentials
            .get("apiKey")
            .or_else(|| credentials.get("subscriptionKey"))
            .or_else(|| credentials.get("token"))
            .cloned()
            .unwrap_or_default();
        Some(CloudEngine {
            config,
            api_key,
            client: reqwest::blocking::Client::new(),
        })
    }
}

#[allow(clippy::too_many_lines)]
fn build_config(id: &str, creds: &HashMap<String, String>) -> Option<CloudConfig> {
    match id {
        "openai" => Some(CloudConfig {
            synth_url: "https://api.openai.com/v1/audio/speech".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice".into(),
            model_param: Some("model".into()),
            model_default: Some("gpt-4o-mini-tts".into()),
            default_voice: Some("alloy".into()),
            text_field: "input".into(),
            ..Default::default()
        }),
        "elevenlabs" => {
            let voice_id = creds
                .get("voiceId")
                .cloned()
                .unwrap_or_else(|| "21m00Tcm4TlvDq8ikWAM".into());
            Some(CloudConfig {
                synth_url: format!("https://api.elevenlabs.io/v1/text-to-speech/{voice_id}"),
                auth_header: "xi-api-key".into(),
                voice_param: String::new(),
                model_param: Some("model_id".into()),
                model_default: Some("eleven_multilingual_v2".into()),
                text_field: "text".into(),
                ..Default::default()
            })
        }
        "azure" => {
            let region = creds
                .get("region")
                .cloned()
                .unwrap_or_else(|| "eastus".into());
            Some(CloudConfig {
                synth_url: format!(
                    "https://{region}.tts.speech.microsoft.com/cognitiveservices/v1"
                ),
                auth_header: "Ocp-Apim-Subscription-Key".into(),
                voice_param: String::new(),
                text_field: String::new(),
                ..Default::default()
            })
        }
        "google" => Some(CloudConfig {
            synth_url: "https://texttospeech.googleapis.com/v1/text:synthesize".into(),
            voice_param: String::new(),
            text_field: String::new(),
            ..Default::default()
        }),
        "cartesia" => Some(CloudConfig {
            synth_url: "https://api.cartesia.ai/tts/bytes".into(),
            auth_header: "X-API-Key".into(),
            voice_param: "voice_id".into(),
            model_param: Some("model_id".into()),
            model_default: Some("sonic-2".into()),
            default_voice: None,
            text_field: "text".into(),
            ..Default::default()
        }),
        "deepgram" => Some(CloudConfig {
            synth_url: "https://api.deepgram.com/v1/speak".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Token ".into(),
            voice_param: "voice".into(),
            default_voice: Some("aura-asteria-en".into()),
            text_field: "text".into(),
            ..Default::default()
        }),
        "playht" => {
            let user_id = creds.get("userId").cloned().unwrap_or_default();
            let mut extra = HashMap::new();
            extra.insert("user_id".into(), serde_json::Value::String(user_id));
            Some(CloudConfig {
                synth_url: "https://api.play.ht/api/v2/tts".into(),
                auth_header: "Authorization".into(),
                auth_prefix: "Bearer ".into(),
                voice_param: "voice".into(),
                text_field: "text".into(),
                extra_body: extra,
                ..Default::default()
            })
        }
        "fishaudio" => Some(CloudConfig {
            synth_url: "https://api.fish.audio/v1/tts".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "reference_id".into(),
            text_field: "text".into(),
            ..Default::default()
        }),
        "hume" => Some(CloudConfig {
            synth_url: "https://api.hume.ai/v0/tts".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice".into(),
            text_field: "text".into(),
            ..Default::default()
        }),
        "mistral" => Some(CloudConfig {
            synth_url: "https://api.mistral.ai/v1/tts".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice".into(),
            text_field: "text".into(),
            ..Default::default()
        }),
        "murf" => Some(CloudConfig {
            synth_url: "https://api.murf.ai/v1/speech/generate".into(),
            auth_header: "api-key".into(),
            voice_param: "voice_id".into(),
            text_field: "text".into(),
            ..Default::default()
        }),
        "resemble" => Some(CloudConfig {
            synth_url: "https://app.resemble.ai/api/v2/synthesize".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Token ".into(),
            voice_param: "voice_uuid".into(),
            text_field: "text".into(),
            ..Default::default()
        }),
        "unrealspeech" => Some(CloudConfig {
            synth_url: "https://api.v7.unrealspeech.com/speech".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice_id".into(),
            default_voice: Some("Scarlett".into()),
            text_field: "text".into(),
            ..Default::default()
        }),
        "upliftai" => Some(CloudConfig {
            synth_url: "https://api.upliftai.org/v1/tts".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice".into(),
            text_field: "text".into(),
            ..Default::default()
        }),
        "watson" => {
            let region = creds
                .get("region")
                .cloned()
                .unwrap_or_else(|| "us-east".into());
            let instance_id = creds.get("instanceId").cloned().unwrap_or_default();
            Some(CloudConfig {
                synth_url: format!(
                    "https://{region}.text-to-speech.watson.cloud.ibm.com/instances/{instance_id}/v1/synthesize"
                ),
                auth_header: "Authorization".into(),
                auth_prefix: format!(
                    "Basic {}:",
                    base64_encode("apikey", &creds.get("apiKey").cloned().unwrap_or_default())
                ),
                voice_param: "voice".into(),
                text_field: "text".into(),
                ..Default::default()
            })
        }
        "witai" => Some(CloudConfig {
            synth_url: "https://api.wit.ai/synthesize?v=20240304".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            text_field: String::new(),
            ..Default::default()
        }),
        "xai" => Some(CloudConfig {
            synth_url: "https://api.x.ai/v1/audio/speech".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice".into(),
            text_field: "input".into(),
            ..Default::default()
        }),
        "modelslab" => Some(CloudConfig {
            synth_url: "https://modelslab.com/api/v1/text_to_speech".into(),
            voice_param: "voice".into(),
            text_field: "text".into(),
            ..Default::default()
        }),
        "polly" => Some(CloudConfig {
            synth_url: "https://polly.us-east-1.amazonaws.com/v1/speech".into(),
            voice_param: "VoiceId".into(),
            default_voice: Some("Joanna".into()),
            text_field: "Text".into(),
            ..Default::default()
        }),
        _ => None,
    }
}

/// Hex-encode bytes for basic-auth style tokens.
fn base64_encode(_prefix: &str, data: &str) -> String {
    use std::fmt::Write;
    let mut result = String::new();
    for byte in data.as_bytes() {
        write!(result, "{byte:02x}").unwrap();
    }
    result
}

impl TtsEngine for CloudEngine {
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        _rate: f32,
        _pitch: f32,
        _volume: f32,
    ) -> TtsResult<()> {
        let voice_to_use = voice
            .map(std::string::ToString::to_string)
            .or_else(|| self.config.default_voice.clone())
            .unwrap_or_default();

        let mut body = serde_json::Map::new();
        body.insert(
            self.config.text_field.clone(),
            serde_json::Value::String(text.to_string()),
        );

        if !self.config.voice_param.is_empty() && !voice_to_use.is_empty() {
            body.insert(
                self.config.voice_param.clone(),
                serde_json::Value::String(voice_to_use),
            );
        }
        if let Some(ref model_param) = self.config.model_param {
            if let Some(ref model) = self.config.model_default {
                body.insert(
                    model_param.clone(),
                    serde_json::Value::String(model.clone()),
                );
            }
        }
        for (k, v) in &self.config.extra_body {
            body.insert(k.clone(), v.clone());
        }

        let mut req = self.client.post(&self.config.synth_url).json(&body);

        if !self.config.auth_header.is_empty() {
            let val = format!("{}{}", self.config.auth_prefix, self.api_key);
            req = req.header(&self.config.auth_header, val);
        }

        let resp = req
            .send()
            .map_err(|e| TtsError(format!("HTTP error: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().unwrap_or_default();
            return Err(TtsError(format!("API error {status}: {body_text}")));
        }

        let _audio_bytes = resp
            .bytes()
            .map_err(|e| TtsError(format!("Read error: {e}")))?;
        Ok(())
    }

    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<()> {
        self.speak(text, voice, rate, pitch, volume)
    }

    fn stop(&self) -> TtsResult<()> {
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        Ok(vec![])
    }

    fn engine_id(&self) -> &'static str {
        "cloud"
    }
}

/// Create a cloud engine from a JSON credentials string.
pub fn create_cloud_engine(id: &str, credentials_json: &str) -> Option<Box<dyn TtsEngine>> {
    let creds: HashMap<String, String> = if credentials_json.is_empty() {
        HashMap::new()
    } else {
        serde_json::from_str(credentials_json).unwrap_or_default()
    };
    CloudEngine::new(id, &creds).map(|e| Box::new(e) as Box<dyn TtsEngine>)
}
