//! Windows SAPI (Speech API) engine.

use crate::engine::{estimate_word_boundaries, preprocess_speech_markdown, TtsEngine};
use crate::types::{TtsError, TtsResult, Voice};
use std::sync::Mutex;

// Explicit imports rather than wildcard — clippy rejects `use foo::*` for
// non-prelude modules. The `windows` crate exposes a lot of SAPI types so
// the list is long but unambiguous.
#[cfg(feature = "sapi")]
use windows::{
    core::{Interface, Result, HSTRING, PCWSTR},
    Win32::Media::Audio::WAVEFORMATEX,
    Win32::Media::Speech::{
        IEnumSpObjectTokens, ISpEventSource, ISpObjectToken, ISpObjectTokenCategory,
        ISpStreamFormat, ISpVoice, SpMemoryStream, SpObjectTokenCategory, SpVoice, SPCAT_VOICES,
        SPEI_END_INPUT_STREAM, SPEI_WORD_BOUNDARY, SPEVENT, SPF_ASYNC, SPF_IS_XML,
        SPF_PURGEBEFORESPEAK,
    },
    Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IStream, CLSCTX_ALL,
        COINIT_MULTITHREADED, STREAM_SEEK_SET,
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
            if event_source.SetInterest(interest, interest).is_err() {
                // Classic SPEI events aren't supported — notably by the modern
                // "Microsoft Natural"/Edge voices, whose ISpEventSource rejects
                // the interest mask with E_INVALIDARG. Fall back to a
                // synchronous speak and deliver estimated word boundaries so
                // callers still get highlighting rather than a hard failure.
                let sync_flags = flags & !(SPF_ASYNC.0 as u32);
                sp_voice
                    .Speak(input_wide, sync_flags, None)
                    .map_err(|e| TtsError(format!("SAPI Speak failed: {e}")))?;
                if let Some(cb) = on_boundary.as_mut() {
                    let visible = strip_ssml_tags(processed_text);
                    for b in estimate_word_boundaries(&visible) {
                        #[allow(clippy::cast_precision_loss)]
                        let start = b.offset as f32 / 1000.0;
                        #[allow(clippy::cast_precision_loss)]
                        let end = (b.offset + b.duration) as f32 / 1000.0;
                        cb(&b.text, start, end, -1, -1);
                    }
                }
                return Ok(());
            }

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
                            // wParam = char position in WCHARs (WPARAM wraps usize);
                            // lParam = length in WCHARs (LPARAM wraps isize).
                            let pos = event.wParam.0;
                            let len = event.lParam.0.max(0) as usize;
                            let word = word_at(&visible_wide, pos, len);
                            // Duration: we don't know it yet (it's the time
                            // until the next boundary); report a nominal
                            // 1 ms span. Callers that care can compute deltas.
                            #[allow(clippy::cast_precision_loss)]
                            let start_sec = audio_offset_ms as f32 / 1000.0;
                            #[allow(clippy::cast_precision_loss)]
                            let end_sec = (audio_offset_ms + 1) as f32 / 1000.0;
                            cb(&word, start_sec, end_sec, pos as i32, len as i32);
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

/// Build the SAPI input buffer and report whether it should be parsed as XML.
///
/// Runs SpeechMarkdown preprocessing and folds a non-normal pitch into an
/// SSML `<prosody>` wrapper (mirrors the engines that emit SSML). Returns
/// `(input_wide, is_xml, processed_text)` where `processed_text` is the
/// post-SpeechMarkdown string used by the boundary-event position mapper.
fn build_sapi_input(text: &str, pitch: f32) -> (HSTRING, bool, String) {
    let (processed, is_ssml) = preprocess_speech_markdown(text, "sapi");
    let needs_pitch = (pitch - 1.0).abs() > f32::EPSILON;

    let (input, is_xml) = if is_ssml {
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
            processed.clone()
        };
        (HSTRING::from(&final_ssml), true)
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
        (HSTRING::from(&ssml), true)
    } else {
        (HSTRING::from(&processed), false)
    };

    (input, is_xml, processed)
}

