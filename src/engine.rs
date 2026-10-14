//! Core TTS engine trait.

use crate::types::{SpeakOptions, TtsResult, Voice, WordBoundary};
use std::fmt;

/// Callback for streaming audio chunks.
pub type OnAudioCallback<'a> = &'a mut dyn FnMut(&[u8]);

/// Callback for word boundary events.
/// Signature: (word, start_sec, end_sec, byte_offset, byte_len, estimated)
/// `byte_offset` is a byte index into the text as submitted (for
/// SpeechMarkdown input, the caller's original source — highlighting
/// targets the caller's string) and `byte_len` a byte length. When a word cannot be located (normalization mismatch,
/// punctuation artifact), the callback receives the last known offset
/// held (0 if nothing matched yet) with `byte_len` -1 — not a -1 offset.
/// `estimated` is true for proportional estimates (unpatched voices,
/// wpm model), false for measured timings (cloud provider timings).
pub type OnBoundaryCallback<'a> = &'a mut dyn FnMut(&str, f32, f32, i32, i32, bool);

/// Callback for SSML mark/bookmark events.
/// Signature: (name, start_sec, end_sec, char_offset)
/// char_offset is -1 when the engine doesn't report it. Engines that
/// estimated) audio position; consumers map it to their bookmark event
/// (SAPI `SPEI_TTS_BOOKMARK`, SSIP index marks).
pub type OnMarkCallback<'a> = &'a mut dyn FnMut(&str, f32, f32, i32);

/// Callback for speech-started events.
pub type OnStartCallback<'a> = &'a mut dyn FnMut();

/// Callback for speech-finished events.
pub type OnEndCallback<'a> = &'a mut dyn FnMut();

/// Callback for error events.
pub type OnErrorCallback<'a> = &'a mut dyn FnMut(&str);

/// Convert SpeechMarkdown to SSML when detected, otherwise return the text
/// unchanged. Returns `(processed_text, is_ssml)`.
///
/// `platform` picks the SSML flavour:
///   - `"azure"` → MicrosoftAzure
///   - `"google"` → GoogleAssistant
///   - `"elevenlabs"` → ElevenLabs pre-v3 prompt markup (`<break>` tags,
///     no SSML document). Not SSML: `is_ssml` is returned false so engines
///     pass the markup through instead of stripping it.
///   - `"elevenlabs-v3"` → Eleven v3 audio-tag dialect (`[whispers]`,
///     `[pause]`, native slash IPA). Eleven v3 parses no SSML at all.
///   - `"sapi"` / `"avsynth"` / anything else → AmazonAlexa (the closest
///     generic SSML baseline; SAPI's own parser accepts the subset that
///     speechmarkdown-rust emits for Alexa)
///
/// Available whenever the `speechmarkdown` feature is on (auto-enabled by
/// `cloud`, `sapi`, and `avsynth`). Without it, this is a no-op stub.
#[cfg(feature = "speechmarkdown")]
#[must_use]
pub fn preprocess_speech_markdown(text: &str, platform: &str) -> (String, bool) {
    use speechmarkdown_rust::{Platform, SpeechMarkdownParser};

    // If the input is already SSML (starts with <speak), pass it through
    // unchanged and flag it so the engine knows not to escape/wrap it.
    if text.trim_start().to_ascii_lowercase().starts_with("<speak") {
        return (text.to_string(), true);
    }

    if !SpeechMarkdownParser::is_speech_markdown(text) {
        return (text.to_string(), false);
    }

    let platform = match platform {
        // Edge speaks the Azure SSML dialect (same Speech platform; it is
        // not an Alexa-family endpoint) — its free endpoint just lacks a
        // few elements, which the engine boundary strips.
        "azure" | "edge" => Platform::MicrosoftAzure,
        "google" => Platform::GoogleAssistant,
        // ElevenLabs prompt markup, not SSML: the dialects must reach the
        // API verbatim (stripping would drop every break/tag, and v3
        // models read stray XML aloud).
        "elevenlabs" => Platform::ElevenLabs,
        "elevenlabs-v3" => Platform::ElevenLabsV3,
        // Gemini 3.8 TTS is likewise a prompt dialect: angle-bracket
        // vocal bursts/pauses and CAPS emphasis — no SSML document.
        "gemini" => Platform::Gemini,

        // Everything else falls back to the Alexa-baseline SSML dialect.
        _ => Platform::AmazonAlexa,
    };

    let is_prompt_dialect = matches!(
        platform,
        Platform::ElevenLabs | Platform::ElevenLabsV3 | Platform::Gemini
    );

    match SpeechMarkdownParser::to_ssml(text, platform) {
        // The prompt dialects are text, not SSML: flag them so engines
        // send the string as-is rather than treating it as SSML.
        Ok(ssml) => (ssml, !is_prompt_dialect),
        Err(_) => (text.to_string(), false),
    }
}

