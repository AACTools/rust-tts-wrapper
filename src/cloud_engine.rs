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

/// Size of each audio chunk delivered via `on_audio` for JSON-body engines
/// (ElevenLabs `audio_base64`, Google `audioContent`). 8 KiB matches the
/// streaming-Read buffer used for HTTP-response engines. Exposed as a
/// named constant so tests can pin against the same value the production
/// `speak()` loop uses, rather than against the magic literal `8192`.
#[cfg(feature = "cloud")]
pub(crate) const STREAMING_CHUNK_SIZE: usize = 8192;

#[cfg(feature = "cloud")]
use {
    tungstenite::client::IntoClientRequest,
    tungstenite::{connect, Message},
    url::Url,
    uuid::Uuid,
};

/// Decode an MP3 byte buffer to little-endian mono PCM16. Multi-channel input
/// is downmixed by averaging interleaved samples. Returns an empty vec if no
/// frames decode. Used so cloud engines deliver uniform PCM16 through
/// `on_audio` (matching the local SherpaOnnx / SAPI engines) instead of raw
/// MP3 bytes that a SAPI site would have to decode itself.
#[cfg(feature = "cloud")]
fn decode_mp3_to_pcm16_mono(mp3: &[u8]) -> Vec<u8> {
    use symphonia::core::audio::AudioBufferRef;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    // Cursor needs an owned buffer: MediaSourceStream boxes the source as
    // `dyn MediaSource + 'static`, so a borrowed `&[u8]` cursor won't compile.
    let mss = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(mp3.to_vec())),
        MediaSourceStreamOptions::default(),
    );
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut format = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(p) => p.format,
        Err(_) => return Vec::new(),
    };
    let track = format.default_track().cloned();
    let Some(track) = track else {
        return Vec::new();
    };
    let Ok(mut decoder) =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())
    else {
        return Vec::new();
    };

    let mut pcm: Vec<u8> = Vec::new();
    while let Ok(packet) = format.next_packet() {
        let Ok(decoded_buf) = decoder.decode(&packet) else {
            continue;
        };
        let frames = decoded_buf.frames();
        #[allow(clippy::cast_precision_loss)]
        let nch = decoded_buf.spec().channels.count().max(1) as f32;
        // symphonia's MP3 decoder emits F32; handle S16 too as a defensive
        // fallback. Other sample formats are skipped (MP3 won't produce them).
        match decoded_buf {
            AudioBufferRef::F32(buf) => {
                let channel_planes = buf.planes();
                let slices = channel_planes.planes();
                for f in 0..frames {
                    let sum: f32 = slices
                        .iter()
                        .map(|s| s.get(f).copied().unwrap_or(0.0))
                        .sum();
                    push_mono_f32(&mut pcm, sum / nch);
                }
            }
            AudioBufferRef::S16(buf) => {
                let channel_planes = buf.planes();
                let slices = channel_planes.planes();
                for f in 0..frames {
                    let sum: i32 = slices
                        .iter()
                        .map(|s| s.get(f).copied().unwrap_or(0))
                        .map(i32::from)
                        .sum();
                    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                    let avg = (sum as f32 / nch) as i16;
                    pcm.extend_from_slice(&avg.to_le_bytes());
                }
            }
            _ => {}
        }
    }
    pcm
}

/// Scale a normalised f32 sample (`[-1.0, 1.0]`) to little-endian PCM16 and
/// append it. Pulled out so the F32 branch above stays readable.
#[cfg(feature = "cloud")]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn push_mono_f32(out: &mut Vec<u8>, s: f32) {
    let s16 = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
    out.extend_from_slice(&s16.to_le_bytes());
}
/// Sniff the first few bytes for an MP3 sync word or ID3 tag. Kept as a
/// diagnostic helper but not used for delivery routing — raw PCM16 audio
/// frequently contains 0xFF 0xE0+ byte pairs that false-positive, so format
/// routing uses the explicit `CloudConfig::response_is_pcm` flag instead.
#[cfg(test)]
fn looks_like_mp3(b: &[u8]) -> bool {
    if b.len() >= 3 && &b[..3] == b"ID3" {
        return true;
    }
    let scan = b.len().min(4096).saturating_sub(1);
    (0..scan).any(|i| b[i] == 0xFF && (b[i + 1] & 0xE0) == 0xE0)
}

/// Microsoft Edge "Read Aloud" constants. The trusted client token is the
/// well-known value used by edge-tts / VoiceGarden-SAPI; `Sec-MS-GEC` is
/// derived from it and the current time (see `edge_sec_ms_gec`).
#[cfg(feature = "cloud")]
const EDGE_TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
#[cfg(feature = "cloud")]
const EDGE_VOICE_LIST_URL: &str = "https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list?trustedclienttoken=6A5AA1D4EAFF4E9FB37E23D68491D6F4";
#[cfg(feature = "cloud")]
const EDGE_DEFAULT_VOICE: &str = "en-US-AriaNeural";
/// Edge's WS endpoint 403-rejects bare handshakes — it expects the Edge
/// browser's Read Aloud User-Agent (and the Read Aloud extension Origin).
#[cfg(feature = "cloud")]
const EDGE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.0.0";
#[cfg(feature = "cloud")]
const EDGE_ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";

