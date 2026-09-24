#![allow(clippy::wildcard_imports)] // shared-import pattern for the split modules
use super::*;

/// Base64 encode for auth tokens.
pub(crate) fn base64_encode(data: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data.as_bytes())
}

/// If `ssml` doesn't contain a `<voice>` tag, inject one with the given voice
/// name so that `tts_set_voice` takes effect when using `tts_speak_ssml`.
/// The SSML is otherwise passed through unchanged.
#[cfg(feature = "cloud")]
pub(crate) fn inject_voice_if_missing(ssml: &str, voice: &str) -> String {
    if ssml.contains("<voice") || voice.is_empty() {
        return ssml.to_string();
    }
    // Inject <voice name='...'> right after the opening <speak ...> tag,
    // and </voice> right before the closing </speak> tag.
    let voice_tag = format!("<voice name='{voice}'>");
    if let Some(close_idx) = ssml.find('>') {
        // Check this is the <speak> tag, not something else.
        let tag_start = &ssml[..=close_idx];
        if tag_start
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("<speak")
        {
            let insert_at = close_idx + 1;
            let (before, after) = ssml.split_at(insert_at);
            // Insert </voice> before </speak> in the "after" part.
            let after_with_close = if let Some(pos) = after.rfind("</speak") {
                let (content, close) = after.split_at(pos);
                format!("{content}</voice>{close}")
            } else {
                format!("{after}</voice>")
            };
            return format!("{before}{voice_tag}{after_with_close}");
        }
    }
    ssml.to_string()
}

/// The SSML 1.0 synthesis namespace (`<speak xmlns=…>`).
pub(crate) const SSML_XMLNS: &str = "http://www.w3.org/2001/10/synthesis";

/// Derive a BCP-47 language tag from an Azure/Edge voice name
/// (`en-GB-SoniaNeural` → `en-GB`), defaulting to `en-US` when the name
/// doesn't look like a locale.
pub(crate) fn voice_lang(voice: &str) -> String {
    let head: String = voice.chars().take(5).collect();
    let chars: Vec<char> = head.chars().collect();
    let locale_like = chars.len() == 5
        && chars[2] == '-'
        && [0, 1, 3, 4].iter().all(|&i| chars[i].is_ascii_alphabetic());
    if locale_like {
        head
    } else {
        "en-US".to_string()
    }
}

/// Complete an SSML document's `<speak>` envelope with the attributes
/// Azure/Edge require: `version`, `xmlns` and `xml:lang`.
///
/// A bare `<speak>` — exactly what speech-dispatcher's index-marking
/// wrapper produces, and common from other SSML emitters — is *accepted*
/// by the service: the turn completes with `turn.end` and no error, but
/// **zero audio is synthesised**. Filling in the missing attributes
/// before the request goes out makes such documents speak. Present
/// attributes and the rest of the document are passed through verbatim;
/// plain text (no `<speak` envelope) is returned unchanged.
#[cfg(feature = "cloud")]
pub(crate) fn normalize_ssml_envelope(ssml: &str, voice: &str) -> String {
    let trimmed = ssml.trim_start();
    if !trimmed.to_ascii_lowercase().starts_with("<speak") {
        return ssml.to_string();
    }
    let Some(tag_end) = trimmed.find('>') else {
        return ssml.to_string(); // unterminated tag — leave alone
    };
    let inner = trimmed[..=tag_end]
        .strip_prefix('<')
        .and_then(|t| t.strip_suffix('>'))
        .unwrap_or("");
    // Attribute names present in the tag (name only, up to `=`).
    let mut has_version = false;
    let mut has_xmlns = false;
    let mut has_lang = false;
    for attr in inner.split_whitespace().skip(1) {
        match attr
            .split('=')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "version" => has_version = true,
            "xmlns" | "xmlns:xmlns" => has_xmlns = true,
            "xml:lang" => has_lang = true,
            _ => {}
        }
    }
    if has_version && has_xmlns && has_lang {
        return ssml.to_string(); // nothing to do
    }
    let mut words = inner.split_whitespace();
    // XML tag names are case-sensitive: keep the original spelling so the
    // opening tag still matches a `</SPEAK>` close.
    let first = words.next().unwrap_or("speak");
    let self_closing = first.ends_with('/');
    let name = first.trim_end_matches('/');
    let mut open = format!("<{name}");
    // Keep custom attributes, dropping a trailing self-closing slash.
    let attrs = words.collect::<Vec<_>>().join(" ");
    let self_closing = self_closing || attrs.ends_with('/');
    let attrs = attrs.trim_end_matches('/');
    if !attrs.is_empty() {
        open.push(' ');
        open.push_str(attrs);
    }
    if !has_version {
        open.push_str(" version=\"1.0\"");
    }
    if !has_xmlns {
        open.push_str(" xmlns=\"");
        open.push_str(SSML_XMLNS);
        open.push('"');
    }
    if !has_lang {
        open.push_str(" xml:lang=\"");
        open.push_str(&voice_lang(voice));
        open.push('"');
    }
    if self_closing {
        open.push('/');
    }
    open.push('>');
    let lead = &ssml[..ssml.len() - trimmed.len()];
    format!("{lead}{open}{}", &trimmed[tag_end + 1..])
}

