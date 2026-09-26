// The split modules resolve shared names through the parent glob.
#![allow(clippy::wildcard_imports)]

use super::*;

/// Qwen cloud TTS (Alibaba Cloud Model Studio / DashScope) via the
/// duplex WebSocket "tts_v2" protocol shared by Qwen-Audio-TTS and
/// CosyVoice models.
///
/// Wire shape (all JSON text frames carry `header.action`/`header.event`;
/// audio arrives as raw binary frames, one after each `sentence-synthesis`
/// event):
///
/// ```text
/// client → run-task       (model, voice, format, rate/pitch/volume, …)
/// server → task-started
/// client → continue-task  (text, ≤ 20 000 chars per message)
/// client → finish-task
/// server → result-generated sentence-begin | sentence-synthesis | sentence-end
/// server → task-finished | task-failed
/// ```
///
/// Audio is requested as raw PCM16LE mono 24 kHz (`format: "pcm"`), so
/// binary frames flow straight through `on_audio` with no decode step.
///
/// Connections are deliberately NOT pooled (unlike Edge/Azure): DashScope
/// tasks are connection-scoped, idle connections auto-close after 60 s,
/// and a `task-failed` socket must be discarded per protocol — pooling
/// would buy little and risk checking in a poisoned socket.
#[cfg(feature = "cloud")]
pub(crate) const QWEN_WS_URL_QWENCLOUD: &str = "wss://maas.qwencloudapi.com/api-ws/v1/inference";

/// Singapore / international Model Studio endpoint.
#[cfg(feature = "cloud")]
pub(crate) const QWEN_WS_URL_INTL: &str = "wss://dashscope-intl.aliyuncs.com/api-ws/v1/inference";

/// Beijing (China mainland) Model Studio endpoint.
#[cfg(feature = "cloud")]
pub(crate) const QWEN_WS_URL_BEIJING: &str = "wss://dashscope.aliyuncs.com/api-ws/v1/inference";

#[cfg(feature = "cloud")]
pub(crate) const QWEN_DEFAULT_MODEL: &str = "qwen-audio-3.0-tts-flash";

#[cfg(feature = "cloud")]
pub(crate) const QWEN_DEFAULT_VOICE: &str = "longanhuan_v3.6";

/// continue-task hard limit per message (protocol caps at 20 000 chars;
/// leave headroom so a chunk never straddles the boundary).
#[cfg(feature = "cloud")]
const QWEN_TEXT_CHUNK_CHARS: usize = 19_000;

/// Resolve the WebSocket inference URL. A `wsUrl` credential wins; then a
/// `region` credential (`qwencloud` | `intl` | `beijing`); otherwise the
/// Qwen Cloud default. An unrecognized `region` is an error rather than a
/// silent fallback (a typo'd "Beijing" would otherwise route to the wrong
/// endpoint and fail with a confusing auth error).
#[cfg(feature = "cloud")]
pub(crate) fn qwen_ws_url(credentials: &HashMap<String, String>) -> TtsResult<String> {
    if let Some(url) = credentials.get("wsUrl").filter(|u| !u.is_empty()) {
        return Ok(url.clone());
    }
    match credentials.get("region").map(String::as_str) {
        None | Some("qwencloud" | "") => Ok(QWEN_WS_URL_QWENCLOUD.into()),
        Some("intl") => Ok(QWEN_WS_URL_INTL.into()),
        Some("beijing") => Ok(QWEN_WS_URL_BEIJING.into()),
        Some(other) => Err(TtsError(format!(
            "qwen: unknown region '{other}' (expected qwencloud, intl, or beijing; \
             or set wsUrl to override the endpoint)"
        ))),
    }
}