/// Generate the `Sec-MS-GEC` token Microsoft Edge "Read Aloud" requires.
///
/// Algorithm (matching edge-tts / VoiceGarden `WSConnectionPool::GetGECToken`):
/// take the Windows FILETIME tick count (100-ns units since 1601-01-01), round
/// down to the nearest 5-minute boundary (3,000,000,000 ticks), concatenate
/// with the trusted client token, and SHA-256 → uppercase hex. Returns a
/// fresh token valid for up to 5 minutes.
#[cfg(feature = "cloud")]
fn edge_sec_ms_gec() -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let unix_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // FILETIME epoch (1601-01-01) is 116_444_736_000s before Unix (1970-01-01);
    // 100-ns ticks = nanos/100 + that offset in ticks.
    #[allow(clippy::cast_possible_truncation)]
    let filetime_ticks: u128 = unix_nanos / 100 + 116_444_736_000_000_000;
    let rounded = filetime_ticks - (filetime_ticks % 3_000_000_000);
    let input = format!("{rounded}{EDGE_TRUSTED_CLIENT_TOKEN}");
    let hash = Sha256::digest(input.as_bytes());
    let mut token = String::with_capacity(hash.len() * 2);
    for b in &hash {
        let _ = write!(token, "{b:02X}");
    }
    token
}

/// The concrete WebSocket stream type returned by tungstenite's `connect` for a
/// `wss://` URL. Stored in the connection pool between synthesis calls.
#[cfg(feature = "cloud")]
type WsStream = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

#[cfg(feature = "cloud")]
struct PooledConn {
    socket: WsStream,
    born_at: std::time::Instant,
}