/// Remove elements the target endpoint silently refuses, from an SSML
/// document bound for Azure/Edge.
///
/// Both services lack the W3C SSML `<mark>` element — an utterance
/// containing one synthesises **zero audio**, with no error from the
/// service. Azure proper documents its own `<bookmark mark=…>`
/// replacement element, but the **free Edge endpoint zero-audios on
/// `<bookmark>` and on `<mstts:express-as …>` style sections too**
/// (verified live), so in Edge mode (`is_edge`) those tags are dropped
/// as well. `<mark>`/`<bookmark>` are empty elements, and for the
/// paired `mstts:express-as` wrapper only the tags are removed — the
/// spoken content inside survives. Consumers that need positions should
/// use word-boundary events. speech-dispatcher's wrapper injects
/// `<mark name="__spd_N"/>` around every pause, so pass-through SSML
/// from SSIP clients hits this constantly.
#[cfg(feature = "cloud")]
pub(crate) fn strip_unsupported_marks(ssml: &str, is_edge: bool) -> String {
    let has_marks = ssml.contains("<mark") || ssml.contains("</mark");
    let has_edge_extras = is_edge
        && (ssml.contains("<bookmark")
            || ssml.contains("</bookmark")
            || ssml.contains("<mstts:express-as")
            || ssml.contains("</mstts:express-as"));
    if !has_marks && !has_edge_extras {
        return ssml.to_string();
    }
    let mut out = String::with_capacity(ssml.len());
    let mut rest = ssml;
    while let Some(pos) = rest.find('<') {
        let after = &rest[pos..];
        // Tag-name boundary check so `<market>` stays untouched.
        let name_done = |s: &str, prefix: &str| {
            s.strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with([' ', '\t', '\r', '\n', '/', '>']))
        };
        let drop = name_done(after, "<mark")
            || name_done(after, "</mark")
            || (is_edge
                && (name_done(after, "<bookmark")
                    || name_done(after, "</bookmark")
                    || name_done(after, "<mstts:express-as")
                    || name_done(after, "</mstts:express-as")));
        if drop {
            if let Some(end) = after.find('>') {
                out.push_str(&rest[..pos]);
                rest = &after[end + 1..];
                continue;
            }
        }
        out.push_str(&rest[..=pos]);
        rest = &rest[pos + 1..];
    }
    out.push_str(rest);
    out
}

/// Build SSML for Azure TTS.
pub(crate) fn build_azure_ssml(
    text: &str,
    voice: &str,
    rate: f32,
    pitch: f32,
    volume: f32,
) -> String {
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
pub(crate) fn build_google_request(
    text: &str,
    voice: &str,
    add_marks: bool,
    input_ssml: Option<&str>,
) -> (serde_json::Value, Vec<String>) {
    // Derive the language code from the voice name. Standard Google voice
    // names start with "xx-YY" (e.g. "en-US-Wavenet-D"). Newer named
    // voices (Gemini/Chirp3-HD: "Algieba", "Aoede", etc.) don't — Google
    // routes those by name alone, so we omit languageCode for them.
    let voice_obj = if voice.len() >= 5 && voice.as_bytes().get(2) == Some(&b'-') {
        let lang = &voice[..5];
        serde_json::json!({ "languageCode": lang, "name": voice })
    } else {
        serde_json::json!({ "name": voice })
    };

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
        "voice": voice_obj,
        // sampleRateHertz is pinned so the decoded PCM rate is knowable by
        // callers that receive bare bytes via on_audio (Google otherwise
        // returns each voice's natural rate: 22050/24000/32000).
        "audioConfig": { "audioEncoding": "MP3", "sampleRateHertz": 24000 }
    });

    if add_marks {
        body["enableTimePointing"] = serde_json::json!(["SSML_MARK"]);
    }

    (body, words_list)
}

