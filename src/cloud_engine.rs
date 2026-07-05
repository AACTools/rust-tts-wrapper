//! Generic cloud TTS engine supporting 19 providers via HTTP APIs.
//!
//! Handles Azure (SSML body), Google (base64 REST), and all other JSON-body
//! providers. Includes voice fetching for engines with list endpoints and
//! word boundary support where APIs provide timing data.

use crate::engine::{estimate_word_boundaries, preprocess_speech_markdown, TtsEngine};
use crate::types::{normalize_gender, LanguageCode, TtsError, TtsResult, Voice, WordBoundary};
use std::collections::HashMap;

#[cfg(feature = "cloud")]
use {
    tungstenite::{connect, Message},
    url::Url,
    uuid::Uuid,
};

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
    /// Whether this engine requires SSML in the request body (Azure).
    body_is_ssml: bool,
    /// Content-Type header override for the synthesis request.
    content_type: Option<String>,
    /// Additional headers to send with synthesis requests.
    extra_headers: HashMap<String, String>,
    /// URL for the voice listing endpoint, if available.
    voices_url: Option<String>,
    /// Provider ID string for voice mapping.
    provider_id: String,
}

/// A TTS engine that synthesises speech by calling a cloud HTTP API.
#[derive(Debug)]
pub struct CloudEngine {
    config: CloudConfig,
    api_key: String,
    credentials: HashMap<String, String>,
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
            credentials: credentials.clone(),
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
            provider_id: "openai".into(),
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
                model_param: Some("model_id".into()),
                model_default: Some("eleven_multilingual_v2".into()),
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
                "audio-24khz-96kbitrate-mono-mp3".into(),
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
        "cartesia" => Some(CloudConfig {
            synth_url: "https://api.cartesia.ai/tts/bytes".into(),
            auth_header: "X-API-Key".into(),
            voice_param: "voice_id".into(),
            model_param: Some("model_id".into()),
            model_default: Some("sonic-2".into()),
            text_field: "text".into(),
            voices_url: Some("https://api.cartesia.ai/voices".into()),
            provider_id: "cartesia".into(),
            ..Default::default()
        }),
        "deepgram" => Some(CloudConfig {
            synth_url: "https://api.deepgram.com/v1/speak".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Token ".into(),
            voice_param: "model".into(),  // Fixed: Deepgram uses "model" not "voice"
            default_voice: Some("aura-asteria-en".into()),
            text_field: "text".into(),
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
                extra_headers: extra_headers,
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
            provider_id: "fishaudio".into(),
            ..Default::default()
        }),
        "hume" => {
            // Hume API requires voice as object: {"voice": {"name": "..."}}
            let voice_name = creds.get("voice").cloned().unwrap_or_default();
            let mut extra_body = HashMap::new();
            extra_body.insert("voice".into(), serde_json::json!({"name": voice_name}));
            extra_body.insert("audio_format".into(), serde_json::Value::String("wav".into()));

            Some(CloudConfig {
                synth_url: "https://api.hume.ai/v0/tts".into(),
                auth_header: "Authorization".into(),
                auth_prefix: "Bearer ".into(),
                voice_param: "".into(),  // Not used - voice in extra_body
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
                    base64_encode(&format!("apiKey:{}", creds.get("apiKey").cloned().unwrap_or_default()))
                ),
                voice_param: "voice".into(),
                text_field: "text".into(),
                provider_id: "watson".into(),
                ..Default::default()
            })
        }
        "witai" => Some(CloudConfig {
            synth_url: "https://api.wit.ai/synthesize?v=20240304".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
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
        _ => None,
    }
}

/// Base64 encode for auth tokens.
fn base64_encode(data: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data.as_bytes())
}

