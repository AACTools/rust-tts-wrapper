// The split modules resolve shared names through the parent glob.
#![allow(clippy::wildcard_imports)]

/// Pick the SpeechMarkdown platform selector for a provider/model pair.
///
/// Most providers map to `provider` unchanged (the caller's provider id is
/// itself the selector for azure/google/gemini/the Alexa fallback);
/// ElevenLabs markup is model-dependent: `eleven_v3*` parses no SSML and
/// needs the audio-tag dialect, every other ElevenLabs model understands
/// `<break>`.
pub(crate) fn elevenlabs_smd_platform<'a>(provider: &'a str, model: Option<&str>) -> &'a str {
    // v4 and v3 share the audio-tag dialect: neither parses SSML
    // (breaks become ellipses/audio tags, styles become [tags]).
    if provider == "elevenlabs"
        && model.is_some_and(|m| m.starts_with("eleven_v3") | m.starts_with("eleven_v4"))
    {
        "elevenlabs-v3"
    } else {
        provider
    }
}

/// Does this model take the inline `"/IPA/"` pronunciation syntax?
/// (v4's native IPA; v3 ignores phoneme tags entirely, eleven_flash_v2
/// is the only model family that parses XML `<phoneme>`.)
#[must_use]
pub(crate) fn elevenlabs_takes_inline_ipa(model: Option<&str>) -> bool {
    model.is_some_and(|m| m.starts_with("eleven_v4"))
}

/// Rewrite SSML `<phoneme>` tags into the model-appropriate form:
///
/// - inline-IPA models (v4): `<phoneme alphabet="ipa" ph="X">w</phoneme>`
///   becomes `"/X/"` (ElevenLabs' native syntax);
/// - everything else: the tag is unsupported, keep just the word.
///
/// Applied before SSML→dialect conversion so the inline form rides
/// through SpeechMarkdown untouched.
#[must_use]
pub(crate) fn elevenlabs_inline_phonemes(ssml: &str) -> String {
    let mut out = String::with_capacity(ssml.len());
    let mut rest = ssml;
    while let Some(start) = rest.find('<') {
        let after = &rest[start..];
        if let Some(tag_end) = after.find('>') {
            let tag = &after[..=tag_end];
            if tag.starts_with("<phoneme") {
                // Find the ph="..." attribute: splitting on quotes
                // yields name and value as separate parts, so the value
                // is the part AFTER the one ending in `ph=`.
                let parts: Vec<&str> = tag.split(['"', '\'']).collect();
                let ph = parts
                    .iter()
                    .position(|p| p.trim() == "ph=" || p.trim_end() == "ph=")
                    .and_then(|i| parts.get(i + 1).copied());
                let alphabet_is_ipa =
                    tag.contains("alphabet=\"ipa\"") || !tag.contains("alphabet=");
                // Skip to the closing </phoneme>, keeping the inner word
                // only when we cannot produce an inline form.
                let remaining = &after[tag_end + 1..];
                let close = remaining.find("</phoneme>").map_or(remaining.len(), |i| i);
                let inner = &remaining[..close];
                let word = inner.trim();
                match ph {
                    Some(ipa) if alphabet_is_ipa => {
                        out.push_str(&rest[..start]);
                        out.push_str("\"/");
                        out.push_str(ipa);
                        out.push_str("/\"");
                    }
                    _ => {
                        out.push_str(&rest[..start]);
                        out.push_str(word);
                    }
                }
                rest = &remaining[close.min(remaining.len())..];
                rest = rest.strip_prefix("</phoneme>").unwrap_or(rest);
                continue;
            }
        }
        out.push_str(&rest[..=start]);
        rest = &rest[start + 1..];
    }
    out.push_str(rest);
    out
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