/// Parse Google timepoints into word boundaries.
pub(crate) fn parse_google_timepoints(
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
            estimated: false,
        });
    }
    boundaries
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

/// Map the wrapper's rate/pitch/volume multipliers (1.0 = normal, 0.0 =
/// unset) onto the Gemini prompting guide's style vocabulary. Sustained
/// delivery is a turn-level `speech_metadata.style` concern on Gemini 3.8
/// TTS — there is no numeric prosody — so numeric parameters can only be
/// expressed approximately as style words.
pub(crate) fn gemini_style_from_params(rate: f32, pitch: f32, volume: f32) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if rate > 0.0 && (rate - 1.0).abs() > f32::EPSILON {
        if rate < 1.0 {
            parts.push("speaking slowly");
        } else {
            parts.push("speaking rapidly");
        }
    }
    if pitch > 0.0 && (pitch - 1.0).abs() > f32::EPSILON {
        if pitch < 1.0 {
            parts.push("low pitch");
        } else {
            parts.push("high pitch");
        }
    }
    if volume > 0.0 && (volume - 1.0).abs() > f32::EPSILON {
        if volume < 1.0 {
            parts.push("speaking softly");
        } else {
            parts.push("speaking loudly");
        }
    }
    parts.join(", ")
}

/// Build the Interactions API request body for Gemini TTS.
///
/// The transcript is sent verbatim — the model performs the text as
/// written (the SpeechMarkdown `gemini` dialect has already rendered
/// angle-bracket vocal bursts, pause tags and CAPS emphasis into it).
/// Turn-level delivery rides in `speech_metadata.style`: an explicit
/// credential `style` wins outright; otherwise the rate/pitch/volume
/// multipliers map onto the documented style vocabulary. The voice may
/// be a prebuilt name ("Kore"), an Extended Voice Library ID, a voice
/// design ID (`voice_...`) or a stateless replication key
/// (`voicekey_...`) — all pass through in `speech_config`.
pub(crate) fn build_gemini_request(
    text: &str,
    voice: &str,
    rate: f32,
    pitch: f32,
    volume: f32,
    model: Option<&str>,
    style_override: Option<&str>,
) -> serde_json::Value {
    let style = style_override.map_or_else(
        || gemini_style_from_params(rate, pitch, volume),
        str::to_string,
    );

    let mut content = serde_json::json!({ "type": "text", "text": text });
    if !style.is_empty() {
        content["annotations"] = serde_json::json!([
            { "type": "speech_metadata", "style": style }
        ]);
    }

    serde_json::json!({
        "model": model.unwrap_or("gemini-3.8-flash-tts"),
        "input": [{ "type": "user_input", "content": [content] }],
        "response_format": { "type": "audio" },
        "generation_config": { "speech_config": [{ "voice": voice }] },
    })
}

/// Outcome of scanning an Interactions API response for audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GeminiAudioBlock {
    /// The last audio block, decoded from base64.
    Present(Vec<u8>),
    /// No audio block in the response (text-only reply, refusal).
    Absent,
    /// An audio block existed but its base64 payload was corrupt.
    Corrupt,
}

