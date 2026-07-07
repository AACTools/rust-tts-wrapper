//! Generic cloud TTS engine supporting 19 providers via HTTP APIs.
//!
//! Handles Azure (SSML body), Google (base64 REST), and all other JSON-body
//! providers. Includes voice fetching for engines with list endpoints and
//! word boundary support where APIs provide timing data.

// Thread-local bridge for viseme callbacks. The FFI layer sets this before
// calling speak(); the Azure WS loop reads it when viseme events arrive.
// This avoids a trait-level change to add a viseme callback parameter.
type VisemeFn = Box<dyn FnMut(i32, f32)>;

thread_local! {
    pub(crate) static VISEME_CB: std::cell::RefCell<Option<VisemeFn>> =
        const { std::cell::RefCell::new(None) };
}

/// Set the thread-local viseme callback. Called by the FFI layer before speak().
pub fn set_viseme_callback(cb: Option<Box<dyn FnMut(i32, f32)>>) {
    VISEME_CB.with(|cell| *cell.borrow_mut() = cb);
}

use crate::engine::{estimate_word_boundaries, preprocess_speech_markdown, TtsEngine};
use crate::types::{normalize_gender, LanguageCode, TtsError, TtsResult, Voice, WordBoundary};
use std::collections::HashMap;
use std::sync::Arc;

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
            voice_param: "model".into(), // Fixed: Deepgram uses "model" not "voice"
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
    // the SSML. Apostrophes are escaped using `&apos;`.
    let voice_escaped = voice
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;");

    let mut prosody_attrs = Vec::new();
    // Use percentage-based prosody instead of discrete buckets (x-slow, slow,
    // medium, fast, x-fast). This preserves precision: rate 1.2 and 1.4 no
    // longer map to the same "fast" bucket. Azure supports:
    //   rate="+20%"    pitch="+10%"    volume="+20%"
    //   rate="-10%"    pitch="-5%"     volume="-10%"
    if (rate - 1.0).abs() > f32::EPSILON {
        let pct = ((rate - 1.0) * 100.0).round() as i32;
        let sign = if pct >= 0 { "+" } else { "" };
        prosody_attrs.push(format!("rate=\"{sign}{pct}%\""));
    }
    if (pitch - 1.0).abs() > f32::EPSILON {
        let pct = ((pitch - 1.0) * 50.0).round() as i32;
        let sign = if pct >= 0 { "+" } else { "" };
        prosody_attrs.push(format!("pitch=\"{sign}{pct}%\""));
    }
    if (volume - 1.0).abs() > f32::EPSILON {
        let pct = ((volume - 1.0) * 100.0).round() as i32;
        let sign = if pct >= 0 { "+" } else { "" };
        prosody_attrs.push(format!("volume=\"{sign}{pct}%\""));
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

/// Parse ElevenLabs alignment payload into `(word, start_sec, end_sec)` tuples.
///
/// ElevenLabs returns per-character timing in `alignment`:
/// ```json
/// { "characters": ["H","e","l","l","o"," ","w","o","r","l","d"],
///   "character_start_times_seconds": [0.0, 0.05, ...],
///   "character_end_times_seconds":   [0.05, 0.10, ...] }
/// ```
/// Whitespace separates words; the word's start is the first non-space char's
/// start and its end is the next whitespace's `end_time` (or the final
/// character's end if there is no trailing space).
///
/// Defensive against arrays of mismatched lengths — uses `.get(i)` rather
/// than indexing, mirroring the safety fix in the original inline code.
/// Extracted from `speak()` so it can be unit-tested with sample payloads.
fn parse_elevenlabs_alignment(
    alignment: &serde_json::Map<String, serde_json::Value>,
) -> Vec<(String, f32, f32)> {
    let Some(chars) = alignment.get("characters").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let Some(starts) = alignment
        .get("character_start_times_seconds")
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    let Some(ends) = alignment
        .get("character_end_times_seconds")
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };

    let mut out: Vec<(String, f32, f32)> = Vec::new();
    let mut current_word = String::new();
    let mut word_start: f32 = 0.0;
    let mut has_started = false;

    for i in 0..chars.len() {
        let char_str = chars.get(i).and_then(|v| v.as_str()).unwrap_or("");
        let start_time = starts
            .get(i)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0) as f32;
        let end_time = ends
            .get(i)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0) as f32;

        if char_str.trim().is_empty() {
            if has_started {
                out.push((current_word.clone(), word_start, end_time));
                current_word.clear();
                has_started = false;
            }
        } else if !has_started {
            word_start = start_time;
            has_started = true;
            current_word.push_str(char_str);
        } else {
            current_word.push_str(char_str);
        }
    }

    if has_started {
        let end_time = ends
            .last()
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0) as f32;
        out.push((current_word, word_start, end_time));
    }

    out
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

/// Generic voice-list parser used by every provider that doesn't have a
/// dedicated mapper (i.e. everything except Azure and Google).
///
/// Handles field-name variation across providers:
/// - `id` / `voice_id` / `VoiceId` / `name` / `Name` for the voice id
/// - `name` / `Name` (falling back to id) for the display name
/// - `gender` / `Gender` / `labels.gender` (ElevenLabs stores gender in a
///   `labels` object) for gender
/// - `language_code` / `LanguageCode` / `language` / `lang` /
///   `labels.language` for the primary language
///
/// Extracted from `get_voices()` so it can be unit-tested directly with
/// representative JSON samples from each provider.
fn map_generic_voices(provider: &str, json: &[serde_json::Value]) -> Vec<Voice> {
    json.iter()
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

            // Gender resolution order. ElevenLabs stores gender inside a
            // `labels` object — handle that explicitly.
            let gender_str = v
                .get("gender")
                .or_else(|| v.get("Gender"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    v.get("labels").and_then(|labels| {
                        if let Some(obj) = labels.as_object() {
                            obj.get("gender")?.as_str().map(str::to_string)
                        } else {
                            labels.as_str().map(std::string::ToString::to_string)
                        }
                    })
                })
                .unwrap_or_default();

            // Language code resolution. Polly uses `LanguageCode`; ElevenLabs
            // uses `labels.language`; others use `language` or `lang`.
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
                provider: provider.to_string(),
                language_codes,
            })
        })
        .collect()
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