/// Wrap raw audio samples in a minimal WAV header described by `fmt`.
///
/// SAPI renders PCM (wFormatTag == 1); for that case the `fmt ` chunk is the
/// canonical 16 bytes. Non-PCM formats (rare for SAPI voices) emit the
/// WAVEFORMATEX fields plus the `cbSize` count so the file is at least
/// structurally a WAVEFORMATEX-formatted chunk.
fn build_wav(fmt: &WAVEFORMATEX, pcm: &[u8]) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let is_pcm = fmt.wFormatTag == 1;
    let fmt_chunk_size: u32 = if is_pcm {
        16
    } else {
        18 + u32::from(fmt.cbSize)
    };
    // "WAVE" + ("fmt " chunk) + ("data" chunk).
    let riff_size: u32 = 4 + (8 + fmt_chunk_size) + (8 + data_len);

    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&fmt_chunk_size.to_le_bytes());
    out.extend_from_slice(&fmt.wFormatTag.to_le_bytes());
    out.extend_from_slice(&fmt.nChannels.to_le_bytes());
    out.extend_from_slice(&fmt.nSamplesPerSec.to_le_bytes());
    out.extend_from_slice(&fmt.nAvgBytesPerSec.to_le_bytes());
    out.extend_from_slice(&fmt.nBlockAlign.to_le_bytes());
    out.extend_from_slice(&fmt.wBitsPerSample.to_le_bytes());
    if !is_pcm {
        out.extend_from_slice(&fmt.cbSize.to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
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

            // Decide whether the caller wants real word boundaries. If so we
            // take the SPF_ASYNC + event-pump path so we can dispatch
            // SPEI_WORD_BOUNDARY events as each word is spoken. Otherwise we
            // can let SAPI block on Speak and skip the event plumbing.
            let want_real_boundaries = on_boundary.is_some();

            // Build the SSML/plain input. `processed_for_events` is the
            // post-SpeechMarkdown text that speak_with_events needs to map
            // SAPI's WCHAR positions back to the spoken words.
            let (input_wide, is_xml, processed_for_events) = build_sapi_input(text, pitch);
            let mut flags = SPF_ASYNC.0;
            if is_xml {
                flags |= SPF_IS_XML.0;
            }
            let flags = flags as u32;

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

    /// Render speech to a WAV byte buffer instead of playing it.
    ///
    /// The default `synth_to_bytes` collects chunks from the `on_audio`
    /// callback, but SAPI's `speak` plays to the default audio device and
    /// never invokes that callback, so we must capture the stream ourselves.
    /// We redirect a throwaway `ISpVoice`'s output to a `SpMemoryStream`,
    /// speak synchronously, read the PCM back, and wrap it in a WAV header
    /// using the stream's reported `WAVEFORMATEX`.
    fn synth_to_bytes(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
    ) -> TtsResult<Vec<u8>> {
        unsafe {
            // A dedicated voice keeps the speak() voice's default-audio
            // output untouched and avoids save/restore of the output stream.
            let sp_voice: ISpVoice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL)
                .map_err(|e| TtsError(format!("Failed to create ISpVoice for synthesis: {e}")))?;

            if let Some(token) = self.resolve_voice_token(voice) {
                let _ = sp_voice.SetVoice(&token);
            }
            let _ = sp_voice.SetRate(rate_to_sapi(rate));
            let _ = sp_voice.SetVolume(volume_to_sapi(volume));

            // In-memory stream that captures the rendered PCM.
            let mem_stream: IStream = CoCreateInstance(&SpMemoryStream, None, CLSCTX_ALL)
                .map_err(|e| TtsError(format!("Failed to create SpMemoryStream: {e}")))?;
            sp_voice
                .SetOutput(&mem_stream, true)
                .map_err(|e| TtsError(format!("SAPI SetOutput failed: {e}")))?;

            let (input_wide, is_xml, _) = build_sapi_input(text, pitch);
            // Synchronous (no SPF_ASYNC) — Speak blocks until all audio is
            // written to the stream.
            let flags: u32 = if is_xml { SPF_IS_XML.0 as u32 } else { 0 };
            sp_voice
                .Speak(&input_wide, flags, None)
                .map_err(|e| TtsError(format!("SAPI Speak failed: {e}")))?;

            // Query the captured audio format so we can build a valid header.
            let fmt_stream: ISpStreamFormat = mem_stream
                .cast()
                .map_err(|e| TtsError(format!("Stream is not ISpStreamFormat: {e}")))?;
            // GetFormat writes the format GUID into `format_guid` and returns
            // a CoTaskMemAlloc'd WAVEFORMATEX; both need valid storage.
            let format_guid = windows::core::GUID::zeroed();
            let format_ptr = fmt_stream
                .GetFormat(&raw const format_guid)
                .map_err(|e| TtsError(format!("SAPI GetFormat failed: {e}")))?;

            // Speak leaves the cursor at end-of-stream; rewind before reading.
            let mut new_pos = 0u64;
            mem_stream
                .Seek(0, STREAM_SEEK_SET, Some(&raw mut new_pos))
                .map_err(|e| TtsError(format!("SAPI stream seek failed: {e}")))?;

            let mut pcm = Vec::new();
            let mut chunk = [0u8; 16_384];
            loop {
                let mut read = 0u32;
                mem_stream
                    .Read(
                        chunk.as_mut_ptr() as *mut _,
                        chunk.len() as u32,
                        Some(&raw mut read),
                    )
                    .ok()
                    .map_err(|e| TtsError(format!("SAPI stream read failed: {e}")))?;
                if read == 0 {
                    break;
                }
                pcm.extend_from_slice(&chunk[..read as usize]);
            }

            let wav = build_wav(&*format_ptr, &pcm);
            CoTaskMemFree(Some(format_ptr as *const _));
            Ok(wav)
        }
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
                        display: crate::types::locale_display_name(&lang),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_to_sapi_normal_is_zero() {
        assert_eq!(rate_to_sapi(1.0), 0);
    }

    #[test]
    fn test_rate_to_sapi_maps_to_tens() {
        // SPI_RATE uses 10x multiplier: rate 1.5 -> +5, rate 2.0 -> +10.
        assert_eq!(rate_to_sapi(1.5), 5);
        assert_eq!(rate_to_sapi(2.0), 10);
        assert_eq!(rate_to_sapi(0.5), -5);
    }

    #[test]
    fn test_rate_to_sapi_clamps_extremes() {
        assert_eq!(rate_to_sapi(100.0), rate_to_sapi(10.0));
        assert_eq!(rate_to_sapi(0.0), rate_to_sapi(0.1));
    }

    #[test]
    fn test_pitch_to_percent_zero_at_normal() {
        assert_eq!(pitch_to_percent(1.0), "+0%");
    }

    #[test]
    fn test_pitch_to_percent_positive_has_plus() {
        assert_eq!(pitch_to_percent(1.5), "+50%");
        assert_eq!(pitch_to_percent(2.0), "+100%");
    }

    #[test]
    fn test_pitch_to_percent_negative_no_leading_plus() {
        // Negative percentages must not be written as "+-50%".
        let p = pitch_to_percent(0.5);
        assert!(!p.starts_with("+-"));
        assert_eq!(p, "-50%");
    }

    #[test]
    fn test_pitch_to_percent_clamps() {
        // Outside [0.25, 4.0] must clamp, not overflow.
        assert_eq!(pitch_to_percent(10.0), pitch_to_percent(4.0));
        assert_eq!(pitch_to_percent(0.0), pitch_to_percent(0.25));
    }

    #[test]
    fn test_volume_to_sapi_normal() {
        // SAPI volume is 0..100; multiplier 1.0 maps to 50 (middle).
        assert_eq!(volume_to_sapi(1.0), 50);
    }

    #[test]
    fn test_volume_to_sapi_max_and_min() {
        assert_eq!(volume_to_sapi(2.0), 100);
        assert_eq!(volume_to_sapi(0.0), 0);
    }

    #[test]
    fn test_volume_to_sapi_clamps_above_two() {
        assert_eq!(volume_to_sapi(5.0), 100);
    }

    #[test]
    fn test_strip_ssml_tags_collapses_inner_whitespace() {
        // split_whitespace() in strip_ssml_tags collapses runs of
        // whitespace into single spaces, so "Hello   world" becomes
        // "Hello world" — that's the documented behaviour, not a bug.
        assert_eq!(strip_ssml_tags("<p>Hello   world</p>"), "Hello world");
    }

    #[test]
    fn test_strip_ssml_tags_collapses_outer_whitespace() {
        // Leading/trailing whitespace between tags is collapsed via split_whitespace.
        assert_eq!(strip_ssml_tags("<speak>\n  Hello\n</speak>"), "Hello");
    }

    #[test]
    fn test_strip_ssml_tags_no_tags_passthrough() {
        assert_eq!(strip_ssml_tags("plain text"), "plain text");
    }

    #[test]
    fn test_strip_ssml_tags_unclosed_tag_does_not_hang() {
        // Defensive: a malformed '<' with no closing '>' just stops collection.
        assert_eq!(strip_ssml_tags("hello <world"), "hello");
    }

    #[test]
    fn test_word_at_basic_slice() {
        // UTF-16 buffer for "hello".
        let text: Vec<u16> = "hello".encode_utf16().collect();
        assert_eq!(word_at(&text, 0, 5), "hello");
        assert_eq!(word_at(&text, 1, 3), "ell");
    }

    #[test]
    fn test_word_at_out_of_bounds_returns_empty() {
        let text: Vec<u16> = "hi".encode_utf16().collect();
        assert_eq!(word_at(&text, 5, 3), "");
        assert_eq!(word_at(&text, 0, 100), "hi"); // clamped to end
    }

    #[test]
    fn test_word_at_zero_len_returns_empty() {
        let text: Vec<u16> = "hi".encode_utf16().collect();
        assert_eq!(word_at(&text, 0, 0), "");
    }

    #[test]
    fn test_word_at_surrogate_pair() {
        // U+1F600 (😀) is a surrogate pair in UTF-16; from_utf16_lossy must
        // reconstruct it instead of returning two replacement chars.
        let text: Vec<u16> = "😀".encode_utf16().collect();
        assert_eq!(text.len(), 2);
        assert_eq!(word_at(&text, 0, 2), "😀");
    }

    #[test]
    fn test_sapi_bytes_per_millisecond_known_value() {
        // 22 kHz * 16-bit mono = 44,100 bytes/sec = 44.1 bytes/ms.
        let bpm = sapi_bytes_per_millisecond();
        assert!((bpm - 44.1).abs() < 0.001);
    }

    #[test]
    fn test_build_wav_emits_valid_pcm_header() {
        // SAPI's default output: 22050 Hz, 16-bit, mono PCM.
        let fmt = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 1,
            nSamplesPerSec: 22050,
            nAvgBytesPerSec: 44100,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let pcm = vec![0u8; 100]; // 50 silent samples
        let wav = build_wav(&fmt, &pcm);

        // 44-byte header + payload.
        assert_eq!(wav.len(), 44 + 100);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        // RIFF size = 4 ("WAVE") + 8+16 (fmt) + 8+100 (data) = 136.
        assert_eq!(u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]), 136);
        // fmt chunk size is 16 for PCM, format tag is 1 (PCM).
        assert_eq!(u32::from_le_bytes([wav[16], wav[17], wav[18], wav[19]]), 16);
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1);
        // data length matches the input.
        assert_eq!(
            u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]),
            100
        );
    }

    #[test]
    fn test_build_wav_non_pcm_includes_cbsize() {
        // A non-PCM tag (e.g. 0x0002 ADPCM) must include the cbSize field,
        // growing the fmt chunk to 18 bytes.
        let fmt = WAVEFORMATEX {
            wFormatTag: 2,
            nChannels: 1,
            nSamplesPerSec: 8000,
            nAvgBytesPerSec: 4096,
            nBlockAlign: 256,
            wBitsPerSample: 4,
            cbSize: 0,
        };
        let wav = build_wav(&fmt, &[1, 2, 3]);
        // fmt chunk size = 18 for non-PCM (16 base + 2 for cbSize).
        assert_eq!(u32::from_le_bytes([wav[16], wav[17], wav[18], wav[19]]), 18);
    }
}