/// Extract the last audio block (decoded from base64) from an Interactions
/// API response.
///
/// REST shape: `steps[*].content[*]` blocks with `type == "audio"` carry
/// `data` (base64) and `mime_type` (`audio/wav`). The last audio block
/// matches the SDK's `output_audio` convenience property. Base64
/// corruption is reported distinctly from absence so the caller can
/// produce an accurate diagnostic.
pub(crate) fn parse_gemini_interaction_audio(json: &serde_json::Value) -> GeminiAudioBlock {
    use base64::Engine;
    let Some(steps) = json.get("steps").and_then(|v| v.as_array()) else {
        return GeminiAudioBlock::Absent;
    };
    let mut last: Option<&str> = None;
    for step in steps {
        let Some(content) = step.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(|v| v.as_str()) == Some("audio") {
                if let Some(data) = block.get("data").and_then(|v| v.as_str()) {
                    last = Some(data);
                }
            }
        }
    }
    match last {
        Some(data) => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_or(GeminiAudioBlock::Corrupt, GeminiAudioBlock::Present),
        None => GeminiAudioBlock::Absent,
    }
}

/// Parse the sample rate from a RIFF/WAVE `fmt ` header (bytes 24–28,
/// little-endian u32). Returns 24_000 (the documented Gemini output rate)
/// for anything non-conforming — including a magic-valid header whose
/// rate field is zero or implausible, which would otherwise divide the
/// boundary scaler by zero.
pub(crate) fn wav_sample_rate(wav: &[u8]) -> u32 {
    if wav.len() > 28 && &wav[0..4] == b"RIFF" && &wav[8..12] == b"WAVE" && &wav[12..16] == b"fmt "
    {
        let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        if (8_000..=192_000).contains(&rate) {
            return rate;
        }
    }
    24_000
}

/// Fire estimated word boundaries scaled to the actual delivered audio
/// duration. The estimator assumes 150 wpm; for providers that return the
/// complete buffer without timestamps (Gemini), scaling the estimates to
/// the real duration keeps events roughly aligned with playback.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn fire_scaled_estimates(
    cb: &mut crate::engine::OnBoundaryCallback<'_>,
    text: &str,
    pcm: &[u8],
    sample_rate: u32,
) {
    let plan = crate::boundaries::EstimatePlan::build(text);
    let n = plan.len();
    if n == 0 || pcm.is_empty() {
        return;
    }
    let Some(last) = plan.event(n - 1) else {
        return;
    };
    let est_ms = (last.end_s * 1000.0).max(1.0);
    let actual_ms = (pcm.len() as f32 / 2.0) * 1000.0 / sample_rate as f32;
    let scale = actual_ms / est_ms;
    for i in 0..n {
        if let Some(e) = plan.event(i) {
            // These are proportional estimates (scaled to the real audio
            // duration) — flagged as such per the callback contract.
            cb(
                &e.word,
                e.start_s * scale,
                e.end_s * scale,
                e.char_offset,
                e.char_len,
                true,
            );
        }
    }
}

/// Pick the SpeechMarkdown platform selector for a provider/model pair.
///
/// Most providers map to `provider` unchanged (the caller's provider id is
/// itself the selector for azure/google/gemini/the Alexa fallback);
/// ElevenLabs markup is model-dependent: `eleven_v3*` parses no SSML and
/// needs the audio-tag dialect, every other ElevenLabs model understands
/// `<break>`.
pub(crate) fn elevenlabs_smd_platform<'a>(provider: &'a str, model: Option<&str>) -> &'a str {
    if provider == "elevenlabs" && model.is_some_and(|m| m.starts_with("eleven_v3")) {
        "elevenlabs-v3"
    } else {
        provider
    }
}

