//! Windows SAPI (Speech API) engine.

use crate::engine::{preprocess_speech_markdown, TtsEngine};
use crate::types::{TtsError, TtsResult, Voice};
use std::sync::Mutex;

// Explicit imports rather than wildcard — clippy rejects `use foo::*` for
// non-prelude modules. The `windows` crate exposes a lot of SAPI types so
// the list is long but unambiguous.
#[cfg(feature = "sapi")]
use windows::{
    core::{Interface, Result, HSTRING, PCWSTR},
    Win32::Media::Speech::{
        IEnumSpObjectTokens, ISpEventSource, ISpObjectToken, ISpObjectTokenCategory, ISpVoice,
        SpObjectTokenCategory, SpVoice, SPCAT_VOICES, SPEI_END_INPUT_STREAM, SPEI_WORD_BOUNDARY,
        SPEVENT, SPF_ASYNC, SPF_IS_XML, SPF_PURGEBEFORESPEAK,
    },
    Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    },
};

// SPFEI(flag) = (1 << SPEI_X). The windows crate doesn't pre-compute these
// so we spell out the two we need.
#[cfg(feature = "sapi")]
const SPFEI_WORD_BOUNDARY: u64 = 1 << SPEI_WORD_BOUNDARY.0;
#[cfg(feature = "sapi")]
const SPFEI_END_INPUT_STREAM: u64 = 1 << SPEI_END_INPUT_STREAM.0;

#[derive(Debug)]
pub struct SapiEngine {
    voice: Mutex<Option<ISpVoice>>,
    voice_id: Mutex<Option<String>>,
    // Cached voice token so we don't re-enumerate registry tokens on every
    // speak call. `None` means "no explicit voice selected; use the
    // SAPI default".
    cached_token: Mutex<Option<ISpObjectToken>>,
    // Whether *we* called CoInitializeEx and must balance it with
    // CoUninitialize on Drop.
    com_initialized: Mutex<bool>,
}

unsafe impl Send for SapiEngine {}
unsafe impl Sync for SapiEngine {}

impl SapiEngine {
    pub fn new() -> Self {
        let (voice, com_initialized) = unsafe { Self::create_voice() };
        SapiEngine {
            voice: Mutex::new(voice),
            voice_id: Mutex::new(None),
            cached_token: Mutex::new(None),
            com_initialized: Mutex::new(com_initialized),
        }
    }
    /// Create the ISpVoice. Returns `(voice, did_init_com)`.
    unsafe fn create_voice() -> (Option<ISpVoice>, bool) {
        // COINIT_MULTITHREADED is the standard apartment model for non-UI
        // threads. We track whether *we* initialised COM so that Drop can
        // balance the reference count.
        let did_init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        // The windows 0.62 crate exposes the SpVoice COM class CLSID under
        // the unadorned name `SpVoice` (not `CLSID_SpVoice`).
        let voice = CoCreateInstance::<_, ISpVoice>(&SpVoice, None, CLSCTX_ALL).ok();
        (voice, did_init)
    }

    // windows-rs does not expose the sphelper.h `SpEnumTokens` inline helper, so
    // enumerate the voice category tokens directly through the COM interface.
    unsafe fn enum_voice_tokens() -> Result<IEnumSpObjectTokens> {
        let category: ISpObjectTokenCategory =
            CoCreateInstance(&SpObjectTokenCategory, None, CLSCTX_ALL)?;
        category.SetId(SPCAT_VOICES, false)?;
        category.EnumTokens(PCWSTR::null(), PCWSTR::null())
    }

    unsafe fn find_voice_by_id(voice_id: &str) -> Option<ISpObjectToken> {
        let enum_tokens = Self::enum_voice_tokens().ok()?;

        let mut count = 0u32;
        enum_tokens.GetCount(&raw mut count).ok()?;
        for i in 0..count {
            if let Ok(token) = enum_tokens.Item(i) {
                if let Ok(id) = token.GetId() {
                    let matches = id.to_string().is_ok_and(|s| s == voice_id);
                    if matches {
                        return Some(token);
                    }
                }
            }
        }
        None
    }

    /// Resolve the configured voice to a token, using the cache when possible
    ///. Acquires the registry-enumerating path only on cache miss.
    fn resolve_voice_token(&self, override_voice: Option<&str>) -> Option<ISpObjectToken> {
        let requested = override_voice
            .map(str::to_string)
            .or_else(|| self.voice_id.lock().unwrap().clone());

        let cached = self.cached_token.lock().unwrap();
        if let (Some(token), None) = (cached.as_ref(), requested.as_ref()) {
            return Some(token.clone());
        }
        drop(cached);

        let requested = requested?;
        // SAFETY: COM token enumeration; no borrowed references escape.
        let token = unsafe { Self::find_voice_by_id(&requested)? };
        *self.cached_token.lock().unwrap() = Some(token.clone());
        Some(token)
    }

