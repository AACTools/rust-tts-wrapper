//! Windows SAPI (Speech API) engine.

use crate::engine::{estimate_word_boundaries, TtsEngine};
use crate::types::{TtsError, TtsResult, Voice};
use std::sync::Mutex;

#[cfg(feature = "sapi")]
use windows::{core::*, Win32::Media::Speech::*, Win32::System::Com::*};

#[derive(Debug)]
pub struct SapiEngine {
    voice: Mutex<Option<ISpVoice>>,
    voice_id: Mutex<Option<String>>,
}

unsafe impl Send for SapiEngine {}
unsafe impl Sync for SapiEngine {}

impl SapiEngine {
    pub fn new() -> Self {
        let voice = unsafe { Self::create_voice() };
        SapiEngine {
            voice: Mutex::new(voice),
            voice_id: Mutex::new(None),
        }
    }

    unsafe fn create_voice() -> Option<ISpVoice> {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().ok()?;
        CoCreateInstance::<_, ISpVoice>(&SPVOICE_CLSID, None, CLSCTX_ALL).ok()
    }

    // windows-rs does not expose the sphelper.h `SpEnumTokens` inline helper, so
    // enumerate the voice category tokens directly through the COM interface.
    unsafe fn enum_voice_tokens() -> Result<IEnumSpObjectTokens> {
        let category: ISpObjectTokenCategory =
            CoCreateInstance(&SPCATTOKENCATEGORY_CLSID, None, CLSCTX_ALL)?;
        category.SetId(SPCAT_VOICES, false)?;
        category.EnumTokens(PCWSTR::null(), PCWSTR::null())
    }

    unsafe fn find_voice_by_id(voice_id: &str) -> Option<ISpObjectToken> {
        let enum_tokens = Self::enum_voice_tokens().ok()?;

        let mut count = 0u32;
        enum_tokens.GetCount(&mut count).ok()?;
        for i in 0..count {
            if let Ok(token) = enum_tokens.Item(i) {
                if let Ok(id) = token.GetId() {
                    if id.to_string().map(|s| s == voice_id).unwrap_or(false) {
                        return Some(token);
                    }
                }
            }
        }
        None
    }
}

fn rate_to_sapi(rate: f32) -> i32 {
    ((rate.clamp(0.1, 10.0) - 1.0) * 10.0).round() as i32
}

fn pitch_to_sapi(pitch: f32) -> u32 {
    let val = ((pitch.clamp(0.1, 10.0) - 1.0) * 10.0).round() as i32;
    val.unsigned_abs()
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
            let voice_to_use = voice
                .map(|v| v.to_string())
                .or_else(|| self.voice_id.lock().unwrap().clone());

            if let Some(ref vid) = voice_to_use {
                if let Some(token) = Self::find_voice_by_id(vid) {
                    let _ = sp_voice.SetVoice(&token);
                }
            }

            let _ = sp_voice.SetRate(rate_to_sapi(rate));
            let _ = sp_voice.SetVolume(volume_to_sapi(volume));

            if (pitch - 1.0).abs() > f32::EPSILON {
                let pitch_val = pitch_to_sapi(pitch);
                let pitch_str = format!(
                    "<pitch absmiddle=\"{}\"/>",
                    if pitch >= 1.0 {
                        pitch_val as i32
                    } else {
                        -(pitch_val as i32)
                    }
                );
                let wrapped = format!("{pitch_str}{text}");
                let wtext = HSTRING::from(&wrapped);
                let _ = sp_voice.Speak(&wtext, (SPF_ASYNC.0 | SPF_IS_XML.0) as u32, None);
            } else {
                let wtext = HSTRING::from(text);
                let _ = sp_voice.Speak(&wtext, SPF_ASYNC.0 as u32, None);
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
        unsafe { tokens.GetCount(&mut count) }
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
                        .map(|h| h.to_hstring().to_string_lossy())
                        .unwrap_or_else(|_| id.clone())
                };

                let lang = unsafe {
                    token
                        .GetStringValue(windows::core::w!("Language"))
                        .map(|h| h.to_hstring().to_string_lossy())
                        .unwrap_or_else(|_| "en-US".into())
                };

                let gender_str = unsafe {
                    token
                        .GetStringValue(windows::core::w!("Gender"))
                        .map(|h| h.to_hstring().to_string_lossy())
                        .unwrap_or_default()
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