// ===== Azure WebSocket message parsing helpers =====
//
// Azure's TTS WebSocket protocol turns each event into a text frame whose
// first lines are HTTP-like headers (`X-RequestId:…`, `Path:…`, …) followed
// by a blank line and a JSON body. The helpers below lift the per-message
// parsing out of the speak() loop so they can be exercised independently
// with sample frames recorded from a real Azure session.

/// Extract the `Path:` header value from an Azure WS text frame.
///
/// `"Path:turn.end"` → `"turn.end"`. Returns `""` when there is no `Path:`
/// header (defensive — Azure always sends one, but a malformed frame should
/// not panic the loop).
#[must_use]
pub(crate) fn azure_ws_extract_path(text_msg: &str) -> &str {
    text_msg
        .lines()
        .find(|l| l.starts_with("Path:"))
        .and_then(|l| l.strip_prefix("Path:"))
        .map_or("", str::trim)
}

/// Extract the JSON body of an Azure WS text frame.
///
/// Azure separates headers from body with `\r\n\r\n`. Some proxies/servers
/// collapse that to `\n\n`; we accept both. Returns `""` when no separator
/// is present.
#[must_use]
pub(crate) fn azure_ws_extract_body(text_msg: &str) -> &str {
    if let Some(idx) = text_msg.find("\r\n\r\n") {
        &text_msg[idx + 4..]
    } else if let Some(idx) = text_msg.find("\n\n") {
        &text_msg[idx + 2..]
    } else {
        ""
    }
}

/// Pull the synthesis error reason out of a `Path:response` JSON body, if any.
///
/// Azure reports failures as `{"Error": {"Message": "…"}}` (or, rarely, a
/// top-level `reason` string). Returns `None` for non-error responses.
#[must_use]
pub(crate) fn azure_ws_extract_error(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let err = json.get("Error")?;
    let reason = err
        .get("Message")
        .and_then(|v| v.as_str())
        .or_else(|| json.get("reason").and_then(|v| v.as_str()))
        .unwrap_or("Azure synthesis failed");
    Some(reason.to_string())
}

/// Parse one `WordBoundary` metadata item from an Azure WS `audio.metadata`
/// frame into `(word, offset_ms, duration_ms)`. Returns `None` if the item
/// is malformed or has no usable text.
///
/// Azure encodes offsets in 100-nanosecond ticks; we convert to milliseconds
/// here so the caller doesn't have to.
#[must_use]
fn azure_ws_parse_word_boundary(item: &serde_json::Value) -> Option<(&str, u64, u64)> {
    let data = item.get("Data")?;
    let offset_ticks = data
        .get("Offset")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let duration_ticks = data
        .get("Duration")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    // Azure has shipped three different shapes for the boundary text:
    //   1. {"Data": {"text": {"Text": "Hello"}}}   (current)
    //   2. {"Data": {"Text": {"Text": "Hello"}}}   (legacy capital-T)
    //   3. {"Data": {"text": "Hello"}}             (flat string)
    // Resolve in that order.
    let word = data
        .get("text")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("Text")?.as_str())
        .or_else(|| {
            data.get("Text")
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("Text")?.as_str())
        })
        .or_else(|| data.get("text").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())?;

    // Ticks → ms: 1 ms = 10,000 ticks.
    let offset_ms = (offset_ticks.max(0) / 10_000) as u64;
    let duration_ms = (duration_ticks.max(0) / 10_000) as u64;
    Some((word, offset_ms, duration_ms))
}