    /// Drive `Speak` with `SPF_ASYNC` and pump `SPEI_WORD_BOUNDARY` events
    /// until the input stream ends, dispatching each boundary to `on_boundary`.
    ///
    /// `processed_text` is the text we actually fed to SAPI (possibly SSML).
    /// SAPI reports word positions in WCHAR units within the *parsed* text
    /// content, so when the input was SSML we strip tags before indexing.
    ///
    /// Audio byte offsets in the events are converted to milliseconds using
    /// the voice's current output format. We query the format up-front to
    /// avoid per-event overhead.
    fn speak_with_events(
        sp_voice: &ISpVoice,
        input_wide: &HSTRING,
        flags: u32,
        processed_text: &str,
        mut on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
    ) -> TtsResult<()> {
        // Some voices need the input to be passed with SPF_IS_XML so they
        // honour <mark> / <prosody> tags; the caller has already set that
        // bit. For event tracking we additionally need to make the voice
        // emit notifications.
        let event_source: ISpEventSource = sp_voice
            .cast()
            .map_err(|e| TtsError(format!("ISpVoice is not ISpEventSource: {e}")))?;
        let notify_source = sp_voice
            .cast::<windows::Win32::Media::Speech::ISpNotifySource>()
            .map_err(|e| TtsError(format!("ISpVoice is not ISpNotifySource: {e}")))?;

        unsafe {
            // Use a Win32 manual-reset event so we can WaitForSingleObject in
            // the pump loop below.
            notify_source
                .SetNotifyWin32Event()
                .map_err(|e| TtsError(format!("SetNotifyWin32Event failed: {e}")))?;

            // Ask SAPI to (a) signal us when these events arrive and (b)
            // queue them so GetEvents can return them.
            let interest = SPFEI_WORD_BOUNDARY | SPFEI_END_INPUT_STREAM;
            event_source
                .SetInterest(interest, interest)
                .map_err(|e| TtsError(format!("SetInterest failed: {e}")))?;

            // Speak asynchronously; the pump loop blocks until END_INPUT_STREAM.
            sp_voice
                .Speak(input_wide, flags, None)
                .map_err(|e| TtsError(format!("SAPI Speak failed: {e}")))?;

            // Pre-compute the audio-format → ms conversion. ullAudioStreamOffset
            // is in bytes; we want milliseconds.
            let bytes_per_ms = sapi_bytes_per_millisecond().max(1.0);

            // Pre-strip SSML so we can index into the visible text using the
            // positions SAPI reports. SAPI's word positions are in the parsed
            // content, not the raw input — strip_tags mirrors that.
            let visible_text = strip_ssml_tags(processed_text);
            let visible_wide: Vec<u16> = visible_text.encode_utf16().collect();

            let mut event = SPEVENT::default();
            let mut done = false;
            while !done {
                // Block until SAPI has something for us. 5 s is a generous
                // safety net — synthesis typically streams faster than this.
                let rc = notify_source.WaitForNotifyEvent(5_000);
                if rc.is_err() {
                    // Timeout or error: stop pumping and let the caller decide.
                    return Err(TtsError(
                        "SAPI notify wait timed out; aborting boundary pump".into(),
                    ));
                }

                // Drain every queued event before waiting again.
                loop {
                    let mut fetched = 0u32;
                    let rc = event_source.GetEvents(1, &raw mut event, &raw mut fetched);
                    if rc.is_err() || fetched == 0 {
                        break;
                    }

                    // SPEVENT.eEventId lives in the low 16 bits of the
                    // bitfield; the windows crate doesn't expose a helper, so
                    // mask manually. Values are SPEVENTENUM integers.
                    let event_id = event._bitfield & 0xFFFF;
                    #[allow(clippy::cast_precision_loss)]
                    let audio_offset_ms = (event.ullAudioStreamOffset as f64 / bytes_per_ms) as u64;

                    if event_id == SPEI_END_INPUT_STREAM.0 {
                        done = true;
                        break;
                    }

                    if event_id == SPEI_WORD_BOUNDARY.0 {
                        if let Some(cb) = on_boundary.as_mut() {
                            // wParam = char position in WCHARs; lParam = length in WCHARs.
                            // WPARAM/LPARAM wrap usize values on Windows.
                            let pos = event.wParam.0;
                            let len = event.lParam.0;
                            let word = word_at(&visible_wide, pos, len);
                            // Duration: we don't know it yet (it's the time
                            // until the next boundary); report a nominal
                            // 1 ms span. Callers that care can compute deltas.
                            #[allow(clippy::cast_precision_loss)]
                            let start_sec = audio_offset_ms as f32 / 1000.0;
                            #[allow(clippy::cast_precision_loss)]
                            let end_sec = (audio_offset_ms + 1) as f32 / 1000.0;
                            cb(&word, start_sec, end_sec);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Extract `len` WCHARs starting at `pos` from `text` and decode to a String.
/// Returns an empty string on out-of-bounds (defensive — SAPI shouldn't
/// produce one, but a buggy voice might).
fn word_at(text: &[u16], pos: usize, len: usize) -> String {
    let end = pos.saturating_add(len).min(text.len());
    if pos >= end {
        return String::new();
    }
    String::from_utf16_lossy(&text[pos..end])
}

/// Bytes-per-millisecond conversion for `ullAudioStreamOffset`. We assume the
/// SAPI default output format (22 kHz 16-bit mono = 44.1 bytes/ms). Refining
/// this requires querying `ISpAudio::GetFormat`, which is non-trivial through
/// windows-rs; the relative timing this gives is enough for word highlighting.
fn sapi_bytes_per_millisecond() -> f64 {
    22050.0 * 2.0 / 1000.0
}

/// Strip XML/SSML tags from `input`, leaving only the inner text. SAPI's
/// SPEI_WORD_BOUNDARY positions are reported in the parsed text content, so
/// we use this to map back to the original words.
fn strip_ssml_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rate_to_sapi(rate: f32) -> i32 {
    ((rate.clamp(0.1, 10.0) - 1.0) * 10.0).round() as i32
}

/// Map a rate multiplier (1.0 = normal) to a percentage string suitable for
/// SSML `<prosody pitch="+N%">`. Range: ±50% for rate 0.5–2.0.
fn pitch_to_percent(pitch: f32) -> String {
    let p = pitch.clamp(0.25, 4.0);
    let pct = ((p - 1.0) * 100.0).round() as i32;
    if pct >= 0 {
        format!("+{pct}%")
    } else {
        format!("{pct}%")
    }
}

fn volume_to_sapi(volume: f32) -> u16 {
    (volume.clamp(0.0, 2.0) * 50.0).round() as u16
}

impl TtsEngine for SapiEngine {
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        _on_audio: Option<crate::engine::OnAudioCallback>,
        on_boundary: Option<crate::engine::OnBoundaryCallback>,
    ) -> TtsResult<()> {
        let mut guard = self.voice.lock().unwrap();
        let sp_voice = guard
            .as_mut()
            .ok_or_else(|| TtsError("SAPI voice not initialized".into()))?;

        unsafe {
            if let Some(token) = self.resolve_voice_token(voice) {
                let _ = sp_voice.SetVoice(&token);
            }

            let _ = sp_voice.SetRate(rate_to_sapi(rate));
            let _ = sp_voice.SetVolume(volume_to_sapi(volume));

            // Run the input through SpeechMarkdown first; users can write
            // `Hello (world)[emphasis:"strong"]` and have it expanded to
            // SSML. SAPI accepts the SSML subset that speechmarkdown-rust
            // emits (Alexa flavour), so we pass it through with SPF_IS_XML.
            // `is_ssml` also short-circuits when the user passed raw SSML.
            let (processed, is_ssml) = preprocess_speech_markdown(text, "sapi");
            let needs_pitch = (pitch - 1.0).abs() > f32::EPSILON;

            // Decide whether the caller wants real word boundaries. If so we
            // take the SPF_ASYNC + event-pump path so we can dispatch
            // SPEI_WORD_BOUNDARY events as each word is spoken. Otherwise we
            // can let SAPI block on Speak and skip the event plumbing.
            let want_real_boundaries = on_boundary.is_some();

            // We pass the (possibly SSML) input to speak_with_events so it
            // can map SAPI's WCHAR positions back to the spoken text. Keep a
            // clone because HSTRING::from below moves `processed`.
            let processed_for_events = processed.clone();

            // Build the final input buffer + flags.
            let (input_wide, flags) = if is_ssml {
                let final_ssml = if needs_pitch {
                    let pitch_attr = pitch_to_percent(pitch);
                    processed
                        .replacen(
                            "<speak",
                            &format!("<speak><prosody pitch=\"{pitch_attr}\""),
                            1,
                        )
                        .replacen("</speak>", "</prosody></speak>", 1)
                } else {
                    processed
                };
                (
                    HSTRING::from(&final_ssml),
                    (SPF_ASYNC.0 | SPF_IS_XML.0) as u32,
                )
            } else if needs_pitch {
                let pitch_attr = pitch_to_percent(pitch);
                let escaped = processed
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                let ssml = format!(
                    "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\"\
                     xml:lang=\"en-US\"><prosody pitch=\"{pitch_attr}\">{escaped}</prosody></speak>"
                );
                (HSTRING::from(&ssml), (SPF_ASYNC.0 | SPF_IS_XML.0) as u32)
            } else {
                (HSTRING::from(&processed), SPF_ASYNC.0 as u32)
            };

            if want_real_boundaries {
                Self::speak_with_events(
                    sp_voice,
                    &input_wide,
                    flags,
                    &processed_for_events,
                    on_boundary,
                )?;
            } else {
                sp_voice
                    .Speak(&input_wide, flags, None)
                    .map_err(|e| TtsError(format!("SAPI Speak failed: {e}")))?;
            }
        }

        // If the caller didn't ask for real boundaries, we still synthesise
        // synchronously above; fall back to the estimate path so they get
        // *something* (the FFI boundary callback is invoked from lib.rs even
        // when the underlying engine didn't surface real events).
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
        let guard = self.voice.lock().unwrap();
        if let Some(sp_voice) = guard.as_ref() {
            unsafe {
                let _ = sp_voice.Speak(
                    &HSTRING::new(),
                    (SPF_ASYNC.0 | SPF_PURGEBEFORESPEAK.0) as u32,
                    None,
                );
            }
        }
        Ok(())
    }

    fn pause(&self) -> TtsResult<()> {
        let guard = self.voice.lock().unwrap();
        if let Some(sp_voice) = guard.as_ref() {
            unsafe {
                let _ = sp_voice.Pause();
            }
        }
        Ok(())
    }

    fn resume(&self) -> TtsResult<()> {
        let guard = self.voice.lock().unwrap();
        if let Some(sp_voice) = guard.as_ref() {
            unsafe {
                let _ = sp_voice.Resume();
            }
        }
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        let tokens = unsafe {
            Self::enum_voice_tokens()
                .map_err(|e| TtsError(format!("Failed to enumerate SAPI voices: {e}")))?
        };

        let mut count = 0u32;
        unsafe { tokens.GetCount(&raw mut count) }
            .map_err(|e| TtsError(format!("Failed to get voice count: {e}")))?;
        let count = count as usize;

        let mut voices = Vec::with_capacity(count);
        for i in 0..count {
            if let Ok(token) = unsafe { tokens.Item(i as u32) } {
                let id = unsafe { token.GetId().map(|h| h.to_hstring().to_string_lossy()) }
                    .unwrap_or_default();

                let name = unsafe {
                    token
                        .GetStringValue(windows::core::w!("Name"))
                        .map_or_else(|_| id.clone(), |h| h.to_hstring().to_string_lossy())
                };

                let lang = unsafe {
                    token
                        .GetStringValue(windows::core::w!("Language"))
                        .map_or_else(|_| "en-US".into(), |h| h.to_hstring().to_string_lossy())
                };

                let gender_str = unsafe {
                    token
                        .GetStringValue(windows::core::w!("Gender"))
                        .map_or_else(|_| String::new(), |h| h.to_hstring().to_string_lossy())
                };

                voices.push(Voice {
                    id: id.clone(),
                    name,
                    gender: crate::types::normalize_gender(&gender_str),
                    provider: "sapi".to_string(),
                    language_codes: vec![crate::types::LanguageCode {
                        bcp47: lang.clone(),
                        iso639_3: lang.split('-').next().unwrap_or("en").to_string(),
                        display: lang,
                    }],
                });
            }
        }
        Ok(voices)
    }

    fn engine_id(&self) -> &'static str {
        "sapi"
    }
}

impl Drop for SapiEngine {
    fn drop(&mut self) {
        // Release the voice and cached token first while COM is still
        // initialised, then balance our CoInitializeEx with CoUninitialize
        //. COM refs are ref-counted per thread, so we only call
        // CoUninitialize if we successfully called CoInitializeEx.
        if let Ok(mut guard) = self.voice.lock() {
            *guard = None;
        }
        if let Ok(mut cache) = self.cached_token.lock() {
            *cache = None;
        }
        if let Ok(mut com) = self.com_initialized.lock() {
            if *com {
                // SAFETY: matching the CoInitializeEx call from new().
                unsafe {
                    CoUninitialize();
                }
                *com = false;
            }
        }
    }
}