#[cfg(not(feature = "speechmarkdown"))]
#[must_use]
pub fn preprocess_speech_markdown(text: &str, _platform: &str) -> (String, bool) {
    (text.to_string(), false)
}

/// Strip all XML/SSML tags from `ssml`, returning the plain-text content with
/// collapsed whitespace. Used to convert incoming W3C SSML (from
/// `tts_speak_ssml`) to plain text for engines that don't accept SSML
/// (OpenAI, ElevenLabs, SherpaOnnx, etc.).
#[must_use]
pub fn strip_ssml_to_text(ssml: &str) -> String {
    let mut out = String::with_capacity(ssml.len());
    let mut in_tag = false;
    for ch in ssml.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract the `name` attribute from a `<voice name='...'>` (or `"..."`) tag
/// and return the inner SSML content wrapped in a bare `<speak>` element.
///
/// When the C++ adapter calls `tts_speak_ssml` it wraps content in
/// `<speak version='...' xmlns='...'><voice name='...'>INNER</voice></speak>`.
/// Google/Watson/Polly don't use the `<voice>` wrapper (voice is set via API
/// config), so this strips it and returns `(voice_name, "<speak>INNER</speak>")`.
///
/// Returns `(None, original)` when no `<voice>` tag is found.
#[must_use]
pub fn unwrap_voice_tag(ssml: &str) -> (Option<String>, String) {
    // Find <voice ...> opening tag.
    let Some(open_idx) = ssml.find("<voice ") else {
        return (None, ssml.to_string());
    };
    let Some(tag_offset) = ssml[open_idx..].find('>') else {
        return (None, ssml.to_string());
    };
    let tag_end = open_idx + tag_offset;
    let tag_str = &ssml[open_idx..=tag_end];

    let voice_name = extract_name_attr(tag_str);

    // Content between the voice tag's `>` and `</voice>`.
    let content_start = tag_end + 1;
    let content_end = ssml.find("</voice>").unwrap_or(ssml.len());
    let inner = &ssml[content_start..content_end];

    (voice_name, format!("<speak>{inner}</speak>"))
}

/// Pull `name='value'` or `name="value"` out of an XML tag string.
fn extract_name_attr(tag: &str) -> Option<String> {
    for quote in ['\'', '"'] {
        let needle = format!("name={quote}");
        if let Some(start) = tag.find(&needle) {
            let val_start = start + needle.len();
            if let Some(end) = tag[val_start..].find(quote) {
                return Some(tag[val_start..val_start + end].to_string());
            }
        }
    }
    None
}

/// Trait that every TTS engine must implement.
///
/// Mirrors Swift's `TTSClient` protocol.
#[allow(clippy::missing_errors_doc)]
pub trait TtsEngine: Send + Sync + fmt::Debug {
    /// Start speaking `text` asynchronously.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
        on_mark: Option<OnMarkCallback>,
    ) -> TtsResult<()>;

    /// Speak with full [`SpeakOptions`], matching Swift's `speak(_:options:)`.
    fn speak_with_options(
        &self,
        text: &str,
        options: Option<&SpeakOptions>,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
        on_mark: Option<OnMarkCallback>,
    ) -> TtsResult<()> {
        let opts = options.cloned().unwrap_or_default();
        self.speak(
            text,
            opts.voice.as_deref(),
            opts.effective_rate(),
            opts.effective_pitch(),
            opts.effective_volume(),
            on_audio,
            on_boundary,
            on_mark,
        )
    }

    /// Speak `text` synchronously, blocking until synthesis completes.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<OnAudioCallback>,
        on_boundary: Option<OnBoundaryCallback>,
        on_mark: Option<OnMarkCallback>,
    ) -> TtsResult<()>;

    /// Stop any in-progress speech.
    fn stop(&self) -> TtsResult<()>;

    /// Pause speech (default: no-op, engines may override).
    fn pause(&self) -> TtsResult<()> {
        Ok(())
    }

    /// Resume speech (default: no-op, engines may override).
    fn resume(&self) -> TtsResult<()> {
        Ok(())
    }

    /// List available voices for this engine.
    fn get_voices(&self) -> TtsResult<Vec<Voice>>;

    /// Return the unique identifier of this engine (e.g. `"system"`, `"sherpaonnx"`).
    fn engine_id(&self) -> &'static str;

    /// Check whether the configured credentials are valid.
    /// Default: attempt to fetch voices as a validation.
    fn check_credentials(&self) -> TtsResult<bool> {
        match self.get_voices() {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Synthesize text to audio bytes (full buffer, no playback).
    /// Mirrors Swift's `synthToBytes(_:options:)`.
    fn synth_to_bytes(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<Vec<u8>> {
        let mut buf = Vec::new();
        self.speak(
            text,
            voice,
            rate,
            pitch,
            volume,
            Some(&mut |chunk: &[u8]| {
                buf.extend_from_slice(chunk);
            }),
            None,
            None,
        )?;
        Ok(buf)
    }

    /// Synthesize with [`SpeakOptions`].
    fn synth_to_bytes_with_options(
        &self,
        text: &str,
        options: Option<&SpeakOptions>,
    ) -> TtsResult<Vec<u8>> {
        let opts = options.cloned().unwrap_or_default();
        self.synth_to_bytes(
            text,
            opts.voice.as_deref(),
            opts.effective_rate(),
            opts.effective_pitch(),
            opts.effective_volume(),
        )
    }

    /// Synthesize text and return word boundary information.
    fn synth_with_boundaries(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<(Vec<u8>, Vec<WordBoundary>)> {
        let audio = self.synth_to_bytes(text, voice, rate, pitch, volume)?;
        let boundaries = estimate_word_boundaries(text);
        Ok((audio, boundaries))
    }
}

/// Estimate word boundaries using word-length-adjusted timing with an
/// inter-sentence pause. Originally mirroring Swift's
/// `WordTimingEstimator`, since extended (sentence gaps) beyond it.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn estimate_word_boundaries(text: &str) -> Vec<WordBoundary> {
    estimate_word_boundaries_with_wpm(text, 150.0)
}

/// Estimate word boundaries with configurable words per minute.
/// Abbreviation caveat: token-level detection means "Mr." / "e.g." also
/// get the sentence gap — an accepted trade-off for an estimator.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn estimate_word_boundaries_with_wpm(text: &str, words_per_minute: f64) -> Vec<WordBoundary> {
    // Weighted model: per-word duration tracks syllable count (a far
    // better duration proxy than character length — "strength" is one
    // syllable, "audio" is three), plus tiered pauses after punctuation.
    // Calibration: average English ≈ 1.55 syllables/word, so
    // ms_per_syllable = 60_000 / (wpm × 1.55) reproduces the old wpm.
    // Sentence gap stays a flat 350 ms (existing estimator contract —
    // inline punctuation gets no gap here; the tiered beat model lives
    // in weighted_boundaries_scaled where a real duration is known).
    const SENTENCE_GAP_MS: u64 = 350;

    #[allow(clippy::cast_precision_loss)]
    let ms_per_syllable = 60_000.0 / (words_per_minute * 1.55);

    let words: Vec<&str> = text.split_whitespace().filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return Vec::new();
    }

    let mut boundaries: Vec<WordBoundary> = Vec::with_capacity(words.len());
    let mut current_ms: u64 = 0;

    for (i, word) in words.iter().enumerate() {
        if i > 0 && ends_sentence(words[i - 1]) {
            current_ms += SENTENCE_GAP_MS;
        }
        let duration = ((word_weight(word) * ms_per_syllable) as u64).max(1);
        boundaries.push(WordBoundary {
            text: (*word).to_string(),
            offset: current_ms,
            duration,
            estimated: true,
        });
        current_ms += duration;
    }

    boundaries
}