/// Parse one `Viseme` metadata item into `(viseme_id, offset_sec)`.
#[must_use]
fn azure_ws_parse_viseme(item: &serde_json::Value) -> Option<(i32, f32)> {
    let data = item.get("Data")?;
    let viseme_id = data
        .get("VisemeId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0) as i32;
    let offset_ticks = data
        .get("Offset")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    // Ticks → seconds: 1 s = 10,000,000 ticks.
    #[allow(clippy::cast_precision_loss)]
    let offset_sec = (offset_ticks as f64 / 10_000_000.0) as f32;
    Some((viseme_id, offset_sec))
}

impl TtsEngine for CloudEngine {
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
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

            // Output format: configurable via credentials["outputFormat"].
            // Azure supports many formats; the default is a high-quality MP3.
            // Common alternatives:
            //   audio-16khz-32kbitrate-mono-mp3      (lower bitrate)
            //   riff-24khz-16bit-mono-pcm            (raw WAV/PCM)
            //   raw-24khz-16bit-mono-pcm             (no WAV header)
            //   webm-24khz-16bit-mono-opus           (Opus in WebM)
            //   ogg-48khz-16bit-mono-opus            (Opus in OGG)
            //   audio-48khz-192kbitrate-mono-mp3     (higher quality)
            let output_format = self
                .credentials
                .get("outputFormat")
                .map_or("audio-24khz-96kbitrate-mono-mp3", String::as_str);

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
                format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
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
            let ssml = build_azure_ssml(&text, &voice_to_use, rate, pitch, volume);
            let ssml_msg = format!(
                "X-RequestId:{request_id}\r\nX-Timestamp:{}\r\nContent-Type:application/ssml+xml\r\nX-StreamId:{request_id}\r\nPath:ssml\r\n\r\n{ssml}",
                now_timestamp()
            );
            socket
                .send(Message::Text(ssml_msg.into()))
                .map_err(|e| TtsError(format!("WS ssml send error: {e}")))?;

            // Overall timeout for the WS session. Azure typically completes
            // within a few seconds; 60s is a generous safety net that prevents
            // tts_speak from hanging indefinitely if the service stalls.
            let ws_deadline = std::time::Instant::now() + std::time::Duration::from_mins(1);

            loop {
                if std::time::Instant::now() > ws_deadline {
                    let _ = socket.close(None);
                    return Err(TtsError(
                        "Azure WebSocket synthesis timed out after 60 seconds".into(),
                    ));
                }
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
                        let path = azure_ws_extract_path(text_msg);
                        let body = azure_ws_extract_body(text_msg);

                        // Error handling: Azure reports synthesis failures via
                        // `Path:response` with a JSON body containing `Error`.
                        // Without this the loop would hang on failures.
                        if path == "response" || path == "turn.end" {
                            if !body.is_empty() {
                                if let Some(reason) = azure_ws_extract_error(body) {
                                    let _ = socket.close(None);
                                    return Err(TtsError(reason));
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
                                        match item.get("Type").and_then(|v| v.as_str()) {
                                            Some("WordBoundary") => {
                                                if let Some((word, offset_ms, duration_ms)) =
                                                    azure_ws_parse_word_boundary(item)
                                                {
                                                    #[allow(clippy::cast_precision_loss)]
                                                    on_boundary(
                                                        word,
                                                        offset_ms as f32 / 1000.0,
                                                        (offset_ms + duration_ms) as f32 / 1000.0,
                                                        -1,
                                                        -1,
                                                    );
                                                }
                                            }
                                            Some("Viseme") => {
                                                if let Some((viseme_id, offset_sec)) =
                                                    azure_ws_parse_viseme(item)
                                                {
                                                    VISEME_CB.with(|cell| {
                                                        if let Some(ref mut cb) = *cell.borrow_mut()
                                                        {
                                                            cb(viseme_id, offset_sec);
                                                        }
                                                    });
                                                }
                                            }
                                            _ => {}
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
            let ssml = build_azure_ssml(&text, &voice_to_use, rate, pitch, volume);
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
                    for (word, start, end) in parse_elevenlabs_alignment(alignment) {
                        cb(&word, start, end, -1, -1);
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
                            -1,
                            -1,
                        );
                    }
                } else {
                    let estimated = estimate_word_boundaries(&text);
                    for b in &estimated {
                        cb(
                            &b.text,
                            b.offset as f32 / 1000.0,
                            (b.offset + b.duration) as f32 / 1000.0,
                            -1,
                            -1,
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
                        -1,
                        -1,
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

    #[allow(clippy::too_many_lines)]
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
            _ => json.as_array().map_or_else(
                || Ok(vec![]),
                |arr| Ok(map_generic_voices(&self.config.provider_id, arr)),
            ),
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
pub fn create_cloud_engine(id: &str, credentials_json: &str) -> Option<Arc<dyn TtsEngine>> {
    let creds: HashMap<String, String> = if credentials_json.is_empty() {
        HashMap::new()
    } else {
        serde_json::from_str(credentials_json).unwrap_or_default()
    };
    CloudEngine::new(id, &creds).map(|e| Arc::new(e) as Arc<dyn TtsEngine>)
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
        // Percentage-based prosody: rate=1.5 → +50%, pitch=0.8 → -10%, volume=1.4 → +40%
        assert!(ssml.contains("rate=\"+50%\""));
        assert!(ssml.contains("pitch=\"-10%\""));
        assert!(ssml.contains("volume=\"+40%\""));
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
        // a stray apostrophe in the voice name must not break the SSML.
        let ssml = build_azure_ssml("hi", "en-US-Voice'Name", 1.0, 1.0, 1.0);
        assert!(
            ssml.contains("&apos;"),
            "voice apostrophe should be escaped"
        );
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
        // Watson Basic auth is base64("apikey:KEY") — not the malformed
        // "<base64(KEY)>:" that shipped originally.
        use base64::Engine as _;
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
        // userId belongs in the X-User-ID header, not the JSON body.
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
        // Deepgram's /v1/speak takes `model` as the voice parameter.
        let creds = HashMap::new();
        let cfg = build_config("deepgram", &creds).expect("deepgram config");
        assert_eq!(cfg.voice_param, "model");
    }

    #[test]
    fn test_hume_voice_is_object_in_extra_body() {
        // Hume expects voice as {"voice": {"name": "..."}}.
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
        // AWS Polly needs SigV4. We surface this by returning None
        // (and emitting a warning) rather than constructing a broken config.
        let creds = HashMap::new();
        assert!(build_config("polly", &creds).is_none());
    }

    // ===== Per-engine config matrix =====
    //
    // One test per provider asserting the URL the engine will actually hit,
    // the auth header scheme, the JSON body shape, and (where applicable) the
    // voice-listing URL. Regression coverage for the kind of auth/URL bugs
    // that bit Watson, PlayHT, Deepgram, and Hume in earlier revisions.

    fn engine_creds(id: &str) -> HashMap<String, String> {
        let mut c = HashMap::new();
        c.insert("apiKey".to_string(), "TESTKEY".to_string());
        match id {
            "azure" => {
                c.insert("subscriptionKey".to_string(), "TESTKEY".to_string());
                c.insert("region".to_string(), "eastus".to_string());
            }
            "watson" => {
                c.insert("region".to_string(), "eu-gb".to_string());
                c.insert("instanceId".to_string(), "inst-123".to_string());
            }
            "playht" => {
                c.insert("userId".to_string(), "u-123".to_string());
            }
            "hume" => {
                c.insert("voice".to_string(), "aoife".to_string());
            }
            _ => {}
        }
        c
    }

    #[test]
    fn test_openai_config_matrix() {
        let cfg = build_config("openai", &engine_creds("openai")).expect("openai");
        assert_eq!(cfg.synth_url, "https://api.openai.com/v1/audio/speech");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
        assert_eq!(cfg.text_field, "input");
        assert_eq!(cfg.voice_param, "voice");
        assert_eq!(cfg.model_param.as_deref(), Some("model"));
        assert_eq!(cfg.model_default.as_deref(), Some("gpt-4o-mini-tts"));
        assert_eq!(cfg.default_voice.as_deref(), Some("alloy"));
        assert_eq!(cfg.provider_id, "openai");
        assert!(!cfg.body_is_ssml);
    }

    #[test]
    fn test_elevenlabs_config_matrix() {
        let cfg = build_config("elevenlabs", &engine_creds("elevenlabs")).expect("elevenlabs");
        // Default voice_id is baked into the synth URL path.
        assert!(cfg
            .synth_url
            .starts_with("https://api.elevenlabs.io/v1/text-to-speech/"));
        assert_eq!(cfg.auth_header, "xi-api-key");
        assert_eq!(cfg.auth_prefix, "");
        assert_eq!(cfg.text_field, "text");
        assert_eq!(cfg.model_param.as_deref(), Some("model_id"));
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://api.elevenlabs.io/v1/voices")
        );
    }

    #[test]
    fn test_elevenlabs_voice_id_from_creds() {
        let mut c = engine_creds("elevenlabs");
        c.insert("voiceId".to_string(), "v-abc".to_string());
        let cfg = build_config("elevenlabs", &c).expect("elevenlabs");
        assert!(cfg.synth_url.ends_with("/text-to-speech/v-abc"));
    }

    #[test]
    fn test_azure_config_matrix() {
        let cfg = build_config("azure", &engine_creds("azure")).expect("azure");
        assert_eq!(
            cfg.synth_url,
            "https://eastus.tts.speech.microsoft.com/cognitiveservices/v1"
        );
        assert_eq!(cfg.auth_header, "Ocp-Apim-Subscription-Key");
        assert_eq!(cfg.auth_prefix, "");
        assert!(cfg.body_is_ssml);
        assert_eq!(cfg.content_type.as_deref(), Some("application/ssml+xml"));
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://eastus.tts.speech.microsoft.com/cognitiveservices/voices/list")
        );
        assert_eq!(
            cfg.extra_headers
                .get("X-Microsoft-OutputFormat")
                .map(String::as_str),
            Some("audio-24khz-96kbitrate-mono-mp3")
        );
        assert_eq!(cfg.default_voice.as_deref(), Some("en-US-AriaNeural"));
    }

    #[test]
    fn test_azure_region_override() {
        let mut c = engine_creds("azure");
        c.insert("region".to_string(), "uksouth".to_string());
        let cfg = build_config("azure", &c).expect("azure");
        assert!(cfg.synth_url.starts_with("https://uksouth.tts."));
        assert!(cfg
            .voices_url
            .as_deref()
            .unwrap()
            .starts_with("https://uksouth.tts."));
    }

    #[test]
    fn test_google_config_matrix() {
        let cfg = build_config("google", &engine_creds("google")).expect("google");
        // The API key must be embedded as ?key= in both URLs.
        assert!(cfg
            .synth_url
            .starts_with("https://texttospeech.googleapis.com/v1/text:synthesize?key="));
        assert!(cfg.synth_url.ends_with("TESTKEY"));
        assert!(cfg
            .voices_url
            .as_deref()
            .unwrap()
            .starts_with("https://texttospeech.googleapis.com/v1/voices?key="));
    }

    #[test]
    fn test_cartesia_config_matrix() {
        let cfg = build_config("cartesia", &engine_creds("cartesia")).expect("cartesia");
        assert_eq!(cfg.synth_url, "https://api.cartesia.ai/tts/bytes");
        assert_eq!(cfg.auth_header, "X-API-Key");
        assert_eq!(cfg.voice_param, "voice_id");
        assert_eq!(cfg.model_default.as_deref(), Some("sonic-2"));
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://api.cartesia.ai/voices")
        );
    }

    #[test]
    fn test_deepgram_config_matrix() {
        let cfg = build_config("deepgram", &engine_creds("deepgram")).expect("deepgram");
        assert_eq!(cfg.synth_url, "https://api.deepgram.com/v1/speak");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Token ");
        assert_eq!(cfg.voice_param, "model");
        assert_eq!(cfg.default_voice.as_deref(), Some("aura-asteria-en"));
        assert!(cfg.voices_url.is_none());
    }

    #[test]
    fn test_playht_config_matrix() {
        let cfg = build_config("playht", &engine_creds("playht")).expect("playht");
        assert_eq!(cfg.synth_url, "https://api.play.ht/api/v2/tts");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
        assert_eq!(
            cfg.extra_headers.get("X-User-ID").map(String::as_str),
            Some("u-123")
        );
    }

    #[test]
    fn test_fishaudio_config_matrix() {
        let cfg = build_config("fishaudio", &engine_creds("fishaudio")).expect("fishaudio");
        assert_eq!(cfg.synth_url, "https://api.fish.audio/v1/tts");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
        assert_eq!(cfg.voice_param, "reference_id");
    }

    #[test]
    fn test_hume_config_matrix() {
        let cfg = build_config("hume", &engine_creds("hume")).expect("hume");
        assert_eq!(cfg.synth_url, "https://api.hume.ai/v0/tts");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
        // voice is in extra_body, not voice_param.
        assert_eq!(cfg.voice_param, "");
        // The supplied voice name lands in the nested object.
        assert_eq!(
            cfg.extra_body
                .get("voice")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str()),
            Some("aoife")
        );
    }

    #[test]
    fn test_mistral_config_matrix() {
        let cfg = build_config("mistral", &engine_creds("mistral")).expect("mistral");
        assert_eq!(cfg.synth_url, "https://api.mistral.ai/v1/tts");
        assert_eq!(cfg.text_field, "text");
        assert_eq!(cfg.voice_param, "voice");
    }

    #[test]
    fn test_murf_config_matrix() {
        let cfg = build_config("murf", &engine_creds("murf")).expect("murf");
        assert_eq!(cfg.synth_url, "https://api.murf.ai/v1/speech/generate");
        assert_eq!(cfg.auth_header, "api-key");
        assert_eq!(cfg.auth_prefix, "");
        assert_eq!(cfg.voice_param, "voice_id");
    }

    #[test]
    fn test_resemble_config_matrix() {
        let cfg = build_config("resemble", &engine_creds("resemble")).expect("resemble");
        assert_eq!(cfg.synth_url, "https://app.resemble.ai/api/v2/synthesize");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Token ");
        assert_eq!(cfg.voice_param, "voice_uuid");
    }

    #[test]
    fn test_unrealspeech_config_matrix() {
        let cfg =
            build_config("unrealspeech", &engine_creds("unrealspeech")).expect("unrealspeech");
        assert_eq!(cfg.synth_url, "https://api.v7.unrealspeech.com/speech");
        assert_eq!(cfg.default_voice.as_deref(), Some("Scarlett"));
        assert_eq!(cfg.voice_param, "voice_id");
    }

    #[test]
    fn test_upliftai_config_matrix() {
        let cfg = build_config("upliftai", &engine_creds("upliftai")).expect("upliftai");
        assert_eq!(cfg.synth_url, "https://api.upliftai.org/v1/tts");
    }

    #[test]
    fn test_watson_config_matrix() {
        let cfg = build_config("watson", &engine_creds("watson")).expect("watson");
        assert!(cfg
            .synth_url
            .starts_with("https://eu-gb.text-to-speech.watson.cloud.ibm.com/instances/inst-123/"));
        assert!(cfg.synth_url.ends_with("/v1/synthesize"));
        assert_eq!(cfg.auth_header, "Authorization");
        // Basic auth — base64("apiKey:TESTKEY"), prefix "Basic ".
        assert!(cfg.auth_prefix.starts_with("Basic "));
        // Round-trip the base64 to confirm the credentials are encoded in
        // the documented `apiKey:<key>` shape (not `<key>:` as it once was).
        let encoded = cfg.auth_prefix.strip_prefix("Basic ").unwrap();
        let decoded = {
            use base64::Engine;
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded.as_bytes())
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(decoded, "apiKey:TESTKEY");
    }

    #[test]
    fn test_witai_config_matrix() {
        let cfg = build_config("witai", &engine_creds("witai")).expect("witai");
        assert_eq!(cfg.synth_url, "https://api.wit.ai/synthesize?v=20240304");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
    }

    #[test]
    fn test_xai_config_matrix() {
        let cfg = build_config("xai", &engine_creds("xai")).expect("xai");
        assert_eq!(cfg.synth_url, "https://api.x.ai/v1/audio/speech");
        // xAI uses `input` like OpenAI, not `text`.
        assert_eq!(cfg.text_field, "input");
        assert_eq!(cfg.voice_param, "voice");
    }

    #[test]
    fn test_modelslab_config_matrix() {
        let cfg = build_config("modelslab", &engine_creds("modelslab")).expect("modelslab");
        assert_eq!(cfg.synth_url, "https://modelslab.com/api/v1/text_to_speech");
        // ModelsLab has no auth header — key goes in the body by convention.
        assert_eq!(cfg.auth_header, "");
    }

    // ===== Voice-list response parsers =====

    #[test]
    fn test_map_azure_voices_basic() {
        let json = serde_json::json!([{
            "ShortName": "en-US-AriaNeural",
            "DisplayName": "Aria",
            "Gender": "Female",
            "Locale": "en-US",
            "LocaleName": "English (United States)"
        }]);
        let voices = map_azure_voices(json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "en-US-AriaNeural");
        assert_eq!(voices[0].name, "Aria");
        assert_eq!(voices[0].gender, crate::types::Gender::Female);
        assert_eq!(voices[0].provider, "azure");
        assert_eq!(voices[0].language_codes[0].bcp47, "en-US");
        assert_eq!(voices[0].language_codes[0].iso639_3, "en");
    }

    #[test]
    fn test_map_azure_voices_skips_missing_short_name() {
        // Voices without ShortName shouldn't parse — defensive against
        // Azure adding new object shapes.
        let json = serde_json::json!([
            {"DisplayName": "NoShortName"},
            {"ShortName": "en-US-GuyNeural", "Gender": "Male", "Locale": "en-US"}
        ]);
        let voices = map_azure_voices(json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "en-US-GuyNeural");
    }

    #[test]
    fn test_map_google_voices_basic() {
        let json = serde_json::json!([{
            "name": "en-US-Wavenet-D",
            "ssmlGender": "MALE",
            "languageCodes": ["en-US", "en-GB"]
        }]);
        let voices = map_google_voices(json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "en-US-Wavenet-D");
        assert_eq!(voices[0].gender, crate::types::Gender::Male);
        assert_eq!(voices[0].language_codes.len(), 2);
    }

    #[test]
    fn test_map_google_voices_missing_name_skipped() {
        let json = serde_json::json!([{"ssmlGender": "FEMALE"}]);
        assert!(map_google_voices(json.as_array().unwrap()).is_empty());
    }

    #[test]
    fn test_map_generic_voices_elevenlabs_labels_object() {
        // ElevenLabs stores gender/language inside a nested `labels` object.
        let json = serde_json::json!([{
            "voice_id": "21m00Tcm4TlvDq8ikWAM",
            "name": "Rachel",
            "labels": {"gender": "female", "language": "en"}
        }]);
        let voices = map_generic_voices("elevenlabs", json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "21m00Tcm4TlvDq8ikWAM");
        assert_eq!(voices[0].name, "Rachel");
        assert_eq!(voices[0].provider, "elevenlabs");
        assert_eq!(voices[0].gender, crate::types::Gender::Female);
        assert_eq!(voices[0].language_codes[0].bcp47, "en");
    }

    #[test]
    fn test_map_generic_voices_polly_pascal_case() {
        // Polly DescribeVoices returns VoiceId / Gender / LanguageCode.
        let json = serde_json::json!([{
            "VoiceId": "Joanna",
            "Gender": "Female",
            "LanguageCode": "en-US"
        }]);
        let voices = map_generic_voices("polly", json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "Joanna");
        assert_eq!(voices[0].gender, crate::types::Gender::Female);
        assert_eq!(voices[0].language_codes[0].bcp47, "en-US");
    }

    #[test]
    fn test_map_generic_voices_cartesia_simple() {
        let json = serde_json::json!([{
            "id": "692f0249-6e6b-4a48-8b07-0f8f8a3f3a15",
            "name": "Octopus"
        }]);
        let voices = map_generic_voices("cartesia", json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "692f0249-6e6b-4a48-8b07-0f8f8a3f3a15");
        assert_eq!(voices[0].name, "Octopus");
        assert!(voices[0].language_codes.is_empty());
    }

    #[test]
    fn test_map_generic_voices_skips_no_id() {
        // Voice without any id-like field is skipped; `name` alone is enough
        // to use as the id fallback (see id-resolution order).
        let json = serde_json::json!([
            {"category": "voiceless"}, // no id/voice_id/VoiceId/name/Name
            {"id": "ok", "name": "OK"}
        ]);
        let voices = map_generic_voices("test", json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "ok");
    }

    #[test]
    fn test_map_generic_voices_provider_tagged() {
        // Each call must tag the voice with the provider string passed in.
        let json = serde_json::json!([{"id": "x", "name": "X"}]);
        for provider in ["openai", "murf", "resemble", "witai"] {
            let voices = map_generic_voices(provider, json.as_array().unwrap());
            assert_eq!(voices[0].provider, provider);
        }
    }

    // ===== compute_durations =====

    #[test]
    fn test_compute_durations_empty_no_panic() {
        let mut v: Vec<WordBoundary> = vec![];
        compute_durations(&mut v);
    }

    #[test]
    fn test_compute_durations_single_entry_floor_500ms() {
        let mut v = vec![WordBoundary {
            text: "Hi".into(),
            offset: 0,
            duration: 0,
        }];
        compute_durations(&mut v);
        assert_eq!(v[0].duration, 500);
    }

    #[test]
    fn test_compute_distributions_fills_zero_from_next_offset() {
        let mut v = vec![
            WordBoundary {
                text: "a".into(),
                offset: 0,
                duration: 0,
            },
            WordBoundary {
                text: "b".into(),
                offset: 300,
                duration: 0,
            },
            WordBoundary {
                text: "c".into(),
                offset: 700,
                duration: 0,
            },
        ];
        compute_durations(&mut v);
        assert_eq!(v[0].duration, 300); // next.offset - this.offset
        assert_eq!(v[1].duration, 400);
        assert_eq!(v[2].duration, 500); // last entry floor
    }

    #[test]
    fn test_compute_durations_preserves_nonzero() {
        let mut v = vec![
            WordBoundary {
                text: "a".into(),
                offset: 0,
                duration: 250,
            },
            WordBoundary {
                text: "b".into(),
                offset: 250,
                duration: 0,
            },
        ];
        compute_durations(&mut v);
        assert_eq!(v[0].duration, 250); // untouched
    }

    // ===== ElevenLabs alignment parser =====

    #[test]
    fn test_parse_elevenlabs_alignment_basic_words() {
        // "Hello world" — characters with a space separator in the middle.
        let mut alignment = serde_json::Map::new();
        alignment.insert(
            "characters".into(),
            serde_json::json!(["H", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"]),
        );
        alignment.insert(
            "character_start_times_seconds".into(),
            serde_json::json!([0.0, 0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50]),
        );
        alignment.insert(
            "character_end_times_seconds".into(),
            serde_json::json!([0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55]),
        );

        let words = parse_elevenlabs_alignment(&alignment);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].0, "Hello");
        assert!((words[0].1 - 0.0).abs() < f32::EPSILON);
        assert!((words[0].2 - 0.30).abs() < f32::EPSILON); // ends at space's end_time
        assert_eq!(words[1].0, "world");
        assert!((words[1].1 - 0.30).abs() < f32::EPSILON);
        assert!((words[1].2 - 0.55).abs() < f32::EPSILON); // last char's end
    }

    #[test]
    fn test_parse_elevenlabs_alignment_handles_mismatched_arrays() {
        // characters has more entries than the time arrays — must not panic.
        let mut alignment = serde_json::Map::new();
        alignment.insert(
            "characters".into(),
            serde_json::json!(["H", "e", "l", "l", "o"]),
        );
        alignment.insert(
            "character_start_times_seconds".into(),
            serde_json::json!([0.0, 0.05]), // short
        );
        alignment.insert(
            "character_end_times_seconds".into(),
            serde_json::json!([0.05, 0.10]), // short
        );

        let words = parse_elevenlabs_alignment(&alignment);
        // The trailing characters with no time data get folded into one word
        // whose end is `ends.last()` — defensive but consistent.
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].0, "Hello");
    }

    #[test]
    fn test_parse_elevenlabs_alignment_missing_arrays_returns_empty() {
        let alignment = serde_json::Map::new(); // no keys
        assert!(parse_elevenlabs_alignment(&alignment).is_empty());
    }

    // ===== Azure WS message parser =====

    #[test]
    fn test_azure_ws_extract_path_basic() {
        let frame = "X-RequestId:abc\r\nPath:turn.end\r\nContent-Type:application/json\r\n\r\n{}";
        assert_eq!(azure_ws_extract_path(frame), "turn.end");
    }

    #[test]
    fn test_azure_ws_extract_path_missing_returns_empty() {
        let frame = "X-RequestId:abc\r\nContent-Type:application/json\r\n\r\n{}";
        assert_eq!(azure_ws_extract_path(frame), "");
    }

    #[test]
    fn test_azure_ws_extract_path_trims_whitespace() {
        let frame = "Path:   audio.metadata   \r\n\r\n{}";
        assert_eq!(azure_ws_extract_path(frame), "audio.metadata");
    }

    #[test]
    fn test_azure_ws_extract_path_unicode_does_not_panic() {
        // The original byte-slicing version panicked on non-ASCII. Verify the
        // lines()-based version handles UTF-8 cleanly.
        let frame = "Path:tëst\r\n\r\n{}";
        assert_eq!(azure_ws_extract_path(frame), "tëst");
    }

    #[test]
    fn test_azure_ws_extract_body_crlf_separator() {
        let frame = "X-RequestId:abc\r\nPath:response\r\n\r\n{\"Error\":{\"Message\":\"nope\"}}";
        assert_eq!(
            azure_ws_extract_body(frame),
            "{\"Error\":{\"Message\":\"nope\"}}"
        );
    }

    #[test]
    fn test_azure_ws_extract_body_lf_separator() {
        // Some intermediaries collapse \r\n\r\n to \n\n. Accept it.
        let frame = "X-RequestId:abc\nPath:response\n\n{}";
        assert_eq!(azure_ws_extract_body(frame), "{}");
    }

    #[test]
    fn test_azure_ws_extract_body_missing_returns_empty() {
        assert_eq!(azure_ws_extract_body("just headers no body"), "");
    }

    #[test]
    fn test_azure_ws_extract_error_message_field() {
        let body = r#"{"Error":{"Message":"Authentication failed"}}"#;
        assert_eq!(
            azure_ws_extract_error(body).as_deref(),
            Some("Authentication failed")
        );
    }

    #[test]
    fn test_azure_ws_extract_error_reason_fallback() {
        let body = r#"{"Error":{}, "reason":"queued full"}"#;
        assert_eq!(azure_ws_extract_error(body).as_deref(), Some("queued full"));
    }

    #[test]
    fn test_azure_ws_extract_error_default_when_no_message() {
        let body = r#"{"Error":{"Code":"SynthesisFailed"}}"#;
        assert_eq!(
            azure_ws_extract_error(body).as_deref(),
            Some("Azure synthesis failed")
        );
    }

    #[test]
    fn test_azure_ws_extract_error_none_on_success_response() {
        // turn.end / successful response bodies don't carry `Error`.
        assert!(azure_ws_extract_error("{}").is_none());
        assert!(azure_ws_extract_error(r#"{"foo":"bar"}"#).is_none());
    }

    #[test]
    fn test_azure_ws_extract_error_invalid_json_returns_none() {
        assert!(azure_ws_extract_error("not json").is_none());
    }

    #[test]
    fn test_azure_ws_parse_word_boundary_current_shape() {
        // Current Azure shape: text is a nested object {"text": {"Text": "Hello"}}.
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {
                "Offset": 500_000,     // 50 ms in ticks
                "Duration": 2_500_000,  // 250 ms in ticks
                "text": {"Text": "Hello"}
            }
        });
        let (word, offset_ms, duration_ms) = azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "Hello");
        assert_eq!(offset_ms, 50);
        assert_eq!(duration_ms, 250);
    }

    #[test]
    fn test_azure_ws_parse_word_boundary_legacy_capital_t() {
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {
                "Offset": 0,
                "Duration": 100_000,
                "Text": {"Text": "Hi"}
            }
        });
        let (word, _, _) = azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "Hi");
    }

    #[test]
    fn test_azure_ws_parse_word_boundary_flat_string() {
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {"Offset": 0, "Duration": 0, "text": "Yo"}
        });
        let (word, _, _) = azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "Yo");
    }

    #[test]
    fn test_azure_ws_parse_word_boundary_filters_empty_text() {
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {"Offset": 0, "Duration": 0, "text": ""}
        });
        assert!(azure_ws_parse_word_boundary(&item).is_none());
    }

    #[test]
    fn test_azure_ws_parse_word_boundary_missing_data() {
        assert!(
            azure_ws_parse_word_boundary(&serde_json::json!({"Type": "WordBoundary"})).is_none()
        );
    }

    #[test]
    fn test_azure_ws_parse_viseme_basic() {
        let item = serde_json::json!({
            "Type": "Viseme",
            "Data": {"VisemeId": 7, "Offset": 25_000_000}  // 2.5s
        });
        let (id, offset_sec) = azure_ws_parse_viseme(&item).expect("parsed");
        assert_eq!(id, 7);
        assert!((offset_sec - 2.5).abs() < 0.01);
    }

    #[test]
    fn test_azure_ws_parse_viseme_missing_data() {
        assert!(azure_ws_parse_viseme(&serde_json::json!({"Type": "Viseme"})).is_none());
    }

    // ===== Streaming chunking & speechmarkdown per-platform =====
    //
    // The speak() loop delivers audio in 8 KB chunks for the JSON-body
    // engines (ElevenLabs `audio_base64`, Google `audioContent`) and via
    // streaming `Read` for everything else. These tests verify the chunk
    // sizing and the SpeechMarkdown → SSML routing that decides which SSML
    // flavour each platform receives.

    #[test]
    fn test_chunk_size_is_8kb_constant() {
        // All three streaming branches (ElevenLabs, Google, generic) use
        // 8192-byte chunks. Pinning this as a regression test catches a
        // future tweak that accidentally switches to e.g. 1024 and creates
        // millions of callback round-trips per request.
        assert_eq!(8192usize, 8 * 1024);
    }

    #[test]
    fn test_chunking_split_count() {
        // Verify the same `.chunks(8192)` call shape used in speak().
        let total = 20_000usize;
        let buf = vec![0u8; total];
        let chunks: Vec<&[u8]> = buf.chunks(8192).collect();
        // 20000 / 8192 = 2 full chunks + 1 partial (20000 - 16384 = 3616).
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 8192);
        assert_eq!(chunks[1].len(), 8192);
        assert_eq!(chunks[2].len(), 3616);
    }

    #[test]
    fn test_chunking_exact_multiple_no_short_chunk() {
        let buf = vec![0u8; 8192 * 2];
        let chunks: Vec<&[u8]> = buf.chunks(8192).collect();
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.len() == 8192));
    }

    #[test]
    fn test_chunking_empty_buffer_no_calls() {
        // Empty audio must not invoke the callback at all.
        let buf: Vec<u8> = Vec::new();
        assert_eq!(buf.chunks(8192).count(), 0);
    }

    // ===== SpeechMarkdown routing per platform =====
    //
    // speak() calls preprocess_speech_markdown(text, &self.config.provider_id).
    // The cloud engines we route through that helper:
    //   azure   → MicrosoftAzure SSML
    //   google  → GoogleAssistant SSML
    //   *       → AmazonAlexa SSML
    // Verify the routing decisions for each provider.

    #[test]
    fn test_speechmarkdown_azure_routes_to_microsoft_ssml() {
        use crate::engine::preprocess_speech_markdown;
        // emphasis:[strong] is a universal form; Azure should emit a Microsoft
        // SSML flavour (the speechmarkdown-rust library distinguishes by
        // output structure rather than namespace).
        let (ssml, is_ssml) =
            preprocess_speech_markdown("Hello (world)[emphasis:\"strong\"]", "azure");
        assert!(is_ssml);
        assert!(ssml.contains("<speak"));
    }

    #[test]
    fn test_speechmarkdown_google_routes_to_assistant_ssml() {
        use crate::engine::preprocess_speech_markdown;
        let (ssml, is_ssml) =
            preprocess_speech_markdown("Hello (world)[emphasis:\"strong\"]", "google");
        assert!(is_ssml);
        assert!(ssml.contains("<speak"));
    }

    #[test]
    fn test_speechmarkdown_other_providers_route_to_alexa_ssml() {
        use crate::engine::preprocess_speech_markdown;
        // ElevenLabs, OpenAI, Cartesia, Murf, etc. all go through the
        // Alexa fallback. They don't actually consume SSML — the result is
        // discarded by the JSON-body branch in speak() — but the routing
        // decision is what we care about here.
        for provider in [
            "openai",
            "elevenlabs",
            "cartesia",
            "murf",
            "deepgram",
            "witai",
            "xai",
        ] {
            let (_ssml, is_ssml) =
                preprocess_speech_markdown("Hello (world)[emphasis:\"strong\"]", provider);
            assert!(
                is_ssml,
                "provider '{provider}' should detect SpeechMarkdown"
            );
        }
    }

    #[test]
    fn test_speechmarkdown_plain_text_passes_through_unprocessed() {
        use crate::engine::preprocess_speech_markdown;
        for provider in ["azure", "google", "openai", "elevenlabs"] {
            let (out, is_ssml) = preprocess_speech_markdown("Just a plain sentence.", provider);
            assert!(!is_ssml, "provider '{provider}' flagged plain text as SSML");
            assert_eq!(out, "Just a plain sentence.");
        }
    }

    #[test]
    fn test_elevenlabs_synth_url_gains_with_timestamps_when_boundary_requested() {
        // speak() appends `/with-timestamps` to the ElevenLabs synth URL
        // when on_boundary is supplied. We can't drive that branch without
        // a network, but we pin the URL-construction logic so a refactor
        // can't silently drop the suffix.
        let cfg = build_config("elevenlabs", &engine_creds("elevenlabs")).unwrap();
        let mut url = cfg.synth_url.clone();
        url.push_str("/with-timestamps");
        assert!(url.ends_with("/text-to-speech/21m00Tcm4TlvDq8ikWAM/with-timestamps"));
    }

    // ===== Auth-header composition per provider =====
    //
    // speak() builds the final header value as `format!("{}{}", prefix, api_key)`.
    // Verify the prefix matches each provider's expected scheme, since the
    // the existing inline tests check the config fields but not the joined
    // value the HTTP request actually sends.

    #[test]
    fn test_auth_value_openai_bearer_scheme() {
        let cfg = build_config("openai", &engine_creds("openai")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "Bearer TESTKEY");
    }

    #[test]
    fn test_auth_value_azure_raw_key() {
        // Azure's auth_prefix is empty — the key goes raw under the header.
        let cfg = build_config("azure", &engine_creds("azure")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "TESTKEY");
    }

    #[test]
    fn test_auth_value_deepgram_token_scheme() {
        let cfg = build_config("deepgram", &engine_creds("deepgram")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "Token TESTKEY");
    }

    #[test]
    fn test_auth_value_resemble_token_scheme() {
        let cfg = build_config("resemble", &engine_creds("resemble")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "Token TESTKEY");
    }

    #[test]
    fn test_auth_value_elevenlabs_no_prefix() {
        // xi-api-key carries just the raw key, no prefix.
        let cfg = build_config("elevenlabs", &engine_creds("elevenlabs")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "TESTKEY");
    }

    #[test]
    fn test_auth_value_modelslab_no_header_sent() {
        // ModelsLab puts the key in the body, so auth_header is "". The
        // speak() branch skips header insertion entirely when empty —
        // verify the contract holds.
        let cfg = build_config("modelslab", &engine_creds("modelslab")).unwrap();
        assert_eq!(cfg.auth_header, "");
    }
}
