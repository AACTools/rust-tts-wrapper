use super::*;

pub(crate) type VisemeFn = Box<dyn FnMut(i32, f32)>;

thread_local! {
    pub(crate) static VISEME_CB: std::cell::RefCell<Option<VisemeFn>> =
        const { std::cell::RefCell::new(None) };
}

/// Set the thread-local viseme callback. Called by the FFI layer before speak().
pub fn set_viseme_callback(cb: Option<Box<dyn FnMut(i32, f32)>>) {
    VISEME_CB.with(|cell| *cell.borrow_mut() = cb);
}

/// A TTS engine that synthesises speech by calling a cloud HTTP API.
#[derive(Debug)]
pub struct CloudEngine {
    pub(crate) config: CloudConfig,
    pub(crate) api_key: String,
    pub(crate) credentials: HashMap<String, String>,
    pub(crate) client: reqwest::blocking::Client,
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
        _on_mark: Option<crate::engine::OnMarkCallback>,
    ) -> TtsResult<()> {
        // SpeechMarkdown platform selector: ElevenLabs needs the dialect
        // that matches the requested model (v3 audio tags vs pre-v3
        // <break> markup) — the other dialect gets read aloud or ignored.
        // The effective model honours an extra_body model_id override:
        // the dialect must follow what is actually sent, not just
        // model_default.
        let smd_platform =
            elevenlabs_smd_platform(&self.config.provider_id, effective_model(&self.config));
        // Caller-facing text for word-boundary offset mapping: when
        // SpeechMarkdown was reformatted (rather than passed through or
        // converted to SSML), injected ElevenLabs tags shift offsets, so
        // search the user's original input — the spoken words live there.
        let user_text = text;
        let (original_text, is_ssml) = preprocess_speech_markdown(text, smd_platform);

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
                    // Clone: original_text is still needed for boundary
                    // text mapping below.
                    text = original_text.clone();
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
                    // ElevenLabs and Gemini parse no SSML: translate into
                    // the model-matched dialect (breaks, whisper, styles
                    // survive) rather than stripping to plain text.
                    #[cfg(feature = "speechmarkdown")]
                    if self.config.provider_id == "elevenlabs"
                        || self.config.provider_id == "gemini"
                    {
                        text = ssml_to_dialect(&original_text, smd_platform)
                            .unwrap_or_else(|| crate::engine::strip_ssml_to_text(&original_text));
                    } else {
                        text = crate::engine::strip_ssml_to_text(&original_text);
                    }
                    #[cfg(not(feature = "speechmarkdown"))]
                    {
                        text = crate::engine::strip_ssml_to_text(&original_text);
                    }
                }
            }
        } else {
            google_ssml_override = None;
            // Clone: original_text stays available for boundary mapping.
            text = original_text.clone();
        }

        // Boundary word search target (see `user_text` above): plain or
        // dialect-reformatted input maps against what the caller passed;
        // SSML paths keep searching the processed string (previously
        // existing behavior). Exception: ElevenLabs SSML input was
        // translated into prompt markup — the dialect text (and its tag
        // fragments) must not leak into boundary words or offset
        // mapping; search the plain spoken text instead.
        let plain_spoken;
        let boundary_search_text: &str = if is_ssml
            && (self.config.provider_id == "elevenlabs" || self.config.provider_id == "gemini")
        {
            plain_spoken = crate::engine::strip_ssml_to_text(&original_text);
            plain_spoken.as_str()
        } else if is_ssml {
            text.as_str()
        } else {
            user_text
        };

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
            // Total synthesis audio received on the wire. A turn that ends
            // cleanly (turn.end, no error) with zero audio means the request
            // was malformed in a way the service doesn't report — the classic
            // case being a bare <speak> envelope — and must surface as an
            // error, not a silent success.
            let mut ws_audio_bytes = 0usize;
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

            // Send SSML.
            // When is_ssml=true (tts_speak_ssml), the text IS already SSML —
            // send it directly without build_azure_ssml wrapping (which would
            // XML-escape the tags). If the SSML lacks a <voice> tag but the
            // caller set one via tts_set_voice, inject it so the voice takes
            // effect. The envelope is completed first (a bare <speak> is
            // accepted but synthesises zero audio) and unsupported position
            // elements are dropped (<mark> on both; <bookmark> — Azure's own
            // documented element — on Edge only, where it also zeroes audio).
            let is_edge = self.config.provider_id == "edge";
            let ssml = if is_ssml {
                inject_voice_if_missing(
                    &strip_unsupported_marks(
                        &normalize_ssml_envelope(&text, &voice_to_use),
                        is_edge,
                    ),
                    &voice_to_use,
                )
            } else {
                build_azure_ssml(&text, &voice_to_use, rate, pitch, volume)
            };
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
            // from_secs (not the unstable const from_mins) for stable-rustc portability.
            #[allow(clippy::duration_suboptimal_units)]
            let ws_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);

            // Edge returns MP3 frames (raw PCM isn't supported on its free
            // endpoint). A dedicated decode thread pulls frames off a pipe
            // and hands PCM chunks back over a channel, so the WS loop below
            // never blocks on decoding (it must keep reading messages to
            // feed the pipe). Azure (response_is_pcm) streams PCM frames
            // straight through with no decode step.
            let mut edge_pipe: Option<Arc<SharedPipe>> = None;
            let mut edge_handle: Option<std::thread::JoinHandle<()>> = None;
            let mut edge_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>> = None;
            let deliver_edge_audio =
                |rx: &std::sync::mpsc::Receiver<Vec<u8>>,
                 cb: &mut crate::engine::OnAudioCallback| {
                    while let Ok(chunk) = rx.try_recv() {
                        if !chunk.is_empty() {
                            cb(&chunk);
                        }
                    }
                };

            // Tracks the cumulative character offset within the source text as
            // Azure WS word-boundary events arrive. When Azure doesn't provide
            // text.Offset, we compute it by accumulating word lengths + spaces.
            let mut ws_cumulative_offset = 0usize;

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
                                                        // Azure doesn't always send
                                                        // text.Offset (the field is absent,
                                                        // not -1). When missing, compute
                                                        // the offset by accumulating word
                                                        // lengths + spaces as boundaries
                                                        // arrive. This gives plain-text-
                                                        // relative offsets without depending
                                                        // on the SSML structure.
                                                        let final_offset = if char_offset >= 0 {
                                                            char_offset
                                                        } else {
                                                            ws_cumulative_offset as i32
                                                        };
                                                        let final_len = if char_len >= 0 {
                                                            char_len
                                                        } else {
                                                            // Bytes, matching
                                                            // ws_cumulative_offset's byte
                                                            // arithmetic above.
                                                            word.len() as i32
                                                        };
                                                        // Advance the running offset past
                                                        // this word + the space that follows.
                                                        // TODO: assumes single-space
                                                        // separation; punctuation and
                                                        // double spaces make the
                                                        // cumulative estimate drift.
                                                        ws_cumulative_offset += word.len() + 1;
                                                        #[allow(clippy::cast_precision_loss)]
                                                        cb(
                                                            word,
                                                            offset_ms as f32 / 1000.0,
                                                            (offset_ms + duration_ms) as f32
                                                                / 1000.0,
                                                            final_offset,
                                                            final_len,
                                                            false,
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
                            ws_audio_bytes += audio.len();
                            if self.config.response_is_pcm {
                                // Azure raw-PCM frames — deliver straight through.
                                if let Some(cb) = on_audio.as_mut() {
                                    cb(audio);
                                }
                            } else if let Some(cb) = on_audio.as_mut() {
                                // Edge MP3 frames — start the decode thread on
                                // first audio, push the frames, and deliver
                                // whatever PCM has come back so far.
                                if edge_pipe.is_none() {
                                    let pipe = Arc::new(SharedPipe::new());
                                    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
                                    let dec_pipe = Arc::clone(&pipe);
                                    edge_handle = Some(
                                        std::thread::Builder::new()
                                            .name("edge-mp3-decode".into())
                                            .spawn(move || {
                                                let mut dec = IncrementalDecoder::new(dec_pipe);
                                                loop {
                                                    match dec.next_chunk() {
                                                        Ok(Some(chunk)) => {
                                                            if tx.send(chunk).is_err() {
                                                                break;
                                                            }
                                                        }
                                                        Ok(None) => break,
                                                        Err(e) => {
                                                            eprintln!(
                                                                "rust-tts-wrapper: \
                                                                 edge decode error: {e}"
                                                            );
                                                            break;
                                                        }
                                                    }
                                                }
                                            })
                                            .expect("spawn edge decode thread"),
                                    );
                                    edge_rx = Some(rx);
                                    edge_pipe = Some(pipe);
                                }
                                if let Some(pipe) = edge_pipe.as_ref() {
                                    pipe.push(audio);
                                }
                                if let Some(rx) = edge_rx.as_ref() {
                                    deliver_edge_audio(rx, cb);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Edge: signal EOF so the decode thread releases its tail, join
            // it, then deliver whatever PCM is still queued.
            if let Some(pipe) = edge_pipe.as_ref() {
                pipe.finish();
            }
            if let Some(handle) = edge_handle.take() {
                let _ = handle.join();
            }
            if let (Some(rx), Some(cb)) = (edge_rx.as_ref(), on_audio.as_mut()) {
                deliver_edge_audio(rx, cb);
            }

            // Clean turn.end → return the still-open socket to the pool for the
            // next utterance. Otherwise (timeout / server-closed / error) the
            // socket is dropped and discarded, never pooled.
            if clean_finish {
                ws_checkin(ws_url_str, socket);
            }
            if ws_audio_bytes == 0 {
                return Err(TtsError(format!(
                    "{} synthesis completed with no audio (malformed SSML envelope?)",
                    self.config.provider_id
                )));
            }
            return Ok(());
        }

        // ElevenLabs word timing comes from the /with-timestamps endpoint
        // variant. Model support for that variant varies (not documented
        // for eleven_v3) — if it rejects the request we degrade to a
        // plain synthesis and estimated boundaries below rather than
        // failing every boundary-requesting call on that model.
        let wants_timestamps = self.config.provider_id == "elevenlabs" && on_boundary.is_some();

        // Build and send the synthesis request. A closure so the
        // with-timestamps attempt can be retried without the suffix.
        let send_synthesis = |synth_url: &str| -> Result<reqwest::blocking::Response, TtsError> {
            let mut req = self.client.post(synth_url);

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
                // Azure: send SSML XML body. When is_ssml=true, the text is
                // already SSML — send it directly (don't escape/wrap with
                // build_azure_ssml). Inject voice if the SSML lacks a <voice>
                // tag, after completing the envelope and dropping unsupported
                // <mark> elements (Azure accepts its documented <bookmark>
                // here, so bookmarks are kept on this path).
                let ssml = if is_ssml {
                    inject_voice_if_missing(
                        &strip_unsupported_marks(
                            &normalize_ssml_envelope(&text, &voice_to_use),
                            false,
                        ),
                        &voice_to_use,
                    )
                } else {
                    build_azure_ssml(&text, &voice_to_use, rate, pitch, volume)
                };
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
            } else if self.config.provider_id == "gemini" {
                // Gemini Interactions API: turn-based JSON body with
                // speech_metadata style annotations.
                let body = build_gemini_request(
                    &text,
                    &voice_to_use,
                    rate,
                    pitch,
                    volume,
                    effective_model(&self.config),
                    self.credentials.get("style").map(String::as_str),
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
                // ElevenLabs: map the wrapper's rate multiplier (1.0 = normal)
                // onto the deterministic voice_settings.speed API parameter
                // (valid range 0.7–1.2; clamped). Only sent for explicit
                // non-default rates. pitch/volume have no API equivalent
                // (v3 models: use audio tags). Inserted before extra_body so
                // a config-supplied voice_settings object (stability,
                // similarity, …) takes precedence over the derived one.
                if self.config.provider_id == "elevenlabs"
                    && rate > 0.0
                    && (rate - 1.0).abs() > f32::EPSILON
                    && !self.config.extra_body.contains_key("voice_settings")
                {
                    let speed = rate.clamp(0.7, 1.2);
                    body.insert(
                        "voice_settings".to_string(),
                        serde_json::json!({ "speed": speed }),
                    );
                }
                for (k, v) in &self.config.extra_body {
                    body.insert(k.clone(), v.clone());
                }
                req = req.json(&serde_json::Value::Object(body));
                req.send()
            };
            resp.map_err(|e| TtsError(format!("HTTP error: {e}")))
        };

        let mut synth_url = self.config.synth_url.clone();
        if wants_timestamps {
            synth_url.push_str("/with-timestamps");
        }
        let mut resp = send_synthesis(&synth_url)?;

        let mut timestamps_degraded = false;
        if wants_timestamps && !resp.status().is_success() {
            // The /with-timestamps variant was rejected (likely a model
            // that doesn't support it). Drop the suffix and fall back to
            // streamed audio + estimated boundaries. A genuine failure
            // (auth, quota, bad voice) fails the retry too and surfaces
            // its error there.
            timestamps_degraded = true;
            resp = send_synthesis(&self.config.synth_url)?;
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().unwrap_or_default();
            return Err(TtsError(format!("API error {status}: {body_text}")));
        }

        // Total audio delivered for this utterance. A 2xx response with no
        // audio at all is a failure (malformed request the service didn't
        // reject, empty synthesis, …) — reported as an error rather than a
        // silent success.
        let mut audio_total = 0usize;

        // The with-timestamps response is one JSON document (base64 audio
        // + character alignment). When the variant was rejected above, the
        // retry returned a plain streamed body — handled by the streaming
        // branch below with estimated boundaries.
        if wants_timestamps && !timestamps_degraded {
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
                audio_total += pcm.len();
                if let Some(cb) = on_audio.as_mut() {
                    for chunk in pcm.chunks(STREAMING_CHUNK_SIZE) {
                        cb(chunk);
                    }
                }
            }

            if let Some(cb) = on_boundary.as_mut() {
                if let Some(alignment) = json.get("alignment").and_then(|v| v.as_object()) {
                    // Word positions via the shared matcher: exact →
                    // case/accent-insensitive → hold-last (offsets are
                    // byte-based; a held miss reports length -1).
                    let mut search = crate::word_search::WordSearch::new(boundary_search_text);
                    for (word, start, end) in parse_elevenlabs_alignment(alignment) {
                        let (offset, len) = search.find_next(&word);
                        cb(&word, start, end, offset.max(0), len, false);
                    }
                }
            }
        } else if self.config.provider_id == "gemini" {
            // Gemini Interactions API: one JSON document whose audio block
            // is base64 WAV (PCM16, 24 kHz by default). Decode and deliver
            // as uniform PCM; boundaries are estimates scaled to the actual
            // delivered duration (the API exposes no timestamps).
            let resp_text = resp
                .text()
                .map_err(|e| TtsError(format!("Read error: {e}")))?;
            let json: serde_json::Value = serde_json::from_str(&resp_text)
                .map_err(|e| TtsError(format!("JSON parse: {e}")))?;

            match parse_gemini_interaction_audio(&json) {
                GeminiAudioBlock::Present(wav) => {
                    let sample_rate = wav_sample_rate(&wav);
                    let pcm = decode_audio_to_pcm16_mono(&wav, "wav");
                    if pcm.is_empty() {
                        // An audio block was present but symphonia could
                        // not probe/decode it — an error, not a silent
                        // success.
                        return Err(TtsError(format!(
                            "gemini audio block failed to decode ({} wav bytes)",
                            wav.len()
                        )));
                    }
                    audio_total += pcm.len();
                    if let Some(cb) = on_audio.as_mut() {
                        for chunk in pcm.chunks(STREAMING_CHUNK_SIZE) {
                            cb(chunk);
                        }
                    }
                    if let Some(cb) = on_boundary.as_mut() {
                        fire_scaled_estimates(cb, boundary_search_text, &pcm, sample_rate);
                    }
                }
                GeminiAudioBlock::Corrupt => {
                    return Err(TtsError(
                        "gemini audio block base64 payload was corrupt".into(),
                    ));
                }
                GeminiAudioBlock::Absent => {
                    // 2xx without an audio block: a safety refusal, a
                    // filtered prompt, or an in-band error. Surface
                    // whatever detail the interaction carries instead of
                    // a bare "no audio".
                    let detail = json
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .map_or_else(String::new, |m| format!(": {m}"));
                    return Err(TtsError(format!(
                        "gemini synthesis returned no audio{detail}"
                    )));
                }
            }
        } else if self.config.provider_id == "google"
            && (on_boundary.is_some() || on_audio.is_some())
        {
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
                audio_total += pcm.len();
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
                    // Google's timepoints carry no text positions: recover
                    // them with the shared matcher (byte-true, hold-last).
                    let mut search = crate::word_search::WordSearch::new(&text);
                    for b in &boundaries {
                        let (char_offset, char_len) = search.find_next(&b.text);
                        #[allow(clippy::cast_precision_loss)]
                        cb(
                            &b.text,
                            b.offset as f32 / 1000.0,
                            (b.offset + b.duration) as f32 / 1000.0,
                            char_offset.max(0),
                            char_len,
                            false,
                        );
                    }
                } else {
                    let estimated = estimate_word_boundaries(&text);
                    let mut search = crate::word_search::WordSearch::new(&text);
                    for b in &estimated {
                        let (char_offset, char_len) = search.find_next(&b.text);
                        #[allow(clippy::cast_precision_loss)]
                        cb(
                            &b.text,
                            b.offset as f32 / 1000.0,
                            (b.offset + b.duration) as f32 / 1000.0,
                            char_offset.max(0),
                            char_len,
                            false,
                        );
                    }
                }
            }
        } else if on_audio.is_some() || (timestamps_degraded && on_boundary.is_some()) {
            // Most providers respond with an MP3 body (OpenAI, ElevenLabs,
            // Deepgram, Watson, …); a few return raw PCM natively (Azure via
            // X-Microsoft-OutputFormat, Cartesia). Stream the body as it
            // downloads, decoding MP3 → PCM16 mono incrementally so audio
            // reaches on_audio before the response completes.
            //
            // Estimated word boundaries fire progressively, anchored to
            // delivered audio, instead of all-at-once afterwards. The plan
            // is built from the caller-facing text so formatter-injected
            // tags are not estimated as words (user-authored markup in
            // the SpeechMarkdown source still is — the estimator has no
            // markup filter). Also entered for a boundaries-only request
            // when the timestamps variant degraded — otherwise those
            // callers would get audio but no boundaries at all.
            let plan = on_boundary
                .is_some()
                .then(|| EstimatePlan::build(boundary_search_text));
            let mut on_event = |ev: StreamEvt<'_>| match ev {
                StreamEvt::Audio(bytes) => {
                    audio_total += bytes.len();
                    if let Some(cb) = on_audio.as_mut() {
                        cb(bytes);
                    }
                }
                StreamEvt::Boundary(word, start, end, offset, len) => {
                    if let Some(bcb) = on_boundary.as_mut() {
                        bcb(word, start, end, offset, len, false);
                    }
                }
            };
            stream_body_to_on_audio(
                resp,
                self.config.response_is_pcm,
                // Raw-PCM providers here (Azure, Cartesia) are pinned to
                // 24 kHz in their CloudConfigs.
                24_000,
                plan,
                &mut on_event,
            )
            .map_err(TtsError)?;
        } else {
            let audio_bytes = resp
                .bytes()
                .map_err(|e| TtsError(format!("Read error: {e}")))?;
            audio_total += audio_bytes.len();
        }
        if audio_total == 0 {
            return Err(TtsError(format!(
                "{} synthesis returned no audio",
                self.config.provider_id
            )));
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
        on_mark: Option<crate::engine::OnMarkCallback>,
    ) -> TtsResult<()> {
        self.speak(
            text,
            voice,
            rate,
            pitch,
            volume,
            on_audio,
            on_boundary,
            on_mark,
        )
    }

    fn stop(&self) -> TtsResult<()> {
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        // Engines that have no voice-list endpoint return a static list.
        if let Some(voices) = static_voices(&self.config.provider_id) {
            return Ok(voices);
        }

        let Some(ref voices_url) = self.config.voices_url else {
            return Ok(vec![]);
        };

        // reqwest::blocking::Client::send() internally creates a tokio
        // runtime on the calling thread. When the caller is a managed
        // runtime (.NET, Python) that has already set up threading state
        // on the current thread, this causes a native access violation
        // (0xc0000005 on Windows). Running the HTTP call on a dedicated
        // OS thread gives us a clean thread state with no conflicts.
        let url = voices_url.clone();
        let auth_header = self.config.auth_header.clone();
        let auth_value = if self.config.auth_header.is_empty() {
            String::new()
        } else {
            format!("{}{}", self.config.auth_prefix, self.api_key)
        };
        let client = self.client.clone();

        let handle = std::thread::Builder::new()
            .name("tts-voice-list".into())
            .spawn(move || -> TtsResult<serde_json::Value> {
                let mut req = client.get(url.as_str());
                if !auth_header.is_empty() {
                    req = req.header(&auth_header, auth_value);
                }
                let resp = req
                    .send()
                    .map_err(|e| TtsError(format!("Voice list HTTP error: {e}")))?;
                if !resp.status().is_success() {
                    return Ok(serde_json::Value::Array(vec![]));
                }
                resp.json::<serde_json::Value>()
                    .map_err(|e| TtsError(format!("Voice list parse error: {e}")))
            })
            .map_err(|e| TtsError(format!("Failed to spawn voice-list thread: {e}")))?;

        let json = handle
            .join()
            .map_err(|_| TtsError("Voice-list thread panicked".into()))??;

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
            "gemini" => json
                .get("voices")
                .and_then(|v| v.as_array())
                .map_or_else(|| Ok(vec![]), |arr| Ok(map_gemini_voices(arr))),
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
            "gemini" => "gemini",
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