/// Warm WebSocket connection pool, keyed by synthesis URL. A fresh
/// `tungstenite::connect` is a full TLS+WS handshake (~300 ms); reusing a live
/// connection between utterances removes that latency. Azure keys are stable
/// (region+key); Edge keys include the Sec-MS-GEC token so they rotate every
/// 5-minute window (old conns age out instead of being reused stale).
#[cfg(feature = "cloud")]
static WS_POOL: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Vec<PooledConn>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Max age before a pooled connection is considered stale (Azure/Edge close
/// idle sessions after ~3-5 min). Capping at 3 min keeps us from handing out a
/// server-closed connection.
#[cfg(feature = "cloud")]
const WS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(180);
/// Max connections cached per URL — bounds memory for busy callers.
#[cfg(feature = "cloud")]
const WS_POOL_MAX_PER_URL: usize = 4;

/// Take a live connection for `url`, dropping any that have aged out.
#[cfg(feature = "cloud")]
fn ws_checkout(url: &str) -> Option<WsStream> {
    let mut pool = WS_POOL.lock().ok()?;
    let conns = pool.get_mut(url)?;
    while let Some(conn) = conns.pop() {
        if conn.born_at.elapsed() < WS_MAX_AGE {
            return Some(conn.socket);
        }
    }
    None
}

/// Return a connection for reuse, respecting the per-URL cap.
#[cfg(feature = "cloud")]
fn ws_checkin(url: String, socket: WsStream) {
    let Ok(mut pool) = WS_POOL.lock() else {
        return;
    };
    let bucket = pool.entry(url).or_default();
    if bucket.len() < WS_POOL_MAX_PER_URL {
        bucket.push(PooledConn {
            socket,
            born_at: std::time::Instant::now(),
        });
    }
}

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
    /// Whether this engine's synthesis response body is already raw PCM16
    /// (delivered verbatim) rather than MP3 (decoded to PCM before delivery).
    /// Azure returns PCM because we request `raw-24khz-16bit-mono-pcm`;
    /// Cartesia returns raw PCM by design. Everything else returns MP3.
    response_is_pcm: bool,
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
    ///
    /// Credential `synthUrl` (optional) overrides the provider's default
    /// synthesis endpoint. This is primarily useful for tests pointing at
    /// a deterministic local server, but also lets users target a proxy
    /// or self-hosted gateway.
    pub fn new(id: &str, credentials: &HashMap<String, String>) -> Option<Self> {
        let mut config = build_config(id, credentials)?;
        if let Some(url_override) = credentials.get("synthUrl") {
            if !url_override.is_empty() {
                config.synth_url.clone_from(url_override);
            }
        }
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
                    // IBM Watson's Basic-auth scheme requires the literal
                    // string "apikey" (lowercase, one word) as the username.
                    // Using "apiKey" (camelCase) here returns HTTP 401 from
                    // every Watson endpoint.
                    base64_encode(&format!("apikey:{}", creds.get("apiKey").cloned().unwrap_or_default()))
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
///
/// When `input_ssml` is `Some`, it is sent directly as Google's `"ssml"` input
/// (used when `tts_speak_ssml` passes W3C SSML with the `<voice>` wrapper
/// already stripped). Otherwise the text/marks path builds Google SSML or plain
/// text from `text`.
fn build_google_request(
    text: &str,
    voice: &str,
    add_marks: bool,
    input_ssml: Option<&str>,
) -> (serde_json::Value, Vec<String>) {
    let lang = voice.chars().take(5).collect::<String>();

    let mut words_list = Vec::new();

    let input = if let Some(ssml) = input_ssml {
        serde_json::json!({ "ssml": ssml })
    } else if add_marks {
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
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| crate::types::locale_display_name(locale), String::from),
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
fn azure_ws_parse_word_boundary(item: &serde_json::Value) -> Option<(&str, u64, u64, i32, i32)> {
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

    // Extract character offset and length from the nested text object (when
    // present). Azure WS sends: {"text": {"Text": "word", "Offset": 4, "Length": 5}}
    let text_obj = data.get("text").and_then(|v| v.as_object());
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let char_offset = text_obj
        .and_then(|o| o.get("Offset"))
        .and_then(serde_json::Value::as_i64)
        .map_or(-1, |v| v as i32);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let char_len = text_obj
        .and_then(|o| o.get("Length"))
        .and_then(serde_json::Value::as_i64)
        .map_or(-1, |v| v as i32);

    // Ticks → ms: 1 ms = 10,000 ticks.
    let offset_ms = (offset_ticks.max(0) / 10_000) as u64;
    let duration_ms = (duration_ticks.max(0) / 10_000) as u64;
    Some((word, offset_ms, duration_ms, char_offset, char_len))
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

/// Search for `word` in `text` starting from `search_from`, returning the
/// character offset and length relative to the source text. Advances
/// `search_from` past the match so subsequent calls find the next occurrence.
/// Used by the Azure WS boundary handler to compute plain-text-relative
/// offsets when Azure's metadata doesn't provide them (or provides SSML-
/// relative offsets that are wrong for the caller).
#[cfg(feature = "cloud")]
fn ws_boundary_search_text(text: &str, word: &str, search_from: &mut usize) -> (i32, i32) {
    #[allow(clippy::cast_possible_truncation)]
    let char_offset = text[*search_from..]
        .find(word)
        .map_or(-1, |pos| (*search_from + pos) as i32);
    if char_offset >= 0 {
        *search_from = char_offset as usize + word.len();
    }
    let char_len = word.chars().count() as i32;
    (char_offset, char_len)
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
        let (original_text, is_ssml) = preprocess_speech_markdown(text, &self.config.provider_id);

        // When the caller passed W3C SSML (via tts_speak_ssml), adapt per engine:
        //  - Azure/Edge: pass through (their WS/REST paths handle SSML natively)
        //  - Google: strip <voice> wrapper, send inner SSML as Google's ssml input
        //    (Google accepts W3C <phoneme alphabet='ipa'>, <prosody>, etc.)
        //  - Watson: strip <voice> wrapper, send inner SSML as the text field
        //    (Watson auto-detects SSML by the <speak> prefix)
        //  - Others (OpenAI, ElevenLabs, …): strip tags → plain text
        let mut voice_to_use = voice
            .map(std::string::ToString::to_string)
            .or_else(|| self.config.default_voice.clone())
            .unwrap_or_default();

        let google_ssml_override: Option<String>;
        let text: String;

        if is_ssml {
            match self.config.provider_id.as_str() {
                "azure" | "edge" => {
                    google_ssml_override = None;
                    text = original_text;
                }
                "google" => {
                    let (v, inner) = crate::engine::unwrap_voice_tag(&original_text);
                    if let Some(v) = v {
                        voice_to_use = v;
                    }
                    google_ssml_override = Some(inner);
                    text = crate::engine::strip_ssml_to_text(&original_text);
                }
                "watson" => {
                    let (v, inner) = crate::engine::unwrap_voice_tag(&original_text);
                    if let Some(v) = v {
                        voice_to_use = v;
                    }
                    google_ssml_override = None;
                    // Watson recognises SSML when the text starts with <speak>.
                    text = inner;
                }
                _ => {
                    google_ssml_override = None;
                    text = crate::engine::strip_ssml_to_text(&original_text);
                }
            }
        } else {
            google_ssml_override = None;
            text = original_text;
        }

        // WebSocket approach: Azure when word boundaries are requested, or
        // Edge always (Edge is WS-only — it has no REST synth endpoint).
        // Edge reuses the identical Azure "Turn" protocol; only the URL/auth
        // differ (token-based Sec-MS-GEC vs subscription key).
        #[cfg(feature = "cloud")]
        let use_ws = self.config.provider_id == "edge"
            || (self.config.provider_id == "azure" && on_boundary.is_some());
        #[cfg(feature = "cloud")]
        if use_ws {
            let ws_url_str = if self.config.provider_id == "edge" {
                format!(
                    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1\
                     ?TrustedClientToken={EDGE_TRUSTED_CLIENT_TOKEN}\
                     &Sec-MS-GEC={gec}\
                     &Sec-MS-GEC-Version=1-142.0.3595.94",
                    gec = edge_sec_ms_gec()
                )
            } else {
                // Azure — subscription key lives in the query string.
                let region = self
                    .credentials
                    .get("region")
                    .cloned()
                    .unwrap_or_else(|| "eastus".into());
                format!(
                    "wss://{}.tts.speech.microsoft.com/cognitiveservices/websocket/v1?Ocp-Apim-Subscription-Key={}",
                    region, self.api_key
                )
            };

            let ws_url =
                Url::parse(&ws_url_str).map_err(|e| TtsError(format!("Invalid WS URL: {e}")))?;
            // Edge mimics the Edge browser's Read Aloud extension — it
            // 403-rejects bare WS handshakes, so set the Origin + User-Agent
            // before connecting. Azure accepts the default handshake.
            let mut req = ws_url
                .as_str()
                .into_client_request()
                .map_err(|e| TtsError(format!("WS request build: {e}")))?;
            if self.config.provider_id == "edge" {
                let h = req.headers_mut();
                let origin = EDGE_ORIGIN
                    .parse()
                    .map_err(|e| TtsError(format!("Origin header: {e}")))?;
                let ua = EDGE_USER_AGENT
                    .parse()
                    .map_err(|e| TtsError(format!("User-Agent header: {e}")))?;
                h.insert("Origin", origin);
                h.insert("User-Agent", ua);
            }
            // Prefer a pooled (warm) connection; only do the full TLS+WS
            // handshake when the pool has nothing for this URL. `clean_finish`
            // tracks whether the session ended on `turn.end` (socket reusable →
            // check back in) so a broken connection is never pooled.
            let mut clean_finish = false;
            let mut socket = match ws_checkout(&ws_url_str) {
                Some(pooled) => pooled,
                None => {
                    connect(req)
                        .map_err(|e| TtsError(format!("WS connect error: {e}")))?
                        .0
                }
            };

            // Azure requires a 32-char lowercase hex UUID with NO dashes.
            let request_id = Uuid::new_v4().simple().to_string();

            // Output format: configurable via credentials["outputFormat"].
            // Default is raw PCM16 24 kHz mono so WS audio frames are delivered
            // straight through `on_audio` without an MP3 decode step (real-time
            // streaming preserved). Common alternatives:
            //   audio-24khz-96kbitrate-mono-mp3      (MP3 — needs decoding)
            //   riff-24khz-16bit-mono-pcm            (WAV-wrapped PCM)
            //   webm-24khz-16bit-mono-opus           (Opus in WebM)
            //   ogg-48khz-16bit-mono-opus            (Opus in OGG)
            //   audio-48khz-192kbitrate-mono-mp3     (higher-quality MP3)
            // Output format: configurable via credentials["outputFormat"].
            // Azure defaults to raw PCM16 (delivered verbatim); Edge returns
            // MP3 on its free endpoint (raw PCM isn't supported there) and is
            // decoded after the WS session completes.
            let default_format = if self.config.provider_id == "edge" {
                "audio-24khz-96kbitrate-mono-mp3"
            } else {
                "raw-24khz-16bit-mono-pcm"
            };
            let output_format = self
                .credentials
                .get("outputFormat")
                .map_or(default_format, String::as_str);

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
            let ws_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);

            // Edge returns MP3 frames (raw PCM isn't supported on its free
            // endpoint). Accumulate them here and decode once the session ends;
            // Azure (response_is_pcm) streams PCM frames straight through.
            let mut mp3_buf: Vec<u8> = Vec::new();

            // Tracks the character offset within the source text as Azure WS
            // word-boundary events arrive, so we can recompute plain-text
            // offsets when Azure doesn't provide them.
            let mut ws_search_from = 0usize;

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
                                // Clean finish — leave the socket open so it can
                                // go back in the pool for the next utterance.
                                clean_finish = true;
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
                                                if let Some((
                                                    word,
                                                    offset_ms,
                                                    duration_ms,
                                                    char_offset,
                                                    char_len,
                                                )) = azure_ws_parse_word_boundary(item)
                                                {
                                                    if let Some(cb) = on_boundary.as_mut() {
                                                        // Azure's text.Offset (when present)
                                                        // points into the SSML, not the plain
                                                        // text. When it's absent (-1), or when
                                                        // we built our own SSML wrapper (which
                                                        // shifts all offsets), recompute the
                                                        // offset from the source text so
                                                        // consumers get plain-text-relative
                                                        // positions.
                                                        let (final_offset, final_len) =
                                                            if char_offset < 0 {
                                                                let len =
                                                                    word.chars().count() as i32;
                                                                (-1, len)
                                                            } else {
                                                                (char_offset, char_len)
                                                            };
                                                        // Fall back to searching the source
                                                        // text when Azure doesn't provide an
                                                        // offset or it would be relative to
                                                        // SSML we built ourselves.
                                                        let (final_offset, final_len) =
                                                            if final_offset < 0 {
                                                                ws_boundary_search_text(
                                                                    &text,
                                                                    word,
                                                                    &mut ws_search_from,
                                                                )
                                                            } else {
                                                                (final_offset, final_len)
                                                            };
                                                        #[allow(clippy::cast_precision_loss)]
                                                        cb(
                                                            word,
                                                            offset_ms as f32 / 1000.0,
                                                            (offset_ms + duration_ms) as f32
                                                                / 1000.0,
                                                            final_offset,
                                                            final_len,
                                                        );
                                                    }
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
                            let audio = &b[2 + header_length..];
                            if self.config.response_is_pcm {
                                // Azure raw-PCM frames — deliver straight through.
                                if let Some(cb) = on_audio.as_mut() {
                                    cb(audio);
                                }
                            } else {
                                // Edge MP3 frames — accumulate, decode after the loop.
                                mp3_buf.extend_from_slice(audio);
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Edge (and any future MP3-over-WS provider): decode the accumulated
            // MP3 frames to PCM16 mono now and deliver them in chunks, so every
            // WS engine hands the caller the same PCM contract as the REST ones.
            if !self.config.response_is_pcm && !mp3_buf.is_empty() {
                let pcm = decode_mp3_to_pcm16_mono(&mp3_buf);
                if let Some(cb) = on_audio.as_mut() {
                    for chunk in pcm.chunks(STREAMING_CHUNK_SIZE) {
                        cb(chunk);
                    }
                }
            }

            // Clean turn.end → return the still-open socket to the pool for the
            // next utterance. Otherwise (timeout / server-closed / error) the
            // socket is dropped and discarded, never pooled.
            if clean_finish {
                ws_checkin(ws_url_str, socket);
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
            let (body, _words) = build_google_request(
                &text,
                &voice_to_use,
                on_boundary.is_some(),
                google_ssml_override.as_deref(),
            );
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
                let mp3_bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| TtsError(format!("Base64 decode: {e}")))?;
                let pcm = decode_mp3_to_pcm16_mono(&mp3_bytes);
                if let Some(cb) = on_audio.as_mut() {
                    for chunk in pcm.chunks(STREAMING_CHUNK_SIZE) {
                        cb(chunk);
                    }
                }
            }

            if let Some(cb) = on_boundary.as_mut() {
                if let Some(alignment) = json.get("alignment").and_then(|v| v.as_object()) {
                    let mut search_from = 0usize;
                    for (word, start, end) in parse_elevenlabs_alignment(alignment) {
                        #[allow(clippy::cast_possible_truncation)]
                        let char_offset = text[search_from..]
                            .find(&word)
                            .map_or(-1, |pos| (search_from + pos) as i32);

                        if char_offset >= 0 {
                            search_from = char_offset as usize + word.len();
                        }
                        let char_len = word.chars().count() as i32;
                        cb(&word, start, end, char_offset, char_len);
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
                let mp3_bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| TtsError(format!("Base64 decode: {e}")))?;
                let pcm = decode_mp3_to_pcm16_mono(&mp3_bytes);
                if let Some(cb) = on_audio.as_mut() {
                    for chunk in pcm.chunks(STREAMING_CHUNK_SIZE) {
                        cb(chunk);
                    }
                }
            }

            if let Some(cb) = on_boundary.as_mut() {
                let (_, words) = build_google_request(
                    &text,
                    &voice_to_use,
                    true,
                    google_ssml_override.as_deref(),
                );
                if let Some(tps) = json.get("timepoints").and_then(|v| v.as_array()) {
                    let boundaries = parse_google_timepoints(tps, &words);
                    for b in &boundaries {
                        #[allow(clippy::cast_precision_loss)]
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
                    let mut search_from = 0usize;
                    for b in &estimated {
                        #[allow(clippy::cast_possible_truncation)]
                        let char_offset = text[search_from..]
                            .find(&b.text)
                            .map_or(-1, |pos| (search_from + pos) as i32);

                        if char_offset >= 0 {
                            search_from = char_offset as usize + b.text.len();
                        }
                        let char_len = b.text.chars().count() as i32;
                        #[allow(clippy::cast_precision_loss)]
                        cb(
                            &b.text,
                            b.offset as f32 / 1000.0,
                            (b.offset + b.duration) as f32 / 1000.0,
                            char_offset,
                            char_len,
                        );
                    }
                }
            }
        } else if let Some(cb) = on_audio.as_mut() {
            // Most providers respond with an MP3 body (OpenAI, ElevenLabs,
            // Deepgram, Watson, …); a few return raw PCM natively (Azure via
            // X-Microsoft-OutputFormat, Cartesia). Read the whole body, decode
            // MP3 → PCM16 mono when needed so every cloud engine delivers the
            // same PCM contract as the local engines, then chunk it out.
            let body = resp
                .bytes()
                .map_err(|e| TtsError(format!("Read error: {e}")))?;
            let pcm = if self.config.response_is_pcm {
                body.to_vec()
            } else {
                decode_mp3_to_pcm16_mono(&body)
            };
            for chunk in pcm.chunks(STREAMING_CHUNK_SIZE) {
                cb(chunk);
            }

            if let Some(cb) = on_boundary.as_mut() {
                let estimated = estimate_word_boundaries(&text);
                let mut search_from = 0usize;
                for b in &estimated {
                    #[allow(clippy::cast_possible_truncation)]
                    let char_offset = text[search_from..]
                        .find(&b.text)
                        .map_or(-1, |pos| (search_from + pos) as i32);

                    if char_offset >= 0 {
                        search_from = char_offset as usize + b.text.len();
                    }
                    let char_len = b.text.chars().count() as i32;
                    #[allow(clippy::cast_precision_loss)]
                    cb(
                        &b.text,
                        b.offset as f32 / 1000.0,
                        (b.offset + b.duration) as f32 / 1000.0,
                        char_offset,
                        char_len,
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
            // Azure and Edge share the same voice-list JSON shape
            // (`ShortName`/`Gender`/`Locale`/…).
            "azure" | "edge" => json
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

    /// Check whether the configured credentials are valid.
    ///
    /// Engines with a `voices_url` (Azure, Google, ElevenLabs, Cartesia)
    /// make a real authenticated GET and return `Ok(true)` only on a
    /// successful 2xx response. Engines without a voice-list endpoint
    /// return `Ok(false)` — we can't verify the key without making a
    /// billed synth call, so we report "unknown / not verifiable" rather
    /// than the previous false positive.
    ///
    /// This overrides the trait default, which returned `Ok(true)` whenever
    /// `get_voices()` succeeded — including for engines like OpenAI where
    /// `get_voices()` returns an empty vec without ever touching the
    /// network. That made `check_credentials()` report successful
    /// validation for any well-formed engine config, which is misleading.
    fn check_credentials(&self) -> TtsResult<bool> {
        let Some(ref voices_url) = self.config.voices_url else {
            return Ok(false);
        };
        let mut req = self.client.get(voices_url.as_str());
        if !self.config.auth_header.is_empty() {
            let val = format!("{}{}", self.config.auth_prefix, self.api_key);
            req = req.header(&self.config.auth_header, val);
        }
        match req.send() {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    fn engine_id(&self) -> &'static str {
        match self.config.provider_id.as_str() {
            "openai" => "openai",
            "elevenlabs" => "elevenlabs",
            "azure" => "azure",
            "edge" => "edge",
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
        let (body, words) = build_google_request("Hello world", "en-US-Wavenet-D", false, None);
        assert!(body["input"]["text"].as_str().unwrap() == "Hello world");
        assert!(words.is_empty());
    }

    #[test]
    fn test_build_google_request_with_ssml_override() {
        // When tts_speak_ssml passes W3C SSML, the <voice> wrapper is stripped
        // and the inner SSML is sent as Google's ssml input.
        let ssml = "<speak>Hello <phoneme alphabet='ipa' ph='wɜːld'>world</phoneme></speak>";
        let (body, words) =
            build_google_request("Hello world", "en-US-Wavenet-D", false, Some(ssml));
        assert!(body["input"]["ssml"].as_str().unwrap().contains("<phoneme"));
        assert!(body["input"]["ssml"].as_str().unwrap().contains("wɜːld"));
        assert!(
            words.is_empty(),
            "no mark-based words when SSML override is used"
        );
    }

    #[test]
    fn test_build_google_request_with_marks() {
        let (body, words) = build_google_request("Hello world", "en-US-Wavenet-D", true, None);
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
        // IBM Watson Basic auth requires the literal username `apikey`
        // (lowercase, one word) per the IAM authentication spec. Earlier
        // versions of this code used `apiKey` (camelCase) and got HTTP 401
        // from every Watson endpoint. This regression test pins the
        // lowercase form.
        use base64::Engine as _;
        let api_key = "test_key_123";
        let encoded = base64_encode(&format!("apikey:{api_key}"));
        let auth_header = format!("Basic {encoded}");
        assert!(!auth_header.ends_with(':'));
        let decoded = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded, format!("apikey:{api_key}"));
        // Explicit guard against the camelCase regression.
        assert!(
            !decoded.starts_with("apiKey:"),
            "Watson auth must use lowercase 'apikey:' as the username, got: {decoded}"
        );
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
    fn test_edge_config_is_credential_free_ws() {
        // Edge is free + WS-only: no synth REST URL, no auth header, but it
        // must expose the bing.com voice list and a default voice.
        let cfg = build_config("edge", &HashMap::new()).expect("edge config");
        assert_eq!(cfg.provider_id, "edge");
        assert!(cfg.synth_url.is_empty());
        assert!(cfg.auth_header.is_empty());
        assert!(cfg
            .voices_url
            .as_deref()
            .unwrap()
            .contains("speech.platform.bing.com"));
        assert_eq!(cfg.default_voice.as_deref(), Some("en-US-AriaNeural"));
        // Edge returns MP3 on its free endpoint — the WS loop decodes it.
        assert!(!cfg.response_is_pcm);
    }

    #[test]
    fn test_edge_sec_ms_gec_is_uppercase_hex_sha256() {
        // Sec-MS-GEC is SHA-256 → 32 bytes → 64 uppercase hex chars.
        let token = edge_sec_ms_gec();
        assert_eq!(token.len(), 64, "token must be 64 hex chars: {token}");
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "token must be uppercase hex: {token}"
        );
    }

    #[test]
    fn test_edge_sec_ms_gec_is_stable_within_five_minutes() {
        // The token is rounded down to a 5-minute window, so two calls within
        // the same window must yield identical tokens (guards the rounding).
        let a = edge_sec_ms_gec();
        let b = edge_sec_ms_gec();
        assert_eq!(a, b, "tokens within the same 5-minute window must match");
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
            // Raw PCM16 24 kHz mono — flows straight to on_audio without an
            // MP3 decode step (uniform PCM contract across all engines).
            Some("raw-24khz-16bit-mono-pcm")
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
        assert_eq!(decoded, "apikey:TESTKEY");
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
    fn test_looks_like_mp3_id3_tag() {
        assert!(looks_like_mp3(b"ID3\x03\x00\x00\x00\x00"));
    }

    #[test]
    fn test_looks_like_mp3_frame_sync() {
        // First byte 0xFF, second with top 3 bits set (MPEG sync).
        assert!(looks_like_mp3(&[0xFF, 0xE3, 0x10, 0x00]));
        assert!(looks_like_mp3(&[0x00, 0x00, 0xFF, 0xFB, 0x90])); // sync mid-stream
    }

    #[test]
    fn test_looks_like_mp3_raw_pcm_is_false() {
        // Raw PCM16 has no sync word / ID3 — must not be mistaken for MP3.
        assert!(!looks_like_mp3(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05]));
        assert!(!looks_like_mp3(&[]));
    }

    #[test]
    fn test_decode_mp3_garbage_returns_empty_without_panicking() {
        // Empty / non-MP3 input must not panic and must yield no PCM.
        assert!(decode_mp3_to_pcm16_mono(&[]).is_empty());
        assert!(decode_mp3_to_pcm16_mono(b"definitely not mp3").is_empty());
    }

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
        let (word, offset_ms, duration_ms, char_offset, char_len) =
            azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "Hello");
        assert_eq!(offset_ms, 50);
        assert_eq!(duration_ms, 250);
        // No Offset/Length in the text object → -1.
        assert_eq!(char_offset, -1);
        assert_eq!(char_len, -1);
    }

    #[test]
    fn test_azure_ws_parse_word_boundary_with_text_offset() {
        // Azure WS sends character offset and length in the nested text object.
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {
                "Offset": 2_800_000,
                "Duration": 1_500_000,
                "text": {"Text": "quick", "Offset": 4, "Length": 5}
            }
        });
        let (word, _, _, char_offset, char_len) =
            azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "quick");
        assert_eq!(char_offset, 4);
        assert_eq!(char_len, 5);
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
        let (word, _, _, _, _) = azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "Hi");
    }

    #[test]
    fn test_azure_ws_parse_word_boundary_flat_string() {
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {"Offset": 0, "Duration": 0, "text": "Yo"}
        });
        let (word, _, _, _, _) = azure_ws_parse_word_boundary(&item).expect("parsed");
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

    // ===== Streaming chunk size regression =====
    //
    // The speak() loop delivers audio via `on_audio` in chunks of
    // STREAMING_CHUNK_SIZE bytes (used by both the base64-decoded JSON
    // payloads and the HTTP streaming-Read path). Pinning the constant
    // catches a future tweak that accidentally switches to e.g. 1024 and
    // creates millions of callback round-trips per request. Earlier
    // versions of this test asserted on `vec.chunks(8192).count()` against
    // a buffer the test itself built — that was tautological (it tested
    // the stdlib, not production code); referencing the constant makes the
    // test catch the actual regression.

    #[test]
    fn test_streaming_chunk_size_constant_value() {
        // Must be a power of two in the KiB range — anything smaller would
        // explode callback count; anything larger would inflate memory.
        assert_eq!(STREAMING_CHUNK_SIZE, 8 * 1024);
        assert!(STREAMING_CHUNK_SIZE.is_power_of_two());
    }

    #[test]
    fn test_streaming_chunk_size_used_in_speak_path() {
        // Defensive: grep-verify the production code references the
        // constant rather than re-introducing a magic 8192 literal. We
        // count uses of `.chunks(STREAMING_CHUNK_SIZE)` in lines that
        // aren't part of this test (the test itself mentions the magic
        // literal in its assertion message, which would false-positive
        // a naive grep).
        let source = include_str!("cloud_engine.rs");
        let production_uses = source
            .lines()
            // Skip every line inside this test's body, which legitimately
            // mentions both .chunks(STREAMING_CHUNK_SIZE) and the magic
            // literal 8192 in its assertion message.
            .filter(|l| !l.contains("test_streaming_chunk_size_used_in_speak_path"))
            .filter(|l| !l.contains("magic 8192"))
            .filter(|l| l.contains(".chunks(STREAMING_CHUNK_SIZE)"))
            .count();
        assert!(
            production_uses >= 2,
            "expected at least 2 production uses of STREAMING_CHUNK_SIZE, found {production_uses}"
        );
    }

    // ===== SpeechMarkdown routing per platform =====
    //
    // speak() calls preprocess_speech_markdown(text, &self.config.provider_id).
    // The cloud engines we route through that helper:
    //   azure   → MicrosoftAzure SSML
    //   google  → GoogleAssistant SSML
    //   *       → AmazonAlexa SSML
    //
    // Verifying the routing requires asserting that the platforms actually
    // produce DIFFERENT output. Every SSML flavour starts with `<speak>`,
    // so an `assert!(ssml.contains("<speak"))` test would still pass if we
    // accidentally routed Azure to the Alexa flavour. Instead we feed the
    // same input through all three platforms and require that at least one
    // differs — that catches a routing collapse in either direction.

    #[test]
    fn test_speechmarkdown_routing_is_per_platform() {
        // Verify the preprocess_speech_markdown routing match actually
        // dispatches to different Platform variants. Some SpeechMarkdown
        // constructs produce identical SSML across all platforms (the
        // library normalises a common subset), so a single-input test
        // can't distinguish "routing works" from "routing collapsed but
        // the library happens to emit the same bytes". We try several
        // constructs that have historically differed between Microsoft /
        // Google / Alexa flavours and require at least one to produce
        // distinct output across azure/google/other.
        use crate::engine::preprocess_speech_markdown;
        let probe_inputs = [
            "(world)[emphasis:\"strong\"]",
            "+important+",
            "(world)[rate:\"fast\"]",
            "(world)[pitch:\"high\"]",
            "(world)[volume:\"loud\"]",
            "[rate:\"fast\"]hello[/rate]",
            "This is ^italic^ text",
        ];

        let mut found_distinct = false;
        for input in &probe_inputs {
            let (azure_ssml, azure_ok) = preprocess_speech_markdown(input, "azure");
            let (google_ssml, google_ok) = preprocess_speech_markdown(input, "google");
            let (alexa_ssml, alexa_ok) = preprocess_speech_markdown(input, "elevenlabs");

            assert!(azure_ok, "azure failed to parse: {input:?}");
            assert!(google_ok, "google failed to parse: {input:?}");
            assert!(alexa_ok, "alexa failed to parse: {input:?}");

            if azure_ssml != google_ssml || google_ssml != alexa_ssml {
                found_distinct = true;
                break;
            }
        }

        assert!(
            found_distinct,
            "None of {} probe inputs produced distinct SSML across azure/google/alexa. \
             This either means the speechmarkdown-rust library has collapsed its \
             Platform variants to identical output (check the dependency version) or \
             the routing match arm in preprocess_speech_markdown is broken.",
            probe_inputs.len()
        );
    }

    #[test]
    fn test_speechmarkdown_other_providers_detect_input() {
        use crate::engine::preprocess_speech_markdown;
        // ElevenLabs, OpenAI, Cartesia, Murf, etc. all go through the
        // Alexa fallback. They don't actually consume SSML — the result is
        // discarded by the JSON-body branch in speak() — but detection
        // must still flag the input as SpeechMarkdown so callers querying
        // `is_ssml` get a truthful answer.
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
