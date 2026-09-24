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
