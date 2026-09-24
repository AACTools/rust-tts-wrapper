use super::*;

/// Configuration for a single cloud TTS provider.
#[derive(Debug, Clone, Default)]
pub(crate) struct CloudConfig {
    pub(crate) synth_url: String,
    pub(crate) auth_header: String,
    pub(crate) auth_prefix: String,
    pub(crate) voice_param: String,
    pub(crate) model_param: Option<String>,
    pub(crate) model_default: Option<String>,
    pub(crate) default_voice: Option<String>,
    pub(crate) text_field: String,
    pub(crate) extra_body: HashMap<String, serde_json::Value>,
    /// Whether this engine requires SSML in the request body (Azure).
    pub(crate) body_is_ssml: bool,
    /// Content-Type header override for the synthesis request.
    pub(crate) content_type: Option<String>,
    /// Additional headers to send with synthesis requests.
    pub(crate) extra_headers: HashMap<String, String>,
    /// URL for the voice listing endpoint, if available.
    pub(crate) voices_url: Option<String>,
    /// Provider ID string for voice mapping.
    pub(crate) provider_id: String,
    /// Whether this engine's synthesis response body is already raw PCM16
    /// (delivered verbatim) rather than MP3 (decoded to PCM before delivery).
    /// Azure returns PCM because we request `raw-24khz-16bit-mono-pcm`;
    /// Cartesia returns raw PCM by design. Everything else returns MP3.
    pub(crate) response_is_pcm: bool,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn build_config(id: &str, creds: &HashMap<String, String>) -> Option<CloudConfig> {
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
            provider_id: "openai".into(),
            ..Default::default()
        }),
        "elevenlabs" => {
            let voice_id = creds
                .get("voiceId")
                .cloned()
                .unwrap_or_else(|| "21m00Tcm4TlvDq8ikWAM".into());
            // Model selection matters for the SpeechMarkdown dialect:
            // eleven_v3* parses no SSML (audio tags only), pre-v3 models
            // understand <break> but read audio tags aloud. v3 is the
            // default — the most capable model, and the dialects keep the
            // markup correct for it. Unrecognized model IDs surface as
            // API errors rather than being masked.
            let model = creds
                .get("modelId")
                .filter(|m| !m.is_empty())
                .cloned()
                .unwrap_or_else(|| "eleven_v3".into());
            Some(CloudConfig {
                synth_url: format!("https://api.elevenlabs.io/v1/text-to-speech/{voice_id}"),
                auth_header: "xi-api-key".into(),
                model_param: Some("model_id".into()),
                model_default: Some(model),
                text_field: "text".into(),
                voices_url: Some("https://api.elevenlabs.io/v1/voices".into()),
                provider_id: "elevenlabs".into(),
                ..Default::default()
            })
        }
        "azure" => {
            let region = creds
                .get("region")
                .cloned()
                .unwrap_or_else(|| "eastus".into());
            let mut extra = HashMap::new();
            extra.insert(
                "X-Microsoft-OutputFormat".into(),
                // Raw PCM16 24 kHz mono so the bytes flow straight to on_audio
                // without an MP3 decode step (SAPI wants PCM; matches the
                // SherpaOnnx / SAPI engines' PCM delivery contract).
                "raw-24khz-16bit-mono-pcm".into(),
            );
            extra.insert("User-Agent".into(), "rust-tts-wrapper".into());
            Some(CloudConfig {
                synth_url: format!(
                    "https://{region}.tts.speech.microsoft.com/cognitiveservices/v1"
                ),
                auth_header: "Ocp-Apim-Subscription-Key".into(),
                default_voice: Some("en-US-AriaNeural".into()),
                body_is_ssml: true,
                content_type: Some("application/ssml+xml".into()),
                extra_headers: extra,
                voices_url: Some(format!(
                    "https://{region}.tts.speech.microsoft.com/cognitiveservices/voices/list"
                )),
                provider_id: "azure".into(),
                // X-Microsoft-OutputFormat requests raw PCM (see above).
                response_is_pcm: true,
                ..Default::default()
            })
        }
        "google" => {
            let api_key = creds.get("apiKey").cloned().unwrap_or_default();
            Some(CloudConfig {
                synth_url: format!(
                    "https://texttospeech.googleapis.com/v1/text:synthesize?key={api_key}"
                ),
                text_field: "text".into(),
                voices_url: Some(format!(
                    "https://texttospeech.googleapis.com/v1/voices?key={api_key}"
                )),
                provider_id: "google".into(),
                ..Default::default()
            })
        }
        "gemini" => {
            // Gemini 3.8 TTS via the Interactions API. Model selection
            // matters: gemini-3.8-flash-tts (default) is the expressive
            // flagship; gemini-3.8-flash-lite-tts is the high-volume
            // variant. Both share the exact API schema. Older preview
            // models (gemini-2.5-*-tts, gemini-3.1-flash-tts-preview)
            // also work through this path. An unrecognized model ID
            // surfaces as an API error rather than being masked.
            let model = creds
                .get("modelId")
                .filter(|m| !m.is_empty())
                .cloned()
                .unwrap_or_else(|| "gemini-3.8-flash-tts".into());
            let voice = creds
                .get("voice")
                .filter(|v| !v.is_empty())
                .cloned()
                .unwrap_or_else(|| "Kore".into());
            Some(CloudConfig {
                synth_url: "https://generativelanguage.googleapis.com/v1beta/interactions".into(),
                auth_header: "x-goog-api-key".into(),
                auth_prefix: String::new(),
                model_default: Some(model),
                default_voice: Some(voice),
                voices_url: Some("https://generativelanguage.googleapis.com/v1beta/voices".into()),
                provider_id: "gemini".into(),
                ..Default::default()
            })
        }
        "cartesia" => Some(CloudConfig {
            synth_url: "https://api.cartesia.ai/tts/bytes".into(),
            auth_header: "X-API-Key".into(),
            voice_param: "voice_id".into(),
            model_param: Some("model_id".into()),
            model_default: Some("sonic-2".into()),
            text_field: "text".into(),
            voices_url: Some("https://api.cartesia.ai/voices".into()),
            provider_id: "cartesia".into(),
            // Cartesia's /tts/bytes endpoint returns raw PCM s16le @24 kHz.
            response_is_pcm: true,
            ..Default::default()
        }),
        "deepgram" => Some(CloudConfig {
            synth_url: "https://api.deepgram.com/v1/speak".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Token ".into(),
            voice_param: "model".into(), // Fixed: Deepgram uses "model" not "voice"
            default_voice: Some("aura-asteria-en".into()),
            text_field: "text".into(),
            voices_url: Some("https://api.deepgram.com/v1/voices".into()),
            provider_id: "deepgram".into(),
            ..Default::default()
        }),
        "playht" => {
            let user_id = creds.get("userId").cloned().unwrap_or_default();
            let mut extra_headers = HashMap::new();
            extra_headers.insert("X-User-ID".into(), user_id);
            Some(CloudConfig {
                synth_url: "https://api.play.ht/api/v2/tts".into(),
                auth_header: "Authorization".into(),
                auth_prefix: "Bearer ".into(),
                voice_param: "voice".into(),
                text_field: "text".into(),
                voices_url: Some("https://api.play.ht/api/v2/voices".into()),
                extra_headers,
                provider_id: "playht".into(),
                ..Default::default()
            })
        }
        "fishaudio" => Some(CloudConfig {
            synth_url: "https://api.fish.audio/v1/tts".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "reference_id".into(),
            text_field: "text".into(),
            voices_url: Some("https://api.fish.audio/v1/model".into()),
            provider_id: "fishaudio".into(),
            ..Default::default()
        }),
        "hume" => {
            // Hume API requires voice as object: {"voice": {"name": "..."}}
            let voice_name = creds.get("voice").cloned().unwrap_or_default();
            let mut extra_body = HashMap::new();
            extra_body.insert("voice".into(), serde_json::json!({"name": voice_name}));
            extra_body.insert(
                "audio_format".into(),
                serde_json::Value::String("wav".into()),
            );

            Some(CloudConfig {
                synth_url: "https://api.hume.ai/v0/tts".into(),
                auth_header: "Authorization".into(),
                auth_prefix: "Bearer ".into(),
                voice_param: String::new(), // Not used - voice in extra_body
                text_field: "text".into(),
                extra_body,
                provider_id: "hume".into(),
                ..Default::default()
            })
        }
        "mistral" => Some(CloudConfig {
            synth_url: "https://api.mistral.ai/v1/tts".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice".into(),
            text_field: "text".into(),
            provider_id: "mistral".into(),
            ..Default::default()
        }),
        "murf" => Some(CloudConfig {
            synth_url: "https://api.murf.ai/v1/speech/generate".into(),
            auth_header: "api-key".into(),
            voice_param: "voice_id".into(),
            text_field: "text".into(),
            provider_id: "murf".into(),
            ..Default::default()
        }),
        "resemble" => Some(CloudConfig {
            synth_url: "https://app.resemble.ai/api/v2/synthesize".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Token ".into(),
            voice_param: "voice_uuid".into(),
            text_field: "text".into(),
            voices_url: Some("https://app.resemble.ai/api/v2/voices".into()),
            provider_id: "resemble".into(),
            ..Default::default()
        }),
        "unrealspeech" => Some(CloudConfig {
            synth_url: "https://api.v7.unrealspeech.com/speech".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice_id".into(),
            default_voice: Some("Scarlett".into()),
            text_field: "text".into(),
            provider_id: "unrealspeech".into(),
            ..Default::default()
        }),
        "upliftai" => Some(CloudConfig {
            synth_url: "https://api.upliftai.org/v1/tts".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice".into(),
            text_field: "text".into(),
            provider_id: "upliftai".into(),
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
                    "Basic {}",
                    // IBM Watson's Basic-auth scheme requires the literal
                    // string "apikey" (lowercase, one word) as the username.
                    // Using "apiKey" (camelCase) here returns HTTP 401 from
                    // every Watson endpoint.
                    base64_encode(&format!("apikey:{}", creds.get("apiKey").cloned().unwrap_or_default()))
                ),
                voice_param: "voice".into(),
                text_field: "text".into(),
                voices_url: Some(format!(
                    "https://{region}.text-to-speech.watson.cloud.ibm.com/instances/{instance_id}/v1/voices"
                )),
                provider_id: "watson".into(),
                ..Default::default()
            })
        }
        "witai" => Some(CloudConfig {
            synth_url: "https://api.wit.ai/synthesize?v=20240304".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voices_url: Some("https://api.wit.ai/voices?v=20240304".into()),
            provider_id: "witai".into(),
            ..Default::default()
        }),
        "xai" => Some(CloudConfig {
            synth_url: "https://api.x.ai/v1/audio/speech".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            voice_param: "voice".into(),
            text_field: "input".into(),
            provider_id: "xai".into(),
            ..Default::default()
        }),
        "modelslab" => Some(CloudConfig {
            synth_url: "https://modelslab.com/api/v1/text_to_speech".into(),
            voice_param: "voice".into(),
            text_field: "text".into(),
            provider_id: "modelslab".into(),
            ..Default::default()
        }),
        "polly" => {
            // Polly requires AWS Signature V4 - not implemented yet
            // Returning None indicates unsupported engine
            eprintln!("WARNING: AWS Polly requires AWS Signature V4 authentication which is not implemented. Use a different cloud provider.");
            None
        }
        // Microsoft Edge "Read Aloud" — the free, no-subscription Windows
        // neural voices. WS-only (no REST synth endpoint); the URL + Sec-MS-GEC
        // auth are built at speak time in the WS branch below. Voice list shape
        // is identical to Azure's (`ShortName`/`Gender`/`Locale`/…). Edge
        // returns MP3 frames (raw PCM isn't supported on this endpoint), so
        // `response_is_pcm = false` and the WS loop decodes before delivery.
        "edge" => Some(CloudConfig {
            default_voice: Some(EDGE_DEFAULT_VOICE.into()),
            voices_url: Some(EDGE_VOICE_LIST_URL.into()),
            provider_id: "edge".into(),
            response_is_pcm: false,
            ..Default::default()
        }),
        _ => None,
    }
}

/// The model that will actually be sent: an `extra_body["model_id"]`
/// override wins over `model_default` (the JSON-body insert order gives
/// extra_body the last write).
pub(crate) fn effective_model(config: &CloudConfig) -> Option<&str> {
    config
        .extra_body
        .get("model_id")
        .and_then(|v| v.as_str())
        .or(config.model_default.as_deref())
}