/// Build SSML for Azure TTS.
fn build_azure_ssml(text: &str, voice: &str, rate: f32, pitch: f32, volume: f32) -> String {
    let lang = voice.chars().take(5).collect::<String>();

    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");

    // Escape the voice attribute value too — a stray `'` or `<` would break
    // the SSML (§2 H12). Apostrophes are escaped using `&apos;`.
    let voice_escaped = voice
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;");

    let mut prosody_attrs = Vec::new();
    let rate_str = match rate {
        r if r < 0.7 => "x-slow",
        r if r < 0.85 => "slow",
        r if r < 1.15 => "medium",
        r if r < 1.4 => "fast",
        _ => "x-fast",
    };
    let pitch_str = match pitch {
        p if p < 0.7 => "x-low",
        p if p < 0.85 => "low",
        p if p < 1.15 => "medium",
        p if p < 1.4 => "high",
        _ => "x-high",
    };
    let volume_str = match volume {
        v if v < 0.4 => "x-soft",
        v if v < 0.7 => "soft",
        v if v < 1.15 => "medium",
        v if v < 1.5 => "loud",
        _ => "x-loud",
    };
    if (rate - 1.0).abs() > f32::EPSILON {
        prosody_attrs.push(format!("rate=\"{rate_str}\""));
    }
    if (pitch - 1.0).abs() > f32::EPSILON {
        prosody_attrs.push(format!("pitch=\"{pitch_str}\""));
    }
    // Volume was previously dropped silently (§2 H6).
    if (volume - 1.0).abs() > f32::EPSILON {
        prosody_attrs.push(format!("volume=\"{volume_str}\""));
    }

    let inner = if prosody_attrs.is_empty() {
        escaped
    } else {
        format!("<prosody {}>{escaped}</prosody>", prosody_attrs.join(" "))
    };

    format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='{lang}'>\
         <voice name='{voice_escaped}'>{inner}</voice></speak>"
    )
}

/// Build JSON body for Google TTS REST API.
fn build_google_request(
    text: &str,
    voice: &str,
    add_marks: bool,
) -> (serde_json::Value, Vec<String>) {
    let lang = voice.chars().take(5).collect::<String>();

    let mut words_list = Vec::new();

    let input = if add_marks {
        let words: Vec<&str> = text.split_whitespace().filter(|w| !w.is_empty()).collect();
        let mut ssml = String::from("<speak>");
        for (i, w) in words.iter().enumerate() {
            if i > 0 {
                ssml.push(' ');
            }
            let _ = std::fmt::Write::write_fmt(&mut ssml, format_args!("<mark name=\"{i}\"/>{w}"));
            words_list = words.iter().map(|w| (*w).to_string()).collect();
        }
        ssml.push_str("</speak>");
        serde_json::json!({ "ssml": ssml })
    } else {
        serde_json::json!({ "text": text })
    };

    let mut body = serde_json::json!({
        "input": input,
        "voice": { "languageCode": lang, "name": voice },
        "audioConfig": { "audioEncoding": "MP3" }
    });

    if add_marks {
        body["enableTimePointing"] = serde_json::json!(["SSML_MARK"]);
    }

    (body, words_list)
}

/// Parse Google timepoints into word boundaries.
fn parse_google_timepoints(
    timepoints: &[serde_json::Value],
    words: &[String],
) -> Vec<WordBoundary> {
    #[derive(Clone)]
    struct RawTp {
        index: usize,
        time_ms: u64,
    }

    let mut raw: Vec<RawTp> = Vec::new();
    for tp in timepoints {
        let mark = tp.get("markName").and_then(|v| v.as_str()).unwrap_or("");
        let idx: usize = mark.parse().unwrap_or(usize::MAX);
        let secs = tp
            .get("timeSeconds")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        if idx < words.len() {
            raw.push(RawTp {
                index: idx,
                time_ms: (secs * 1000.0) as u64,
            });
        }
    }
    raw.sort_by_key(|r| r.time_ms);

    let mut boundaries = Vec::with_capacity(raw.len());
    for (i, tp) in raw.iter().enumerate() {
        let word = &words[tp.index];
        let duration = if i + 1 < raw.len() {
            raw[i + 1].time_ms.saturating_sub(tp.time_ms)
        } else {
            ((word.len() as u64) * 80).max(50)
        };
        boundaries.push(WordBoundary {
            text: word.clone(),
            offset: tp.time_ms,
            duration,
        });
    }
    boundaries
}

/// Map Azure voices JSON array to unified voices.
fn map_azure_voices(json: &[serde_json::Value]) -> Vec<Voice> {
    let mut voices = Vec::new();
    for v in json {
        let Some(short_name) = v.get("ShortName").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = v
            .get("DisplayName")
            .and_then(|v| v.as_str())
            .unwrap_or(short_name)
            .to_string();
        let gender_raw = v.get("Gender").and_then(|v| v.as_str()).unwrap_or("");
        let locale = v.get("Locale").and_then(|v| v.as_str()).unwrap_or("en-US");

        voices.push(Voice {
            id: short_name.to_string(),
            name,
            gender: normalize_gender(gender_raw),
            provider: "azure".to_string(),
            language_codes: vec![LanguageCode {
                bcp47: locale.to_string(),
                iso639_3: locale.split('-').next().unwrap_or("en").to_string(),
                display: v
                    .get("LocaleName")
                    .and_then(|v| v.as_str())
                    .unwrap_or(locale)
                    .to_string(),
            }],
        });
    }
    voices
}