/// Rough syllable count for duration weighting: contiguous vowel groups
/// (English-centric), every digit spoken individually, minimum one.
#[must_use]
pub fn syllable_count(word: &str) -> usize {
    let mut count = 0usize;
    let mut in_vowel_run = false;
    for ch in word.chars() {
        if ch.is_ascii_digit() {
            // digits read individually ("2026" -> four syllables)
            count += 1;
            in_vowel_run = false;
        } else {
            let vowel = matches!(ch.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y');
            if vowel && !in_vowel_run {
                count += 1;
            }
            in_vowel_run = vowel;
        }
    }
    count.max(1)
}

/// Duration weight of a word in syllable-equivalents: syllables plus a
/// small constant for onsets/codas so "a" is not zero-length.
#[must_use]
pub fn word_weight(word: &str) -> f64 {
    let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
    f64::from(syllable_count(bare) as u32) + 0.3
}

/// Pause weight AFTER a word, in syllable-equivalents, from its trailing
/// punctuation. Sentence ends breathe longest; commas/semicolons beat.
#[must_use]
pub fn trailing_pause_weight(word: &str) -> f64 {
    // look past closing quotes/brackets, like ends_sentence()
    let trimmed = word
        .trim_end()
        .trim_end_matches(['"', '\'', '”', '’', ')', ']', '»']);
    let Some(last) = trimmed.chars().last() else {
        return 0.0;
    };
    match last {
        '.' | '!' | '?' | '…' => 1.4, // sentence breathe (≥ the 350 ms gap)
        ';' | ':' | '—' | '–' => 0.75,
        ',' => 0.55,
        _ => 0.0,
    }
}

/// Word boundaries with RELATIVE durations mapped onto a known total
/// duration (engines that know the audio length but not per-word times —
/// e.g. qwen3-local). The word shape comes from the syllable/pause
/// weights; the timeline is linearly rescaled so the last word ends
/// exactly at `total_secs`. Offsets/durations are absolute seconds.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn weighted_boundaries_scaled(text: &str, total_secs: f32) -> Vec<WordBoundary> {
    let words: Vec<&str> = text.split_whitespace().filter(|w| !w.is_empty()).collect();
    if words.is_empty() || total_secs <= 0.0 {
        return Vec::new();
    }

    // cumulative weight marks: word i occupies (after_i - before_i)
    let mut before: Vec<f64> = Vec::with_capacity(words.len());
    let mut after: Vec<f64> = Vec::with_capacity(words.len());
    let mut cum = 0.0f64;
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            cum += trailing_pause_weight(words[i - 1]);
        }
        before.push(cum);
        cum += word_weight(word);
        after.push(cum);
    }
    let total_w = cum.max(1e-9);

    words
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let start = f64::from(total_secs) * before[i] / total_w;
            let end = f64::from(total_secs) * after[i] / total_w;
            WordBoundary {
                text: (*word).to_string(),
                offset: (start * 1000.0) as u64,
                duration: (((end - start) * 1000.0) as u64).max(1),
                estimated: true,
            }
        })
        .collect()
}