/// A server event lifted out of the speak loop so it can be unit-tested
/// with recorded frames.
#[cfg(feature = "cloud")]
#[derive(Debug, PartialEq)]
pub(crate) enum QwenServerEvent {
    /// `task-started` — the client may now send text.
    TaskStarted,
    /// `result-generated` / `sentence-synthesis` — one binary audio frame
    /// follows this event immediately. Carries the sentence index so
    /// audio bytes can be attributed to the right sentence even when
    /// `sentence-end` events arrive late (observed on the live API).
    ///
    /// A missing `sentence.index` parses as 0: if a server variant ever
    /// omitted indices entirely, every frame would attribute to sentence
    /// 0 and later sentences' boundary bases would collapse onto the
    /// total delivered audio. The live API always sends the index
    /// (pinned by tests); if that ever changes, treat this as a protocol
    /// break to fix, not a silent fallback.
    AudioFrame { sentence: u64 },
    /// `result-generated` / `sentence-end` with the word-timestamp array.
    SentenceEnd { sentence: u64, words: Vec<QwenWord> },
    /// `task-finished` — synthesis complete.
    TaskFinished,
    /// `task-failed` — the connection must be closed.
    TaskFailed { code: String, message: String },
    /// Anything else (sentence-begin, unknown events) — ignored.
    Other,
}

/// One word entry from a `sentence-end` payload. `begin_time`/`end_time`
/// are milliseconds from the start of the sentence's audio.
#[cfg(feature = "cloud")]
#[derive(Debug, PartialEq)]
pub(crate) struct QwenWord {
    pub(crate) text: String,
    pub(crate) begin_ms: u64,
    pub(crate) end_ms: u64,
}

/// Accumulates the running audio-time base for per-sentence word
/// timestamps. Word times in `sentence-end` events are relative to their
/// own sentence, so each sentence needs the offset of its start.
///
/// Ground truth is delivered audio: at 24 kHz 16-bit mono, 48 bytes =
/// 1 ms. Every `sentence-synthesis` event carries the sentence index of
/// the binary frame that follows it, so bytes are attributed to the
/// sentence they actually belong to — even when `sentence-end` events
/// arrive after the next sentence's audio has started (observed on the
/// live API; lump-sum attribution at sentence-end time over-advanced
/// later sentences by the overlap).
#[cfg(feature = "cloud")]
#[derive(Debug, Default)]
pub(crate) struct QwenSentenceClock {
    /// PCM bytes delivered per sentence index (attributed via the
    /// sentence-synthesis marker preceding each binary frame).
    bytes_by_sentence: std::collections::HashMap<u64, usize>,
}

#[cfg(feature = "cloud")]
impl QwenSentenceClock {
    /// Attribute `len` PCM bytes to `sentence` (call when a binary frame
    /// arrives, with the index from the preceding sentence-synthesis).
    pub(crate) fn audio_frame(&mut self, sentence: u64, len: usize) {
        *self.bytes_by_sentence.entry(sentence).or_insert(0) += len;
    }

    /// Resolve the base offset (ms) for the finished `sentence`: the
    /// total duration of all lower-indexed sentences.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn sentence_base_ms(&self, sentence: u64) -> u64 {
        let bytes: usize = self
            .bytes_by_sentence
            .iter()
            .filter(|(&idx, _)| idx < sentence)
            .map(|(_, &v)| v)
            .sum();
        (bytes / 48) as u64
    }
}

