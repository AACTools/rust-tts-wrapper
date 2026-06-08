use crate::engine::{estimate_word_boundaries, TtsEngine};
use crate::types::{TtsError, TtsResult, Voice};
use std::ffi::CStr;
use std::os::raw::c_void;
use std::ptr;
use std::sync::{Arc, Mutex};

extern "C" {
    fn avsynth_create() -> *mut c_void;
    fn avsynth_destroy(handle: *mut c_void);
    fn avsynth_speak(
        handle: *mut c_void,
        text: *const u8,
        voice_id: *const u8,
        rate: f32,
        pitch: f32,
        volume: f32,
    );
    fn avsynth_stop(handle: *mut c_void);
    fn avsynth_pause(handle: *mut c_void);
    fn avsynth_resume(handle: *mut c_void);
    fn avsynth_voice_count(handle: *mut c_void) -> i32;
    fn avsynth_get_voice(
        handle: *mut c_void,
        index: i32,
        id_buf: *mut u8,
        id_buf_len: i32,
        name_buf: *mut u8,
        name_buf_len: i32,
        lang_buf: *mut u8,
        lang_buf_len: i32,
    ) -> i32;
}

#[derive(Debug)]
pub struct AvSynthEngine {
    handle: Arc<Mutex<*mut c_void>>,
    voice_id: Mutex<Option<String>>,
}

unsafe impl Send for AvSynthEngine {}
unsafe impl Sync for AvSynthEngine {}

impl AvSynthEngine {
    pub fn new() -> Self {
        let handle = unsafe { avsynth_create() };
        AvSynthEngine {
            #[allow(clippy::arc_with_non_send_sync)]
            handle: Arc::new(Mutex::new(handle)),
            voice_id: Mutex::new(None),
        }
    }
}

impl TtsEngine for AvSynthEngine {
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
        let guard = self.handle.lock().unwrap();
        if guard.is_null() {
            return Err(TtsError("AVSpeechSynthesizer not initialized".into()));
        }

        let voice_to_use = voice
            .map(std::string::ToString::to_string)
            .or_else(|| self.voice_id.lock().unwrap().clone());

        unsafe {
            let text_c = text.as_ptr();
            let voice_c = voice_to_use.as_ref().map_or(ptr::null(), |v| v.as_ptr());
            avsynth_speak(*guard, text_c, voice_c, rate, pitch, volume);
        }

        if let Some(cb) = on_boundary.as_mut() {
            for b in &estimate_word_boundaries(text) {
                #[allow(clippy::cast_precision_loss)]
                cb(
                    &b.text,
                    b.offset as f32 / 1000.0,
                    (b.offset + b.duration) as f32 / 1000.0,
                );
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
        let guard = self.handle.lock().unwrap();
        if !guard.is_null() {
            unsafe { avsynth_stop(*guard) };
        }
        Ok(())
    }

    fn pause(&self) -> TtsResult<()> {
        let guard = self.handle.lock().unwrap();
        if !guard.is_null() {
            unsafe { avsynth_pause(*guard) };
        }
        Ok(())
    }

    fn resume(&self) -> TtsResult<()> {
        let guard = self.handle.lock().unwrap();
        if !guard.is_null() {
            unsafe { avsynth_resume(*guard) };
        }
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        let guard = self.handle.lock().unwrap();
        if guard.is_null() {
            return Ok(vec![]);
        }

        let count = unsafe { avsynth_voice_count(*guard) };
        if count <= 0 {
            return Ok(vec![]);
        }

        let mut voices = Vec::with_capacity(count as usize);
        for i in 0..count {
            let mut id_buf = [0u8; 256];
            let mut name_buf = [0u8; 256];
            let mut lang_buf = [0u8; 64];
            let rc = unsafe {
                avsynth_get_voice(
                    *guard,
                    i,
                    id_buf.as_mut_ptr(),
                    id_buf.len() as i32,
                    name_buf.as_mut_ptr(),
                    name_buf.len() as i32,
                    lang_buf.as_mut_ptr(),
                    lang_buf.len() as i32,
                )
            };
            if rc == 0 {
                let id = unsafe { CStr::from_ptr(id_buf.as_ptr().cast()) }
                    .to_string_lossy()
                    .into_owned();
                let name = unsafe { CStr::from_ptr(name_buf.as_ptr().cast()) }
                    .to_string_lossy()
                    .into_owned();
                let lang = unsafe { CStr::from_ptr(lang_buf.as_ptr().cast()) }
                    .to_string_lossy()
                    .into_owned();
                voices.push(Voice {
                    id,
                    name,
                    gender: crate::types::Gender::Unknown,
                    provider: "avsynth".to_string(),
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
        "avsynth"
    }
}

impl Drop for AvSynthEngine {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.handle.lock() {
            if !guard.is_null() {
                unsafe { avsynth_destroy(*guard) };
                *guard = ptr::null_mut();
            }
        }
    }
}