/// Whether `word` ends a sentence for estimation purposes: terminal
/// sentence punctuation (possibly before a closing quote or bracket).
/// Semicolons, colons and commas stay inline — they read as short beats,
/// not sentence breaks.
fn ends_sentence(word: &str) -> bool {
    let trimmed = word.trim_end_matches(['"', '\'', ')', ']', '”', '’']);
    trimmed.ends_with('.')
        || trimmed.ends_with('!')
        || trimmed.ends_with('?')
        || trimmed.ends_with('…')
}

#[cfg(test)]
mod tests {
    #[test]
    fn estimator_adds_sentence_gap_between_sentences() {
        let b = estimate_word_boundaries("One. Two words here.");
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].offset, 0);
        // Without a gap, word 2 would start exactly after word 1's
        // duration; with it, at least SENTENCE_GAP_MS later.
        assert!(b[1].offset >= b[0].offset + b[0].duration + 350);
        // No gap inside the second sentence (word 3 follows word 2
        // directly).
        assert_eq!(b[2].offset, b[1].offset + b[1].duration);
    }

    #[test]
    fn estimator_no_gap_without_sentence_punctuation() {
        let b = estimate_word_boundaries("one two three");
        for w in b.windows(2) {
            assert_eq!(w[1].offset, w[0].offset + w[0].duration);
        }
    }

    #[test]
    fn estimator_gap_applies_before_closing_quote() {
        let b = estimate_word_boundaries("He said \"stop.\" Then left.");
        // "Then" follows the quoted sentence end: gap applied.
        let then = &b[3];
        let stop = &b[2];
        assert!(then.offset >= stop.offset + stop.duration + 350);
    }

    #[test]
    fn estimator_inline_punctuation_gets_no_gap() {
        let b = estimate_word_boundaries("first, second; third:");
        for w in b.windows(2) {
            assert_eq!(w[1].offset, w[0].offset + w[0].duration);
        }
    }

    use super::*;

    #[test]
    fn test_strip_ssml_to_text_basic() {
        assert_eq!(
            strip_ssml_to_text("<speak>Hello world</speak>"),
            "Hello world"
        );
    }

    #[test]
    fn test_strip_ssml_to_text_with_tags() {
        let ssml = "<speak><voice name='en-US-Aria'>Hello <phoneme alphabet='ipa' ph='wɜːld'>world</phoneme></voice></speak>";
        assert_eq!(strip_ssml_to_text(ssml), "Hello world");
    }

    #[test]
    fn test_strip_ssml_to_text_collapses_whitespace() {
        assert_eq!(
            strip_ssml_to_text("<speak>\n  Hello\n  world\n</speak>"),
            "Hello world"
        );
    }

    #[test]
    fn test_strip_ssml_to_text_plain_passthrough() {
        assert_eq!(strip_ssml_to_text("just plain text"), "just plain text");
    }

    #[test]
    fn test_unwrap_voice_tag_extracts_voice_and_inner() {
        let ssml = "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis'>\
                    <voice name='en-US-Wavenet-D'>Hello <prosody rate='+20%'>fast</prosody></voice>\
                    </speak>";
        let (voice, inner) = unwrap_voice_tag(ssml);
        assert_eq!(voice.as_deref(), Some("en-US-Wavenet-D"));
        assert!(inner.starts_with("<speak>"));
        assert!(inner.contains("Hello"));
        assert!(inner.contains("<prosody"));
        assert!(!inner.contains("<voice"));
    }

    #[test]
    fn test_unwrap_voice_tag_double_quotes() {
        let ssml = "<speak><voice name=\"en-US-AriaNeural\">Hi</voice></speak>";
        let (voice, inner) = unwrap_voice_tag(ssml);
        assert_eq!(voice.as_deref(), Some("en-US-AriaNeural"));
        assert!(inner.contains("Hi"));
    }

    #[test]
    fn test_unwrap_voice_tag_no_voice_returns_original() {
        let ssml = "<speak>Hello world</speak>";
        let (voice, inner) = unwrap_voice_tag(ssml);
        assert!(voice.is_none());
        assert_eq!(inner, ssml);
    }
}

