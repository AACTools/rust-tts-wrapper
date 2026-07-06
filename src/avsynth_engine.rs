use crate::engine::{estimate_word_boundaries, preprocess_speech_markdown, TtsEngine};
use crate::types::{TtsError, TtsResult, Voice};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};

extern "C" {
    fn avsynth_create() -> *mut c_void;
    fn avsynth_destroy(handle: *mut c_void);
    fn avsynth_speak(
        handle: *mut c_void,
        text: *const c_char,
        voice_id: *const c_char,
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
        id_buf: *mut c_char,
        id_buf_len: i32,
        name_buf: *mut c_char,
        name_buf_len: i32,
        lang_buf: *mut c_char,
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

        // Run the input through SpeechMarkdown. AVSpeechSynthesizer itself
        // doesn't parse SSML, so when the input (or SpeechMarkdown expansion)
        // yields an SSML document we strip the tags and pass the inner text —
        // this matches what js-tts-wrapper does for the avsynth engine. The
        // upshot is that callers can write SpeechMarkdown on macOS and have
        // it Just Work, with the prosody directives applied via the utterance
        // rate/pitch/volume parameters above.
        let (processed, is_ssml) = preprocess_speech_markdown(text, "avsynth");
        let spoken_text = if is_ssml {
            strip_ssml_tags(&processed)
        } else {
            processed
        };

        unsafe {
            // CString::new fails if the input contains an interior NUL. For TTS
            // text and voice ids that is an error we should surface rather than
            // silently truncate or read past the end of the buffer.
            let text_c = CString::new(spoken_text.as_str())
                .map_err(|_| TtsError("text contains interior NUL byte".into()))?;
            let voice_c = match voice_to_use.as_deref() {
                Some(v) => Some(
                    CString::new(v)
                        .map_err(|_| TtsError("voice id contains interior NUL byte".into()))?,
                ),
                None => None,
            };
            avsynth_speak(
                *guard,
                text_c.as_ptr(),
                voice_c.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
                rate,
                pitch,
                volume,
            );
        }

        if let Some(cb) = on_boundary.as_mut() {
            for b in &estimate_word_boundaries(text) {
                #[allow(clippy::cast_precision_loss)]
                cb(
                    &b.text,
                    b.offset as f32 / 1000.0,
                    (b.offset + b.duration) as f32 / 1000.0,
                    -1,
                    -1,
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
                    id_buf.as_mut_ptr().cast(),
                    id_buf.len() as i32,
                    name_buf.as_mut_ptr().cast(),
                    name_buf.len() as i32,
                    lang_buf.as_mut_ptr().cast(),
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

/// Strip XML/SSML tags from `input`, leaving only the inner text.
///
/// AVSpeechSynthesizer doesn't have an SSML parser, so when a caller passes
/// SSML (or SpeechMarkdown that expands to SSML) we keep just the text
/// content. Prosody directives are lost — the equivalent knobs (rate, pitch,
/// volume) are applied via the utterance parameters instead.
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
    // Collapse the run of whitespace that self-closing tags (e.g. `<break/>`)
    // leave behind.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
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
