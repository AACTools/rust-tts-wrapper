//! macOS AVSpeechSynthesizer engine via raw objc calls.

use crate::engine::{estimate_word_boundaries, TtsEngine};
use crate::types::{TtsError, TtsResult, Voice};
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use std::ffi::c_void;
use std::panic::catch_unwind;
use std::sync::{Arc, Mutex};

struct AutoreleasePool(*mut Object);

impl AutoreleasePool {
    unsafe fn new() -> Self {
        let cls = class!(NSAutoreleasePool);
        let pool: *mut Object = msg_send![cls, alloc];
        let pool: *mut Object = msg_send![pool, init];
        AutoreleasePool(pool)
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _: () = msg_send![self.0, drain];
            }
        }
    }
}

#[derive(Debug)]
pub struct AvSynthEngine {
    synth: Arc<Mutex<Option<*mut Object>>>,
    voice_id: Mutex<Option<String>>,
}

unsafe impl Send for AvSynthEngine {}
unsafe impl Sync for AvSynthEngine {}

impl AvSynthEngine {
    pub fn new() -> Self {
        let synth = unsafe {
            let _pool = AutoreleasePool::new();
            let cls = class!(AVSpeechSynthesizer);
            if cls.is_null() {
                return AvSynthEngine {
                    synth: Arc::new(Mutex::new(None)),
                    voice_id: Mutex::new(None),
                };
            }
            let obj: *mut Object = msg_send![cls, alloc];
            if obj.is_null() {
                return AvSynthEngine {
                    synth: Arc::new(Mutex::new(None)),
                    voice_id: Mutex::new(None),
                };
            }
            let obj: *mut Object = msg_send![obj, init];
            if obj.is_null() {
                None
            } else {
                Some(obj)
            }
        };
        AvSynthEngine {
            synth: Arc::new(Mutex::new(synth)),
            voice_id: Mutex::new(None),
        }
    }
}

pub fn is_available() -> bool {
    unsafe {
        let cls = class!(AVSpeechSynthesizer);
        !cls.is_null()
    }
}

unsafe fn to_nsstring(s: &str) -> *mut Object {
    let cls = class!(NSString);
    let bytes = s.as_ptr() as *const c_void;
    let len = s.len();
    let ns: *mut Object = msg_send![cls, alloc];
    let ns: *mut Object = msg_send![ns,
        initWithBytes: bytes
        length: len
        encoding: 4usize
    ];
    ns
}

unsafe fn from_nsstring(ns: *mut Object) -> String {
    if ns.is_null() {
        return String::new();
    }
    let len: usize = msg_send![ns, lengthOfBytesUsingEncoding: 4usize];
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u8; len];
    let _: usize = msg_send![ns,
        getBytes: buf.as_mut_ptr()
        maxLength: len
        encoding: 4usize
        options: 1usize
        range: (0usize, len)
        remainingRange: std::ptr::null::<(usize, usize)>()
    ];
    String::from_utf8_lossy(&buf).into_owned()
}

fn rate_to_avsynth(rate: f32) -> f32 {
    rate.clamp(0.1, 10.0)
}

fn pitch_to_avsynth(pitch: f32) -> f32 {
    pitch.clamp(0.5, 2.0)
}

fn volume_to_avsynth(volume: f32) -> f32 {
    volume.clamp(0.0, 1.0)
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
        let guard = self.synth.lock().unwrap();
        let synth = guard
            .ok_or_else(|| TtsError("AVSpeechSynthesizer not initialized".into()))?;

        unsafe {
            let _pool = AutoreleasePool::new();
            let ns_text = to_nsstring(text);
            let utterance_cls = class!(AVSpeechUtterance);
            let u: *mut Object = msg_send![utterance_cls, alloc];
            let u: *mut Object = msg_send![u, initWithString: ns_text];

            if !u.is_null() {
                let _: () = msg_send![u, setRate: rate_to_avsynth(rate)];
                let _: () = msg_send![u, setPitchMultiplier: pitch_to_avsynth(pitch)];
                let _: () = msg_send![u, setVolume: volume_to_avsynth(volume)];

                let voice_to_use = voice
                    .map(|v| v.to_string())
                    .or_else(|| self.voice_id.lock().unwrap().clone());

                if let Some(ref vid) = voice_to_use {
                    let ns_vid = to_nsstring(vid);
                    let voice_cls = class!(AVSpeechSynthesisVoice);
                    let av_voice: *mut Object = msg_send![voice_cls, voiceWithIdentifier: ns_vid];
                    if !av_voice.is_null() {
                        let _: () = msg_send![u, setVoice: av_voice];
                    }
                }

                let _: () = msg_send![synth, speakUtterance: u];
                let _: () = msg_send![u, release];
            }
            let _: () = msg_send![ns_text, release];
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
        let guard = self.synth.lock().unwrap();
        if let Some(synth) = *guard {
            unsafe {
                let _: () = msg_send![synth, stopSpeakingAtBoundary: 0i32];
            }
        }
        Ok(())
    }

    fn pause(&self) -> TtsResult<()> {
        let guard = self.synth.lock().unwrap();
        if let Some(synth) = *guard {
            unsafe {
                let _: () = msg_send![synth, pauseSpeakingAtBoundary: 0i32];
            }
        }
        Ok(())
    }

    fn resume(&self) -> TtsResult<()> {
        let guard = self.synth.lock().unwrap();
        if let Some(synth) = *guard {
            unsafe {
                let _: () = msg_send![synth, continueSpeaking];
            }
        }
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        unsafe {
            let _pool = AutoreleasePool::new();
            let voice_cls = class!(AVSpeechSynthesisVoice);
            let voices: *mut Object = msg_send![voice_cls, speechVoices];
            if voices.is_null() {
                return Ok(vec![]);
            }

            let count: usize = msg_send![voices, count];
            let mut result = Vec::with_capacity(count);

            for i in 0..count {
                let v: *mut Object = msg_send![voices, objectAtIndex: i];
                if !v.is_null() {
                    let id_ptr: *mut Object = msg_send![v, identifier];
                    let name_ptr: *mut Object = msg_send![v, name];
                    let lang_ptr: *mut Object = msg_send![v, language];

                    let id = from_nsstring(id_ptr);
                    let name = from_nsstring(name_ptr);
                    let lang = from_nsstring(lang_ptr);

                    result.push(Voice {
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
            Ok(result)
        }
    }

    fn engine_id(&self) -> &'static str {
        "avsynth"
    }
}

impl Drop for AvSynthEngine {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.synth.lock() {
            if let Some(ptr) = guard.take() {
                unsafe {
                    let _: () = msg_send![ptr, release];
                }
            }
        }
    }
}
