// The split modules resolve shared names through the parent glob.
#![allow(clippy::wildcard_imports)]

use super::*;

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