#[cfg(test)]
mod weighted_timing_tests {
    use super::*;

    #[test]
    fn syllable_counts() {
        assert_eq!(syllable_count("friend"), 1);
        assert_eq!(syllable_count("friendship"), 2);
        assert_eq!(syllable_count("banana"), 3);
        assert_eq!(syllable_count("strength"), 1);
        assert_eq!(syllable_count("2026"), 4); // digits read individually
        assert_eq!(syllable_count("hmm"), 1); // no vowels -> minimum
    }

    #[test]
    fn scaled_covers_exactly_and_orders_monotonically() {
        let b = weighted_boundaries_scaled("Hello, this is a timing test.", 10.0);
        assert_eq!(b.len(), 6);
        assert_eq!(b[0].offset, 0);
        let last = b.last().unwrap();
        // last word ends at the audio end (10 s)
        assert!(last.offset + last.duration >= 9_990);
        for w in b.windows(2) {
            assert!(w[1].offset >= w[0].offset + w[0].duration - 1);
        }
    }

    #[test]
    fn long_words_get_more_time_than_short() {
        let b = weighted_boundaries_scaled("a antidisestablishmentarianism", 10.0);
        let short = b[0].duration;
        let long = b[1].duration;
        assert!(long > short * 4, "long {long} vs short {short}");
    }

    #[test]
    fn punctuation_beats_pause() {
        // comma between the words: the gap counts toward neither word
        let b = weighted_boundaries_scaled("Hello, world", 10.0);
        let gap = b[1].offset - (b[0].offset + b[0].duration);
        assert!(gap > 10, "expected comma pause, got {gap} ms");
        // and a comma pause is shorter than a sentence pause
        let s = weighted_boundaries_scaled("Hello. World", 10.0);
        let sgap = s[1].offset - (s[0].offset + s[0].duration);
        assert!(sgap > gap);
    }

    #[test]
    fn estimator_long_word_longer_than_short() {
        let b = estimate_word_boundaries("strength audio");
        // 1 syllable vs 3: the weighted model must reflect that
        assert!(b[1].duration > b[0].duration);
    }
}