/// Parse a server text frame into a [`QwenServerEvent`].
#[cfg(feature = "cloud")]
pub(crate) fn qwen_parse_event(frame: &str) -> QwenServerEvent {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(frame) else {
        return QwenServerEvent::Other;
    };
    let header = json.get("header");
    let event = header
        .and_then(|h| h.get("event"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    match event {
        "task-started" => QwenServerEvent::TaskStarted,
        "task-finished" => QwenServerEvent::TaskFinished,
        "task-failed" => QwenServerEvent::TaskFailed {
            code: header
                .and_then(|h| h.get("error_code"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            message: header
                .and_then(|h| h.get("error_message"))
                .and_then(|v| v.as_str())
                .unwrap_or("qwen synthesis failed")
                .to_string(),
        },
        "result-generated" => {
            let output = json.get("payload").and_then(|p| p.get("output"));
            let ty = output
                .and_then(|o| o.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let sentence_index = || {
                output
                    .and_then(|o| o.get("sentence"))
                    .and_then(|s| s.get("index"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default()
            };
            match ty {
                "sentence-synthesis" => QwenServerEvent::AudioFrame {
                    sentence: sentence_index(),
                },
                "sentence-end" => {
                    let words = output
                        .and_then(|o| o.get("sentence"))
                        .and_then(|s| s.get("words"))
                        .and_then(|w| w.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|w| {
                                    let text = w.get("text").and_then(|v| v.as_str())?.to_string();
                                    Some(QwenWord {
                                        text,
                                        begin_ms: w
                                            .get("begin_time")
                                            .and_then(serde_json::Value::as_u64)
                                            .unwrap_or_default(),
                                        end_ms: w
                                            .get("end_time")
                                            .and_then(serde_json::Value::as_u64)
                                            .unwrap_or_default(),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    QwenServerEvent::SentenceEnd {
                        sentence: sentence_index(),
                        words,
                    }
                }
                _ => QwenServerEvent::Other,
            }
        }
        _ => QwenServerEvent::Other,
    }
}

/// Build the `run-task` frame. `word_timestamps` mirrors whether the
/// caller requested boundaries (the API's `word_timestamp_enabled`).
#[cfg(feature = "cloud")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn qwen_run_task_json(
    task_id: &str,
    model: &str,
    voice: &str,
    rate: f32,
    pitch: f32,
    volume: f32,
    word_timestamps: bool,
    instruction: Option<&str>,
) -> serde_json::Value {
    // The wrapper's rate/pitch/volume are 1.0-centred multipliers; the
    // API takes rate/pitch in [0.5, 2.0] and volume in [0, 100] with
    // neutral at 50. Scale volume so 1.0 → 50, 2.0 → 100, 0.0 → 0 (and
    // mute below 0), and clamp the others into range rather than
    // rejecting the caller.
    #[allow(clippy::cast_possible_truncation)]
    let volume_int = (volume.clamp(0.0, 2.0) * 50.0).round() as i64;
    let mut parameters = serde_json::json!({
        "text_type": "PlainText",
        "voice": voice,
        "format": "pcm",
        "sample_rate": 24_000,
        "volume": volume_int,
        "rate": rate.clamp(0.5, 2.0),
        "pitch": pitch.clamp(0.5, 2.0),
        "word_timestamp_enabled": word_timestamps,
    });
    if let Some(instr) = instruction.filter(|s| !s.is_empty()) {
        parameters["instruction"] = serde_json::Value::String(instr.to_string());
    }
    serde_json::json!({
        "header": {
            "action": "run-task",
            "task_id": task_id,
            "streaming": "duplex",
        },
        "payload": {
            "task_group": "audio",
            "task": "tts",
            "function": "SpeechSynthesizer",
            "model": model,
            "parameters": parameters,
            "input": {},
        },
    })
}

/// Split text into ≤ `QWEN_TEXT_CHUNK_CHARS` pieces on char boundaries
/// (the protocol caps a single continue-task at 20 000 characters).
#[cfg(feature = "cloud")]
pub(crate) fn qwen_chunk_text(text: &str) -> Vec<&str> {
    if text.chars().count() <= QWEN_TEXT_CHUNK_CHARS {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        // One step to the byte offset of the (cap+1)-th char — i.e. the
        // char boundary directly after the cap-th char.
        let mut end = text[start..]
            .char_indices()
            .nth(QWEN_TEXT_CHUNK_CHARS)
            .map_or(text.len(), |(i, _)| start + i);
        // Guard against pathological slicing (cannot happen with valid
        // UTF-8, but keeps the loop obviously terminating).
        end = end.clamp(start + 1, text.len());
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

/// Callback aliases so the speak signature stays readable.
#[cfg(feature = "cloud")]
pub(crate) type QwenAudioFn<'a> = &'a mut dyn FnMut(&[u8]);
#[cfg(feature = "cloud")]
pub(crate) type QwenBoundaryFn<'a> = &'a mut dyn FnMut(&str, f32, f32, i32, i32, bool);

/// Run one full duplex synthesis session and deliver the audio.
///
/// Returns the number of PCM bytes delivered (used by the caller to
/// detect "completed with no audio" as an error). On `task-failed` the
/// socket is dropped (the protocol forbids reusing a failed connection).
///
/// Audio is requested as PCM16LE mono 24 kHz and each binary frame is
/// delivered to `on_audio` as it arrives (real-time streaming preserved).
/// When `on_boundary` is present the task is started with
/// `word_timestamp_enabled` and `sentence-end` word arrays are mapped to
/// the caller's text via the shared word matcher (measured timings,
/// `estimated = false`). Voices without timestamp support still synthesise
/// but may report no boundaries.
#[cfg(feature = "cloud")]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
#[allow(clippy::cast_precision_loss)]
pub(crate) fn qwen_speak_ws(
    text: &str,
    voice: &str,
    rate: f32,
    pitch: f32,
    volume: f32,
    api_key: &str,
    credentials: &HashMap<String, String>,
    model: &str,
    instruction: Option<&str>,
    mut on_audio: Option<QwenAudioFn<'_>>,
    mut on_boundary: Option<QwenBoundaryFn<'_>>,
    boundary_search_text: &str,
) -> TtsResult<usize> {
    use tungstenite::client::IntoClientRequest;
    use tungstenite::{connect, Message};
    use url::Url;

    if text.is_empty() {
        // An empty continue-task would fail deep in the service with a
        // generic error; refuse it up front with a diagnosable message.
        return Err(TtsError("qwen: refusing to synthesize empty text".into()));
    }

    let ws_url_str = qwen_ws_url(credentials)?;
    let ws_url = Url::parse(&ws_url_str).map_err(|e| TtsError(format!("Invalid WS URL: {e}")))?;

    // Auth is bound at the handshake only (subsequent task frames carry
    // no key). Raw protocol examples use a lowercase "bearer".
    let mut req = ws_url
        .as_str()
        .into_client_request()
        .map_err(|e| TtsError(format!("WS request build: {e}")))?;
    {
        let h = req.headers_mut();
        let auth = format!("bearer {api_key}")
            .parse()
            .map_err(|e| TtsError(format!("Authorization header: {e}")))?;
        h.insert("Authorization", auth);
        let ua = "rust-tts-wrapper"
            .parse()
            .map_err(|e| TtsError(format!("User-Agent header: {e}")))?;
        h.insert("User-Agent", ua);
    }
    let mut socket = connect(req)
        .map_err(|e| TtsError(format!("WS connect error: {e}")))?
        .0;

    // DashScope SDKs emit 32-hex-no-dash task IDs; match that shape (the
    // Azure WS branch uses .simple() for the same reason).
    let task_id = Uuid::new_v4().simple().to_string();
    let send = |socket: &mut tungstenite::WebSocket<
        tungstenite::stream::MaybeTlsStream<std::net::TcpStream>,
    >,
                value: &serde_json::Value,
                what: &str|
     -> TtsResult<()> {
        socket
            .send(Message::Text(value.to_string().into()))
            .map_err(|e| TtsError(format!("WS {what} send error: {e}")))
    };

    let run_task = qwen_run_task_json(
        &task_id,
        model,
        voice,
        rate,
        pitch,
        volume,
        on_boundary.is_some(),
        instruction,
    );
    send(&mut socket, &run_task, "run-task")?;

    // Wait for task-started before sending text (protocol ordering).
    // Idle timeout: reset on every message so a healthy long synthesis
    // never trips it — only a stalled service does. from_secs (not the
    // unstable from_mins) for stable-rustc portability, matching the
    // Azure WS branch.
    #[allow(clippy::duration_suboptimal_units)]
    let idle_limit = || std::time::Duration::from_secs(120);
    let mut deadline = std::time::Instant::now() + idle_limit();
    loop {
        if std::time::Instant::now() > deadline {
            let _ = socket.close(None);
            return Err(TtsError("qwen WebSocket task start timed out".into()));
        }
        match socket.read() {
            Ok(Message::Text(t)) => match qwen_parse_event(t.as_str()) {
                QwenServerEvent::TaskStarted => break,
                QwenServerEvent::TaskFailed { code, message } => {
                    let _ = socket.close(None);
                    return Err(TtsError(format!("qwen task failed: {message} ({code})")));
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => return Err(TtsError(format!("WS receive error: {e}"))),
        }
        deadline = std::time::Instant::now() + idle_limit();
    }

    for chunk in qwen_chunk_text(text) {
        let continue_task = serde_json::json!({
            "header": {
                "action": "continue-task",
                "task_id": task_id,
                "streaming": "duplex",
            },
            "payload": { "input": { "text": chunk } },
        });
        send(&mut socket, &continue_task, "continue-task")?;
    }
    let finish_task = serde_json::json!({
        "header": {
            "action": "finish-task",
            "task_id": task_id,
            "streaming": "duplex",
        },
        "payload": { "input": {} },
    });
    send(&mut socket, &finish_task, "finish-task")?;

    // Word offsets in sentence-end events are relative to the sentence,
    // not the caller's text — recover positions with the shared matcher
    // (exact → case/accent-insensitive → hold-last), like the
    // ElevenLabs/Google paths.
    let mut search = crate::word_search::WordSearch::new(boundary_search_text);
    // Per-sentence word times need each sentence's audio-time base —
    // see QwenSentenceClock for why delivered bytes are the ground truth.
    let mut clock = QwenSentenceClock::default();
    let mut audio_bytes = 0usize;
    // Index of the sentence the next binary frame belongs to (set by
    // sentence-synthesis markers).
    let mut pending_frame_sentence: u64 = 0;

    loop {
        if std::time::Instant::now() > deadline {
            let _ = socket.close(None);
            return Err(TtsError("qwen WebSocket synthesis timed out".into()));
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(
                tungstenite::error::Error::ConnectionClosed
                | tungstenite::error::Error::AlreadyClosed,
            ) => break,
            Err(e) => return Err(TtsError(format!("WS receive error: {e}"))),
        };
        // Healthy traffic pushes the idle deadline out; only a stalled
        // service (or dead connection) trips it.
        deadline = std::time::Instant::now() + idle_limit();
        match msg {
            Message::Text(t) => match qwen_parse_event(t.as_str()) {
                QwenServerEvent::TaskFinished => {
                    let _ = socket.close(None);
                    return Ok(audio_bytes);
                }
                QwenServerEvent::TaskFailed { code, message } => {
                    let _ = socket.close(None);
                    return Err(TtsError(format!("qwen task failed: {message} ({code})")));
                }
                QwenServerEvent::AudioFrame { sentence } => {
                    // The binary frame that immediately follows belongs
                    // to this sentence.
                    pending_frame_sentence = sentence;
                }
                QwenServerEvent::SentenceEnd { sentence, words } => {
                    let sentence_start_ms = clock.sentence_base_ms(sentence);
                    if let Some(cb) = on_boundary.as_mut() {
                        for w in &words {
                            let (char_offset, char_len) = search.find_next(&w.text);
                            cb(
                                &w.text,
                                (sentence_start_ms + w.begin_ms) as f32 / 1000.0,
                                (sentence_start_ms + w.end_ms) as f32 / 1000.0,
                                char_offset.max(0),
                                char_len,
                                false,
                            );
                        }
                    }
                }
                _ => {}
            },
            // One binary frame follows each sentence-synthesis event; the
            // bytes are raw PCM16LE mono 24 kHz (format: "pcm").
            Message::Binary(b) if !b.is_empty() => {
                audio_bytes += b.len();
                clock.audio_frame(pending_frame_sentence, b.len());
                if let Some(cb) = on_audio.as_mut() {
                    cb(&b);
                }
            }
            _ => {}
        }
    }

    // The loop only breaks on ConnectionClosed/AlreadyClosed: the server
    // dropped the socket without task-finished (network drop / protocol
    // violation). Truncated audio is an error, not a success — task-
    // finished returns Ok directly above.
    Err(TtsError(format!(
        "qwen connection closed before task-finished ({audio_bytes} audio bytes delivered)"
    )))
}