/// Translate W3C (or Alexa/Azure-flavoured) SSML into an ElevenLabs
/// dialect prompt: SSML → SpeechMarkdown → the model-matched dialect.
/// Returns `None` when the input does not parse; callers then fall back
/// to plain-text stripping.
#[cfg(feature = "speechmarkdown")]
/// Translate W3C SSML into a prompt dialect via SpeechMarkdown:
/// SSML → SpeechMarkdown → the target platform's dialect.
///
/// Named for its ElevenLabs origin but shared by every no-SSML dialect
/// (ElevenLabs pre-v3/v3, Gemini): these engines read stray XML aloud,
/// so incoming `tts_speak_ssml` input must be translated, not stripped.
pub(crate) fn ssml_to_dialect(ssml: &str, smd_platform: &str) -> Option<String> {
    use speechmarkdown_rust::{Platform, SpeechMarkdownParser};
    let platform = Platform::from_platform_str(smd_platform)?;
    let smd = SpeechMarkdownParser::to_smd(ssml).ok()?;
    SpeechMarkdownParser::to_ssml(&smd, platform).ok()
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
pub(crate) fn parse_elevenlabs_alignment(
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
pub(crate) fn map_azure_voices(json: &[serde_json::Value]) -> Vec<Voice> {
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
pub(crate) fn map_google_voices(json: &[serde_json::Value]) -> Vec<Voice> {
    let mut voices = Vec::new();
    for v in json {
        let Some(name) = v.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        // Google returns bare named voices (e.g. "Algieba", "Aoede") for
        // Gemini/Chirp3-HD alongside the locale-prefixed duplicates (e.g.
        // "en-US-Chirp3-HD-Algieba"). The bare names fail at synthesis
        // ("requires a model name"). Skip them — the prefixed versions
        // work correctly.
        if !name.contains('-') {
            continue;
        }
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

/// Map Gemini Extended Voice Library JSON to unified voices.
///
/// `GET /v1beta/voices` returns `{ "voices": [ ... ] }` with rich metadata
/// per voice: `{ "id": "kore", "display_name": "Kore", "language_code":
/// "en-US", "accent": "...", "persona": "...", "gender": "..." }`. The
/// same voice IDs work in `speech_config` — including voice design IDs
/// (`voice_...`) and replication keys (`voicekey_...`) when present.
pub(crate) fn map_gemini_voices(json: &[serde_json::Value]) -> Vec<Voice> {
    let mut voices = Vec::new();
    for v in json {
        let Some(id) = v.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = v
            .get("display_name")
            .or_else(|| v.get("displayName"))
            .and_then(|v| v.as_str())
            .unwrap_or(id)
            .to_string();
        let gender_raw = v.get("gender").and_then(|v| v.as_str()).unwrap_or("");
        // Persona enriches the display name so a voice picker can
        // differentiate the 50+ prebuilt voices ("Kore — Firm, ... ").
        let persona = v.get("persona").and_then(|v| v.as_str()).unwrap_or("");
        let display = if persona.is_empty() {
            name.clone()
        } else {
            format!("{name} — {persona}")
        };
        let locale = v
            .get("language_code")
            .or_else(|| v.get("languageCode"))
            .and_then(|v| v.as_str())
            .unwrap_or("en-US");
        voices.push(Voice {
            id: id.to_string(),
            name: display,
            gender: normalize_gender(gender_raw),
            provider: "gemini".to_string(),
            language_codes: vec![LanguageCode {
                iso639_3: locale.split('-').next().unwrap_or("en").to_string(),
                bcp47: locale.to_string(),
                display: crate::types::locale_display_name(locale),
            }],
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
pub(crate) fn map_generic_voices(provider: &str, json: &[serde_json::Value]) -> Vec<Voice> {
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
pub(crate) fn compute_durations(boundaries: &mut [WordBoundary]) {
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
pub(crate) fn azure_ws_parse_word_boundary(
    item: &serde_json::Value,
) -> Option<(&str, u64, u64, i32, i32)> {
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
pub(crate) fn azure_ws_parse_viseme(item: &serde_json::Value) -> Option<(i32, f32)> {
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

/// Static voice lists for engines that don't expose a voice-list API.
/// Returns `None` for engines that *do* have a `voices_url` (those go
/// through the HTTP path in `get_voices`).
#[cfg(feature = "cloud")]
#[allow(clippy::too_many_lines)]
pub(crate) fn static_voices(provider: &str) -> Option<Vec<Voice>> {
    let en_us = || LanguageCode {
        bcp47: "en-US".to_string(),
        iso639_3: "eng".to_string(),
        display: "English (United States)".to_string(),
    };
    let lang = |bcp47: &str, iso: &str, display: &str| LanguageCode {
        bcp47: bcp47.to_string(),
        iso639_3: iso.to_string(),
        display: display.to_string(),
    };
    let voice =
        |id: &str, name: &str, gender: Gender, provider: &str, lcs: Vec<LanguageCode>| Voice {
            id: id.to_string(),
            name: name.to_string(),
            gender,
            provider: provider.to_string(),
            language_codes: lcs,
        };

    match provider {
        "openai" => Some(vec![
            voice(
                "alloy",
                "OpenAI alloy",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice("ash", "OpenAI ash", Gender::Male, "openai", vec![en_us()]),
            voice(
                "ballad",
                "OpenAI ballad",
                Gender::Male,
                "openai",
                vec![en_us()],
            ),
            voice(
                "coral",
                "OpenAI coral",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice("echo", "OpenAI echo", Gender::Male, "openai", vec![en_us()]),
            voice(
                "fable",
                "OpenAI fable",
                Gender::Male,
                "openai",
                vec![en_us()],
            ),
            voice(
                "nova",
                "OpenAI nova",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice("onyx", "OpenAI onyx", Gender::Male, "openai", vec![en_us()]),
            voice(
                "sage",
                "OpenAI sage",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice(
                "shimmer",
                "OpenAI shimmer",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice(
                "verse",
                "OpenAI verse",
                Gender::Unknown,
                "openai",
                vec![en_us()],
            ),
        ]),
        "hume" => Some(vec![
            voice("ito", "Hume Ito", Gender::Unknown, "hume", vec![en_us()]),
            voice(
                "acantha",
                "Hume Acantha",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice(
                "ant ai gonus",
                "Hume Antigonos",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice("ari", "Hume Ari", Gender::Unknown, "hume", vec![en_us()]),
            voice(
                "brant",
                "Hume Brant",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice(
                "daniel",
                "Hume Daniel",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice("fin", "Hume Fin", Gender::Unknown, "hume", vec![en_us()]),
            voice("hype", "Hume Hype", Gender::Unknown, "hume", vec![en_us()]),
            voice("kora", "Hume Kora", Gender::Unknown, "hume", vec![en_us()]),
            voice(
                "mango",
                "Hume Mango",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice(
                "marek",
                "Hume Marek",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice("ogma", "Hume Ogma", Gender::Unknown, "hume", vec![en_us()]),
            voice("sora", "Hume Sora", Gender::Unknown, "hume", vec![en_us()]),
            voice(
                "terrence",
                "Hume Terrence",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice(
                "vitor",
                "Hume Vitor",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice("zach", "Hume Zach", Gender::Unknown, "hume", vec![en_us()]),
        ]),
        "mistral" => Some(vec![
            voice(
                "Amalthea",
                "Mistral Amalthea",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Achan",
                "Mistral Achan",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Brave",
                "Mistral Brave",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Contessa",
                "Mistral Contessa",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Daintree",
                "Mistral Daintree",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Eugora",
                "Mistral Eugora",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Fornax",
                "Mistral Fornax",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Griffin",
                "Mistral Griffin",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Hestia",
                "Mistral Hestia",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Irving",
                "Mistral Irving",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Jasmine",
                "Mistral Jasmine",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Kestra",
                "Mistral Kestra",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Lorentz",
                "Mistral Lorentz",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Mara",
                "Mistral Mara",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Nettle",
                "Mistral Nettle",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Orin",
                "Mistral Orin",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Puck",
                "Mistral Puck",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Quinn",
                "Mistral Quinn",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Rune",
                "Mistral Rune",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Simbe",
                "Mistral Simbe",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Tertia",
                "Mistral Tertia",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Umbriel",
                "Mistral Umbriel",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Vesta",
                "Mistral Vesta",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Wystan",
                "Mistral Wystan",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Xeno",
                "Mistral Xeno",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Yara",
                "Mistral Yara",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Zephyr",
                "Mistral Zephyr",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
        ]),
        "murf" => {
            let de = || lang("de-DE", "deu", "German (Germany)");
            let es = || lang("es-ES", "spa", "Spanish (Spain)");
            let fr = || lang("fr-FR", "fra", "French (France)");
            let pt = || lang("pt-BR", "por", "Portuguese (Brazil)");
            let it = || lang("it-IT", "ita", "Italian (Italy)");
            Some(vec![
                voice(
                    "en-US-natalie",
                    "Murf Natalie",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-owen",
                    "Murf Owen",
                    Gender::Male,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-amira",
                    "Murf Amira",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-daniel",
                    "Murf Daniel",
                    Gender::Male,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-taylor",
                    "Murf Taylor",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-alex",
                    "Murf Alex",
                    Gender::Male,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-emily",
                    "Murf Emily",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice("en-US-ben", "Murf Ben", Gender::Male, "murf", vec![en_us()]),
                voice(
                    "en-US-claire",
                    "Murf Claire",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-glen",
                    "Murf Glen",
                    Gender::Male,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "de-DE-detlef",
                    "Murf Detlef",
                    Gender::Male,
                    "murf",
                    vec![de()],
                ),
                voice(
                    "es-ES-rosalyn",
                    "Murf Rosalyn",
                    Gender::Female,
                    "murf",
                    vec![es()],
                ),
                voice(
                    "fr-FR-henri",
                    "Murf Henri",
                    Gender::Male,
                    "murf",
                    vec![fr()],
                ),
                voice(
                    "pt-BR-thomas",
                    "Murf Thomas",
                    Gender::Male,
                    "murf",
                    vec![pt()],
                ),
                voice(
                    "it-IT-giulia",
                    "Murf Giulia",
                    Gender::Female,
                    "murf",
                    vec![it()],
                ),
            ])
        }
        "unrealspeech" => Some(vec![
            voice(
                "Sierra",
                "UnrealSpeech Sierra",
                Gender::Female,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Dan",
                "UnrealSpeech Dan",
                Gender::Male,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Will",
                "UnrealSpeech Will",
                Gender::Male,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Scarlett",
                "UnrealSpeech Scarlett",
                Gender::Female,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Liv",
                "UnrealSpeech Liv",
                Gender::Female,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Amy",
                "UnrealSpeech Amy",
                Gender::Female,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Eric",
                "UnrealSpeech Eric",
                Gender::Male,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Brian",
                "UnrealSpeech Brian",
                Gender::Male,
                "unrealspeech",
                vec![en_us()],
            ),
        ]),
        "xai" => Some(vec![
            voice(
                "avalon-47",
                "xAI Avalon",
                Gender::Female,
                "xai",
                vec![en_us()],
            ),
            voice("orion-56", "xAI Orion", Gender::Male, "xai", vec![en_us()]),
            voice("luna-30", "xAI Luna", Gender::Female, "xai", vec![en_us()]),
            voice("atlas-84", "xAI Atlas", Gender::Male, "xai", vec![en_us()]),
            voice("aria-42", "xAI Aria", Gender::Female, "xai", vec![en_us()]),
            voice("cosmo-01", "xAI Cosmo", Gender::Male, "xai", vec![en_us()]),
        ]),
        "upliftai" => {
            let ur = || lang("ur-PK", "urd", "Urdu (Pakistan)");
            Some(vec![
                voice(
                    "v_8eelc901",
                    "UpliftAI Info/Education",
                    Gender::Unknown,
                    "upliftai",
                    vec![ur()],
                ),
                voice(
                    "v_30s70t3a",
                    "UpliftAI Nostalgic News",
                    Gender::Unknown,
                    "upliftai",
                    vec![ur()],
                ),
                voice(
                    "v_yypgzenx",
                    "UpliftAI Dada Jee",
                    Gender::Unknown,
                    "upliftai",
                    vec![ur()],
                ),
                voice(
                    "v_kwmp7zxt",
                    "UpliftAI Gen Z",
                    Gender::Unknown,
                    "upliftai",
                    vec![ur()],
                ),
            ])
        }
        "modelslab" => Some(vec![
            voice(
                "madison",
                "ModelsLab Madison",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "tara",
                "ModelsLab Tara",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "leah",
                "ModelsLab Leah",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "jess",
                "ModelsLab Jess",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "mia",
                "ModelsLab Mia",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "zoe",
                "ModelsLab Zoe",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "leo",
                "ModelsLab Leo",
                Gender::Male,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "dan",
                "ModelsLab Dan",
                Gender::Male,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "zac",
                "ModelsLab Zac",
                Gender::Male,
                "modelslab",
                vec![en_us()],
            ),
        ]),
        _ => None,
    }
}