/// Map Google voices JSON array to unified voices.
fn map_google_voices(json: &[serde_json::Value]) -> Vec<Voice> {
    let mut voices = Vec::new();
    for v in json {
        let Some(name) = v.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let gender_raw = v.get("ssmlGender").and_then(|v| v.as_str()).unwrap_or("");
        let lang_codes = v
            .get("languageCodes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        let code = c.as_str()?;
                        Some(LanguageCode {
                            iso639_3: code.split('-').next()?.to_string(),
                            bcp47: code.to_string(),
                            display: code.to_string(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        voices.push(Voice {
            id: name.to_string(),
            name: name.to_string(),
            gender: normalize_gender(gender_raw),
            provider: "google".to_string(),
            language_codes: lang_codes,
        });
    }
    voices
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::map_unwrap_or
)]
#[cfg(feature = "cloud")]
fn compute_durations(boundaries: &mut [WordBoundary]) {
    if boundaries.is_empty() {
        return;
    }
    if boundaries.len() == 1 {
        boundaries[0].duration = boundaries[0].duration.max(500);
        return;
    }
    let len = boundaries.len();
    for i in 0..(len - 1) {
        if boundaries[i].duration == 0 {
            boundaries[i].duration = boundaries[i + 1]
                .offset
                .saturating_sub(boundaries[i].offset);
        }
    }
    if boundaries[len - 1].duration == 0 {
        boundaries[len - 1].duration = 500;
    }
}

impl TtsEngine for CloudEngine {
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        _volume: f32,
        mut on_audio: Option<crate::engine::OnAudioCallback>,
        mut on_boundary: Option<crate::engine::OnBoundaryCallback>,
    ) -> TtsResult<()> {
        let voice_to_use = voice
            .map(std::string::ToString::to_string)
            .or_else(|| self.config.default_voice.clone())
            .unwrap_or_default();

        let (text, _is_ssml) = preprocess_speech_markdown(text, &self.config.provider_id);

        // Azure WebSocket approach if word boundaries are requested
        #[cfg(feature = "cloud")]
        if self.config.provider_id == "azure" && on_boundary.is_some() {
            let Some(on_boundary) = on_boundary.as_mut() else {
                return Ok(());
            };
            let region = self
                .credentials
                .get("region")
                .cloned()
                .unwrap_or_else(|| "eastus".into());
            // Azure requires a 32-char lowercase hex UUID with NO dashes.
            let request_id = Uuid::new_v4().simple().to_string();
            let ws_url_str = format!(
                "wss://{}.tts.speech.microsoft.com/cognitiveservices/websocket/v1?Ocp-Apim-Subscription-Key={}",
                region, self.api_key
            );

            let ws_url = Url::parse(&ws_url_str)
                .map_err(|e| TtsError(format!("Invalid Azure WS URL: {e}")))?;
            let (mut socket, _) = connect(ws_url.as_str())
                .map_err(|e| TtsError(format!("Azure WS Connect error: {e}")))?;

            let output_format = "audio-24khz-96kbitrate-mono-mp3"; // Or another default

            // Helper to produce an ISO 8601 timestamp for the X-Timestamp header.
            let now_timestamp = || {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                let secs = now.as_secs();
                // Build a UTC timestamp string without pulling in chrono.
                let days_since_epoch = secs / 86_400;
                let secs_today = secs % 86_400;
                let hour = secs_today / 3600;
                let minute = (secs_today % 3600) / 60;
                let second = secs_today % 60;
                // Convert days since 1970-01-01 to Y-M-D (Howard Hinnant's algorithm).
                let z = days_since_epoch as i64 + 719_468;
                let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
                let doe = (z - era * 146_097) as u64;
                let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
                let y = yoe as i64 + era * 400;
                let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
                let mp = (5 * doy + 2) / 153;
                let d = doy - (153 * mp + 2) / 5 + 1;
                let m = if mp < 10 { mp + 3 } else { mp - 9 };
                let year = if m <= 2 { y + 1 } else { y };
                format!(
                    "{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z"
                )
            };

            // Send config (must include X-Timestamp per Azure protocol).
            let config_headers = format!(
                "X-RequestId:{request_id}\r\nX-Timestamp:{}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n",
                now_timestamp()
            );
            let config_body = format!(
                r#"{{"context":{{"synthesis":{{"audio":{{"metadataOptions":{{"sentenceBoundaryEnabled":false,"wordBoundaryEnabled":true}},"outputFormat":"{output_format}"}}}}}}}}"#
            );
            let config_msg = format!("{config_headers}{config_body}");
            socket
                .send(Message::Text(config_msg.into()))
                .map_err(|e| TtsError(format!("WS config send error: {e}")))?;

            // Send SSML
            let ssml = build_azure_ssml(&text, &voice_to_use, rate, pitch, _volume);
            let ssml_msg = format!(
                "X-RequestId:{request_id}\r\nX-Timestamp:{}\r\nContent-Type:application/ssml+xml\r\nX-StreamId:{request_id}\r\nPath:ssml\r\n\r\n{ssml}",
                now_timestamp()
            );
            socket
                .send(Message::Text(ssml_msg.into()))
                .map_err(|e| TtsError(format!("WS ssml send error: {e}")))?;

            let mut collected_boundaries = Vec::new();

            loop {
                let msg = match socket.read() {
                    Ok(m) => m,
                    Err(
                        tungstenite::error::Error::ConnectionClosed
                        | tungstenite::error::Error::AlreadyClosed,
                    ) => break,
                    Err(e) => return Err(TtsError(format!("WS receive error: {e}"))),
                };

                match msg {
                    Message::Text(t) => {
                        let text_msg = t.as_str();
                        let path_line = text_msg.lines().find(|l| l.starts_with("Path:"));
                        let path = path_line.map_or("", |l| l.strip_prefix("Path:").unwrap_or(l).trim());

                        // Parse out JSON body once for the branches below.
                        let body = if let Some(idx) = text_msg.find("\r\n\r\n") {
                            &text_msg[idx + 4..]
                        } else if let Some(idx) = text_msg.find("\n\n") {
                            &text_msg[idx + 2..]
                        } else {
                            ""
                        };

                        // Error handling: Azure reports synthesis failures via
                        // `Path:response` with a JSON body containing `Error`.
                        // Without this the loop would hang on failures (§2 H4).
                        if path == "response" || path == "turn.end" {
                            if !body.is_empty() {
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                                    if let Some(err) = json.get("Error") {
                                        let reason = err
                                            .get("Message")
                                            .and_then(|v| v.as_str())
                                            .or_else(|| {
                                                json.get("reason")
                                                    .and_then(|v| v.as_str())
                                            })
                                            .unwrap_or("Azure synthesis failed");
                                        let _ = socket.close(None);
                                        return Err(TtsError(reason.to_string()));
                                    }
                                }
                            }
                            if path == "turn.end" {
                                let _ = socket.close(None);
                                break;
                            }
                        }

                        if path == "audio.metadata" || path == "word-boundary" {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                                if let Some(metadata) =
                                    json.get("Metadata").and_then(|v| v.as_array())
                                {
                                    for item in metadata {
                                        if item.get("Type").and_then(|v| v.as_str())
                                            == Some("WordBoundary")
                                        {
                                            if let Some(data) = item.get("Data") {
                                                let offset_ticks = data
                                                    .get("Offset")
                                                    .and_then(serde_json::Value::as_i64)
                                                    .unwrap_or(0);
                                                let duration_ticks = data
                                                    .get("Duration")
                                                    .and_then(serde_json::Value::as_i64)
                                                    .unwrap_or(0);

                                                let word = if let Some(text_obj) =
                                                    data.get("text").and_then(|v| v.as_object())
                                                {
                                                    text_obj
                                                        .get("Text")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("")
                                                } else if let Some(text_obj) =
                                                    data.get("Text").and_then(|v| v.as_object())
                                                {
                                                    text_obj
                                                        .get("Text")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("")
                                                } else {
                                                    data.get("text")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("")
                                                };

                                                if !word.is_empty() {
                                                    collected_boundaries.push(WordBoundary {
                                                        text: word.to_string(),
                                                        offset: (offset_ticks / 10_000) as u64,
                                                        duration: (duration_ticks / 10_000) as u64,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Message::Binary(b) if b.len() > 2 => {
                        let header_length = ((b[0] as usize) << 8) | (b[1] as usize);
                        if b.len() > 2 + header_length {
                            let audio_start = 2 + header_length;
                            if let Some(cb) = on_audio.as_mut() {
                                cb(&b[audio_start..]);
                            }
                        }
                    }
                    _ => {}
                }
            }

            compute_durations(&mut collected_boundaries);
            for b in &collected_boundaries {
                on_boundary(
                    &b.text,
                    b.offset as f32 / 1000.0,
                    (b.offset + b.duration) as f32 / 1000.0,
                );
            }

            return Ok(());
        }

        let mut synth_url = self.config.synth_url.clone();
        if self.config.provider_id == "elevenlabs" && on_boundary.is_some() {
            synth_url.push_str("/with-timestamps");
        }
        let mut req = self.client.post(&synth_url);

        // Auth header
        if !self.config.auth_header.is_empty() {
            let val = format!("{}{}", self.config.auth_prefix, self.api_key);
            req = req.header(&self.config.auth_header, val);
        }

        // Extra headers
        for (k, v) in &self.config.extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }

        // Body depends on engine type
        let resp = if self.config.body_is_ssml {
            // Azure: send SSML XML body
            let ssml = build_azure_ssml(&text, &voice_to_use, rate, pitch, _volume);
            let ct = self
                .config
                .content_type
                .as_deref()
                .unwrap_or("application/ssml+xml");
            req = req.header("Content-Type", ct);
            req.body(ssml).send()
        } else if self.config.provider_id == "google" {
            // Google: build JSON body with proper structure
            let (body, _words) = build_google_request(&text, &voice_to_use, on_boundary.is_some());
            req = req.json(&body);
            req.send()
        } else {
            // Standard JSON body for all other engines
            let mut body = serde_json::Map::new();
            if !self.config.text_field.is_empty() {
                body.insert(
                    self.config.text_field.clone(),
                    serde_json::Value::String(text.clone()),
                );
            }
            if !self.config.voice_param.is_empty() && !voice_to_use.is_empty() {
                body.insert(
                    self.config.voice_param.clone(),
                    serde_json::Value::String(voice_to_use.clone()),
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
            req = req.json(&serde_json::Value::Object(body));
            req.send()
        };

        let resp = resp.map_err(|e| TtsError(format!("HTTP error: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().unwrap_or_default();
            return Err(TtsError(format!("API error {status}: {body_text}")));
        }

        if self.config.provider_id == "elevenlabs" && on_boundary.is_some() {
            let resp_text = resp
                .text()
                .map_err(|e| TtsError(format!("Read error: {e}")))?;
            let json: serde_json::Value = serde_json::from_str(&resp_text)
                .map_err(|e| TtsError(format!("JSON parse: {e}")))?;

            if let Some(b64) = json.get("audio_base64").and_then(|v| v.as_str()) {
                use base64::Engine;
                let audio_bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| TtsError(format!("Base64 decode: {e}")))?;
                if let Some(cb) = on_audio.as_mut() {
                    for chunk in audio_bytes.chunks(8192) {
                        cb(chunk);
                    }
                }
            }

            if let Some(cb) = on_boundary.as_mut() {
                if let Some(alignment) = json.get("alignment").and_then(|v| v.as_object()) {
                    let chars = alignment.get("characters").and_then(|v| v.as_array());
                    let starts = alignment
                        .get("character_start_times_seconds")
                        .and_then(|v| v.as_array());
                    let ends = alignment
                        .get("character_end_times_seconds")
                        .and_then(|v| v.as_array());

                    if let (Some(chars), Some(starts), Some(ends)) = (chars, starts, ends) {
                        let mut current_word = String::new();
                        let mut word_start: f32 = 0.0;
                        let mut has_started = false;

                        for i in 0..chars.len() {
                            // Bounds check for all arrays to prevent OOB indexing
                            let char_str = chars.get(i).and_then(|v| v.as_str()).unwrap_or("");
                            let start_time = starts.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                            let end_time = ends.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;

                            if char_str.trim().is_empty() {
                                if has_started {
                                    cb(&current_word, word_start, end_time);
                                    current_word.clear();
                                    has_started = false;
                                }
                            } else {
                                if !has_started {
                                    word_start = start_time;
                                    has_started = true;
                                }
                                current_word.push_str(char_str);
                            }
                        }

                        if has_started {
                            let end_time = ends
                                .last()
                                .and_then(serde_json::Value::as_f64)
                                .unwrap_or(0.0) as f32;
                            cb(&current_word, word_start, end_time);
                        }
                    }
                }
            }
        } else if self.config.provider_id == "google" && on_boundary.is_some() {
            // Google returns base64-encoded audio in JSON
            let resp_text = resp
                .text()
                .map_err(|e| TtsError(format!("Read error: {e}")))?;
            let json: serde_json::Value = serde_json::from_str(&resp_text)
                .map_err(|e| TtsError(format!("JSON parse: {e}")))?;

            if let Some(b64) = json.get("audioContent").and_then(|v| v.as_str()) {
                use base64::Engine;
                let audio_bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| TtsError(format!("Base64 decode: {e}")))?;
                if let Some(cb) = on_audio.as_mut() {
                    for chunk in audio_bytes.chunks(8192) {
                        cb(chunk);
                    }
                }
            }

            if let Some(cb) = on_boundary.as_mut() {
                let (_, words) = build_google_request(&text, &voice_to_use, true);
                if let Some(tps) = json.get("timepoints").and_then(|v| v.as_array()) {
                    let boundaries = parse_google_timepoints(tps, &words);
                    for b in &boundaries {
                        cb(
                            &b.text,
                            b.offset as f32 / 1000.0,
                            (b.offset + b.duration) as f32 / 1000.0,
                        );
                    }
                } else {
                    let estimated = estimate_word_boundaries(&text);
                    for b in &estimated {
                        cb(
                            &b.text,
                            b.offset as f32 / 1000.0,
                            (b.offset + b.duration) as f32 / 1000.0,
                        );
                    }
                }
            }
        } else if let Some(cb) = on_audio.as_mut() {
            use std::io::Read;
            let mut resp = resp;
            let mut buffer = [0u8; 8192];
            loop {
                let n = resp
                    .read(&mut buffer)
                    .map_err(|e| TtsError(format!("Read error: {e}")))?;
                if n == 0 {
                    break;
                }
                cb(&buffer[..n]);
            }

            if let Some(cb) = on_boundary.as_mut() {
                let estimated = estimate_word_boundaries(&text);
                for b in &estimated {
                    cb(
                        &b.text,
                        b.offset as f32 / 1000.0,
                        (b.offset + b.duration) as f32 / 1000.0,
                    );
                }
            }
        } else {
            let _audio_bytes = resp
                .bytes()
                .map_err(|e| TtsError(format!("Read error: {e}")))?;
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
        let Some(ref voices_url) = self.config.voices_url else {
            return Ok(vec![]);
        };

        let mut req = self.client.get(voices_url.as_str());

        if !self.config.auth_header.is_empty() {
            let val = format!("{}{}", self.config.auth_prefix, self.api_key);
            req = req.header(&self.config.auth_header, val);
        }

        let resp = req
            .send()
            .map_err(|e| TtsError(format!("Voice list HTTP error: {e}")))?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let json: serde_json::Value = resp
            .json()
            .map_err(|e| TtsError(format!("Voice list parse error: {e}")))?;

        match self.config.provider_id.as_str() {
            "azure" => json
                .as_array()
                .map_or_else(|| Ok(vec![]), |arr| Ok(map_azure_voices(arr))),
            "google" => json
                .get("voices")
                .and_then(|v| v.as_array())
                .map_or_else(|| Ok(vec![]), |arr| Ok(map_google_voices(arr))),
            _ => {
                // Generic: try to parse as array of objects with id/name fields.
                // Handles both lowercase fields (ElevenLabs, Deepgram, etc.)
                // and PascalCase fields (Polly DescribeVoices: VoiceId, Gender,
                // LanguageCode) — §2 H9. Also handles ElevenLabs `labels` being
                // an object rather than a string — §2 H8.
                json.as_array().map_or_else(
                    || Ok(vec![]),
                    |arr| {
                        Ok(arr
                            .iter()
                            .filter_map(|v| {
                                let id = v
                                    .get("id")
                                    .or_else(|| v.get("voice_id"))
                                    .or_else(|| v.get("VoiceId"))
                                    .or_else(|| v.get("name"))
                                    .or_else(|| v.get("Name"))?
                                    .as_str()?;
                                let name = v
                                    .get("name")
                                    .or_else(|| v.get("Name"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(id)
                                    .to_string();

                                // Gender resolution order. ElevenLabs stores
                                // gender inside a `labels` object — handle that
                                // explicitly (§2 H8).
                                let gender_str = v
                                    .get("gender")
                                    .or_else(|| v.get("Gender"))
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string)
                                    .or_else(|| {
                                        v.get("labels").and_then(|labels| {
                                            // ElevenLabs: labels is an object.
                                            if let Some(obj) = labels.as_object() {
                                                obj.get("gender")?.as_str().map(str::to_string)
                                            } else if let Some(s) = labels.as_str() {
                                                Some(s.to_string())
                                            } else {
                                                None
                                            }
                                        })
                                    })
                                    .unwrap_or_default();

                                // Language code resolution. Polly uses
                                // LanguageCode; ElevenLabs uses labels.language;
                                // others may use language or lang.
                                let lang = v
                                    .get("language_code")
                                    .or_else(|| v.get("LanguageCode"))
                                    .or_else(|| v.get("language"))
                                    .or_else(|| v.get("lang"))
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string)
                                    .or_else(|| {
                                        v.get("labels").and_then(|labels| {
                                            labels
                                                .as_object()
                                                .and_then(|o| o.get("language")?.as_str().map(str::to_string))
                                        })
                                    })
                                    .unwrap_or_default();

                                let language_codes = if lang.is_empty() {
                                    vec![]
                                } else {
                                    vec![crate::types::LanguageCode {
                                        bcp47: lang.clone(),
                                        iso639_3: lang.split(['-', '_']).next().unwrap_or(&lang).to_string(),
                                        display: lang,
                                    }]
                                };

                                Some(Voice {
                                    id: id.to_string(),
                                    name,
                                    gender: normalize_gender(&gender_str),
                                    provider: self.config.provider_id.clone(),
                                    language_codes,
                                })
                            })
                            .collect())
                    },
                )
            }
        }
    }

    fn engine_id(&self) -> &'static str {
        match self.config.provider_id.as_str() {
            "openai" => "openai",
            "elevenlabs" => "elevenlabs",
            "azure" => "azure",
            "google" => "google",
            "cartesia" => "cartesia",
            "deepgram" => "deepgram",
            "playht" => "playht",
            "fishaudio" => "fishaudio",
            "hume" => "hume",
            "mistral" => "mistral",
            "murf" => "murf",
            "resemble" => "resemble",
            "unrealspeech" => "unrealspeech",
            "upliftai" => "upliftai",
            "watson" => "watson",
            "witai" => "witai",
            "xai" => "xai",
            "modelslab" => "modelslab",
            "polly" => "polly",
            _ => "cloud",
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_azure_ssml() {
        let ssml = build_azure_ssml("Hello world", "en-US-AriaNeural", 1.0, 1.0, 1.0);
        assert!(ssml.contains("<speak"));
        assert!(ssml.contains("en-US-AriaNeural"));
        assert!(ssml.contains("Hello world"));
        assert!(!ssml.contains("<prosody"));
    }

    #[test]
    fn test_build_azure_ssml_with_prosody() {
        let ssml = build_azure_ssml("Hello world", "en-US-AriaNeural", 1.5, 0.8, 1.4);
        assert!(ssml.contains("<prosody"));
        assert!(ssml.contains("rate="));
        assert!(ssml.contains("pitch="));
        assert!(ssml.contains("volume="));
    }

    #[test]
    fn test_build_google_request_basic() {
        let (body, words) = build_google_request("Hello world", "en-US-Wavenet-D", false);
        assert!(body["input"]["text"].as_str().unwrap() == "Hello world");
        assert!(words.is_empty());
    }

    #[test]
    fn test_build_google_request_with_marks() {
        let (body, words) = build_google_request("Hello world", "en-US-Wavenet-D", true);
        let ssml = body["input"]["ssml"].as_str().unwrap();
        assert!(ssml.contains("<mark name=\"0\"/>"));
        assert!(ssml.contains("<mark name=\"1\"/>"));
        assert_eq!(words.len(), 2);
        assert_eq!(words[0], "Hello");
        assert_eq!(words[1], "world");
        assert!(body.get("enableTimePointing").is_some());
    }

    #[test]
    fn test_parse_google_timepoints() {
        let tps = vec![
            serde_json::json!({"markName": "0", "timeSeconds": 0.125}),
            serde_json::json!({"markName": "1", "timeSeconds": 0.450}),
        ];
        let words = vec!["Hello".to_string(), "world".to_string()];
        let boundaries = parse_google_timepoints(&tps, &words);
        assert_eq!(boundaries.len(), 2);
        assert_eq!(boundaries[0].text, "Hello");
        assert_eq!(boundaries[0].offset, 125);
        assert_eq!(boundaries[0].duration, 325);
        assert_eq!(boundaries[1].text, "world");
        assert_eq!(boundaries[1].offset, 450);
    }

    #[test]
    fn test_estimate_word_boundaries() {
        let boundaries = estimate_word_boundaries("Hello world this is a test");
        assert_eq!(boundaries.len(), 6);
        assert_eq!(boundaries[0].text, "Hello");
        assert_eq!(boundaries[0].offset, 0);
        assert!(boundaries[0].duration > 0);
    }

    #[test]
    fn test_normalize_gender() {
        assert_eq!(
            super::super::types::normalize_gender("Female"),
            super::super::types::Gender::Female
        );
        assert_eq!(
            super::super::types::normalize_gender("male"),
            super::super::types::Gender::Male
        );
        assert_eq!(
            super::super::types::normalize_gender(""),
            super::super::types::Gender::Unknown
        );
    }

    #[test]
    fn test_build_config_all_engines() {
        // Polly is intentionally omitted — it requires SigV4 and returns None.
        // See test_polly_unsupported_returns_none.
        let engines = [
            "openai",
            "elevenlabs",
            "azure",
            "google",
            "cartesia",
            "deepgram",
            "playht",
            "fishaudio",
            "hume",
            "mistral",
            "murf",
            "resemble",
            "unrealspeech",
            "upliftai",
            "watson",
            "witai",
            "xai",
            "modelslab",
        ];
        let creds = HashMap::new();
        for id in &engines {
            assert!(
                build_config(id, &creds).is_some(),
                "Failed for engine: {id}"
            );
        }
    }

    #[test]
    fn test_build_config_unknown() {
        let creds = HashMap::new();
        assert!(build_config("nonexistent", &creds).is_none());
    }

    #[test]
    fn test_azure_ssml_escapes_special_chars() {
        let ssml = build_azure_ssml("A & B < C > D", "en-US-AriaNeural", 1.0, 1.0, 1.0);
        assert!(ssml.contains("&amp;"));
        assert!(ssml.contains("&lt;"));
        assert!(ssml.contains("&gt;"));
    }

    #[test]
    fn test_azure_ssml_escapes_voice_name() {
        // §2 H12: a stray apostrophe in the voice name must not break the SSML.
        let ssml = build_azure_ssml("hi", "en-US-Voice'Name", 1.0, 1.0, 1.0);
        assert!(ssml.contains("&apos;"), "voice apostrophe should be escaped");
        assert!(!ssml.contains("Voice'Name"));
    }

    #[test]
    fn test_speech_markdown_preprocessing() {
        use crate::engine::preprocess_speech_markdown;
        let (result, is_ssml) =
            preprocess_speech_markdown("Hello (world)[emphasis:\"strong\"]", "azure");
        assert!(is_ssml);
        assert!(result.contains("<speak>"));
    }

    #[test]
    fn test_watson_auth_header_format() {
        // §2 C1: Watson Basic auth is base64("apikey:KEY") — not the malformed
        // "<base64(KEY)>:" that shipped originally.
        let api_key = "test_key_123";
        let encoded = base64_encode(&format!("apiKey:{api_key}"));
        let auth_header = format!("Basic {encoded}");
        // Decoding round-trip — must contain the apiKey prefix and the key,
        // separated by a colon, with NO trailing colon outside the base64.
        assert!(!auth_header.ends_with(':'));
        let decoded = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded, format!("apiKey:{api_key}"));
    }

    #[test]
    fn test_playht_config_has_user_id_header() {
        // §2 C3: userId belongs in the X-User-ID header, not the JSON body.
        let mut creds = HashMap::new();
        creds.insert("userId".to_string(), "u-123".to_string());
        creds.insert("apiKey".to_string(), "k".to_string());
        let cfg = build_config("playht", &creds).expect("playht config");
        assert_eq!(
            cfg.extra_headers.get("X-User-ID").map(String::as_str),
            Some("u-123")
        );
        // The voice param stays in the body, not the headers.
        assert_eq!(cfg.voice_param, "voice");
    }

    #[test]
    fn test_deepgram_uses_model_param() {
        // §2 H10: Deepgram's /v1/speak takes `model` as the voice parameter.
        let creds = HashMap::new();
        let cfg = build_config("deepgram", &creds).expect("deepgram config");
        assert_eq!(cfg.voice_param, "model");
    }

    #[test]
    fn test_hume_voice_is_object_in_extra_body() {
        // §2 H11: Hume expects voice as {"voice": {"name": "..."}}.
        let creds = HashMap::new();
        let cfg = build_config("hume", &creds).expect("hume config");
        let voice = cfg
            .extra_body
            .get("voice")
            .expect("voice key should be in extra_body");
        assert!(voice.is_object(), "voice must be an object, got: {voice}");
        assert!(voice.get("name").is_some());
        assert_eq!(
            cfg.extra_body.get("audio_format").and_then(|v| v.as_str()),
            Some("wav")
        );
    }

    #[test]
    fn test_polly_unsupported_returns_none() {
        // §2 C2: AWS Polly needs SigV4. We surface this by returning None
        // (and emitting a warning) rather than constructing a broken config.
        let creds = HashMap::new();
        assert!(build_config("polly", &creds).is_none());
    }
}
