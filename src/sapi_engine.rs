//! Windows SAPI (Speech API) engine.

use crate::engine::{estimate_word_boundaries, TtsEngine};
use crate::types::{TtsError, TtsResult, Voice};
use std::sync::Mutex;

// Explicit imports rather than wildcard — clippy rejects `use foo::*` for
// non-prelude modules. The `windows` crate exposes a lot of SAPI types so
// the list is long but unambiguous.
#[cfg(feature = "sapi")]
use windows::{
    core::{Result, HSTRING, PCWSTR},
    Win32::Media::Speech::{
        IEnumSpObjectTokens, ISpObjectToken, ISpObjectTokenCategory, ISpVoice,
        SpObjectTokenCategory, SpVoice, SPCAT_VOICES, SPF_ASYNC, SPF_IS_XML, SPF_PURGEBEFORESPEAK,
    },
    Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    },
};

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
        mut on_boundary: Option<crate::engine::OnBoundaryCallback>,
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

            // Build SSML only when we need pitch (avoids unnecessary XML
            // parsing for the common case). Use standard `<prosody pitch=...>`
            // rather than the non-standard `<pitch absmiddle>`.
            if (pitch - 1.0).abs() > f32::EPSILON {
                let pitch_attr = pitch_to_percent(pitch);
                let escaped = text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                let ssml = format!(
                    "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\"\
                     xml:lang=\"en-US\"><prosody pitch=\"{pitch_attr}\">{escaped}</prosody></speak>"
                );
                let wtext = HSTRING::from(&ssml);
                // Surface Speak failures instead of swallowing them.
                sp_voice
                    .Speak(&wtext, (SPF_ASYNC.0 | SPF_IS_XML.0) as u32, None)
                    .map_err(|e| TtsError(format!("SAPI Speak failed: {e}")))?;
            } else {
                let wtext = HSTRING::from(text);
                sp_voice
                    .Speak(&wtext, SPF_ASYNC.0 as u32, None)
                    .map_err(|e| TtsError(format!("SAPI Speak failed: {e}")))?;
            }
        }

        if let Some(cb) = on_boundary.as_mut() {
            let estimated = estimate_word_boundaries(text);
            for b in &estimated {
                #[allow(clippy::cast_precision_loss)]
                let start = b.offset as f32 / 1000.0;
                #[allow(clippy::cast_precision_loss)]
                let end = (b.offset + b.duration) as f32 / 1000.0;
                cb(&b.text, start, end);
            }
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
                        .map_or_else(String::new, |h| h.to_hstring().to_string_lossy())
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
