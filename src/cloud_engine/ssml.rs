// The split modules resolve shared names through the parent glob.
#![allow(clippy::wildcard_imports)]

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
