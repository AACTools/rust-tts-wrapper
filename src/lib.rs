//! # rust-tts-wrapper
//!
//! Cross-platform TTS (Text-to-Speech) wrapper with a C ABI.
//! Mirrors [`js-tts-wrapper`] and `SwiftTTSWrapper`, supporting 21 engines:
//! system (speech-dispatcher), Sherpa-ONNX (191 local models), and 19 cloud providers.
//!
//! [`js-tts-wrapper`]: https://github.com/AACTools/js-tts-wrapper
//!
//! ## Quick start (C)
//!
//! ```c
//! tts_ctx* ctx = tts_create("system", NULL);
//! tts_speak(ctx, "Hello world");
//! tts_destroy(ctx);
//! ```

#![allow(
    clippy::missing_panics_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::ptr_as_ptr,
    clippy::cast_ptr_alignment,
    clippy::doc_markdown,
    clippy::multiple_crate_versions,
    clippy::field_reassign_with_default,
    non_camel_case_types,
    dead_code
)]

#[cfg(feature = "cloud")]
mod cloud_engine;
pub mod engine;
pub mod factory;
#[cfg(feature = "sherpaonnx")]
mod sherpaonnx_engine;
#[cfg(feature = "system")]
mod system_engine;
#[cfg(feature = "avsynth")]
mod avsynth_engine;
#[cfg(feature = "sapi")]
mod sapi_engine;
pub mod types;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::sync::Mutex;

use engine::TtsEngine;
use factory::create_engine;

type BoxedEngine = Box<dyn TtsEngine>;

/// Opaque context holding an engine instance and its per-instance settings.
pub type CAudioCb = Option<extern "C" fn(*const u8, usize, *mut std::ffi::c_void)>;
pub type CBoundaryCb = Option<extern "C" fn(*const c_char, f32, f32, *mut std::ffi::c_void)>;
type BoxedAudioCb = Box<dyn FnMut(&[u8])>;
type BoxedBoundaryCb = Box<dyn FnMut(&str, f32, f32)>;

pub struct tts_ctx {
    engine: Mutex<BoxedEngine>,
    voice_id: Mutex<Option<String>>,
    rate: Mutex<f32>,
    pitch: Mutex<f32>,
    volume: Mutex<f32>,
    last_error: Mutex<String>,
    on_audio: Mutex<CAudioCb>,
    on_audio_userdata: Mutex<*mut std::ffi::c_void>,
    on_boundary: Mutex<CBoundaryCb>,
    on_boundary_userdata: Mutex<*mut std::ffi::c_void>,
}

static LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);

fn set_error(msg: &str) {
    if let Ok(mut guard) = LAST_ERROR.lock() {
        *guard = Some(CString::new(msg).unwrap_or_else(|_| CString::new("error").unwrap()));
    }
}

/// Create a new TTS engine instance.
///
/// Returns an opaque context pointer on success, or null on failure.
/// Call [`tts_get_last_error`] to retrieve the error message on failure.
///
/// # Safety
///
/// `engine_id` must be a valid null-terminated C string.
/// `credentials_json` may be null or a valid null-terminated JSON string.
#[no_mangle]
pub extern "C" fn tts_create(
    engine_id: *const c_char,
    credentials_json: *const c_char,
) -> *mut tts_ctx {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tts_create_inner(engine_id, credentials_json)
    }));
    match result {
        Ok(ptr) => ptr,
        Err(_) => {
            set_error("engine creation panicked");
            ptr::null_mut()
        }
    }
}

fn tts_create_inner(
    engine_id: *const c_char,
    credentials_json: *const c_char,
) -> *mut tts_ctx {
    if engine_id.is_null() {
        set_error("engine_id is null");
        return ptr::null_mut();
    }
    let engine_id_str = unsafe { CStr::from_ptr(engine_id) }
        .to_string_lossy()
        .into_owned();
    let creds = if credentials_json.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(credentials_json) }
            .to_string_lossy()
            .into_owned()
    };

    if let Some(engine) = create_engine(&engine_id_str, &creds) {
        let ctx = Box::new(tts_ctx {
            engine: Mutex::new(engine),
            voice_id: Mutex::new(None),
            rate: Mutex::new(1.0),
            pitch: Mutex::new(1.0),
            volume: Mutex::new(1.0),
            last_error: Mutex::new(String::new()),
            on_audio: Mutex::new(None),
            on_audio_userdata: Mutex::new(ptr::null_mut()),
            on_boundary: Mutex::new(None),
            on_boundary_userdata: Mutex::new(ptr::null_mut()),
        });
        Box::into_raw(ctx)
    } else {
        set_error(&format!("Unknown engine: {engine_id_str}"));
        ptr::null_mut()
    }
}

/// Destroy a TTS context and free all associated resources.
///
/// # Safety
///
/// `ctx` must be a pointer previously returned by [`tts_create`],
/// or null (no-op).
#[no_mangle]
pub extern "C" fn tts_destroy(ctx: *mut tts_ctx) {
    if !ctx.is_null() {
        unsafe {
            drop(Box::from_raw(ctx));
        }
    }
}

/// Speak `text` asynchronously using the engine in `ctx`.
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `ctx` must be a valid pointer from [`tts_create`].
/// `text` must be a valid null-terminated C string.
#[no_mangle]
pub extern "C" fn tts_speak(ctx: *mut tts_ctx, text: *const c_char) -> i32 {
    if ctx.is_null() || text.is_null() {
        return -1;
    }
    let ctx_ref = unsafe { &*ctx };
    let text_str = unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    let voice = ctx_ref.voice_id.lock().unwrap().clone();
    let rate = *ctx_ref.rate.lock().unwrap();
    let pitch = *ctx_ref.pitch.lock().unwrap();
    let volume = *ctx_ref.volume.lock().unwrap();

    let audio_cb = *ctx_ref.on_audio.lock().unwrap();
    let audio_userdata = *ctx_ref.on_audio_userdata.lock().unwrap();
    let boundary_cb = *ctx_ref.on_boundary.lock().unwrap();
    let boundary_userdata = *ctx_ref.on_boundary_userdata.lock().unwrap();

    let mut on_audio_closure: Option<BoxedAudioCb> = match audio_cb {
        Some(cb) => Some(Box::new(move |bytes: &[u8]| {
            cb(bytes.as_ptr(), bytes.len(), audio_userdata);
        })),
        None => None,
    };

    let mut on_boundary_closure: Option<BoxedBoundaryCb> = match boundary_cb {
        Some(cb) => Some(Box::new(move |word: &str, start: f32, end: f32| {
            if let Ok(c_word) = CString::new(word) {
                cb(c_word.as_ptr(), start, end, boundary_userdata);
            }
        })),
        None => None,
    };

    let engine = ctx_ref.engine.lock().unwrap();
    match engine.speak(
        &text_str,
        voice.as_deref(),
        rate,
        pitch,
        volume,
        on_audio_closure
            .as_mut()
            .map(|f| &mut **f as &mut dyn FnMut(&[u8])),
        on_boundary_closure
            .as_mut()
            .map(|f| &mut **f as &mut dyn FnMut(&str, f32, f32)),
    ) {
        Ok(()) => 0,
        Err(e) => {
            *ctx_ref.last_error.lock().unwrap() = e.to_string();
            -1
        }
    }
}

/// Speak `text` synchronously (blocks until complete).
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `ctx` must be a valid pointer from [`tts_create`].
/// `text` must be a valid null-terminated C string.
#[no_mangle]
pub extern "C" fn tts_speak_sync(ctx: *mut tts_ctx, text: *const c_char) -> i32 {
    if ctx.is_null() || text.is_null() {
        return -1;
    }
    let ctx_ref = unsafe { &*ctx };
    let text_str = unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    let voice = ctx_ref.voice_id.lock().unwrap().clone();
    let rate = *ctx_ref.rate.lock().unwrap();
    let pitch = *ctx_ref.pitch.lock().unwrap();
    let volume = *ctx_ref.volume.lock().unwrap();

    let audio_cb = *ctx_ref.on_audio.lock().unwrap();
    let audio_userdata = *ctx_ref.on_audio_userdata.lock().unwrap();
    let boundary_cb = *ctx_ref.on_boundary.lock().unwrap();
    let boundary_userdata = *ctx_ref.on_boundary_userdata.lock().unwrap();

    let mut on_audio_closure: Option<BoxedAudioCb> = match audio_cb {
        Some(cb) => Some(Box::new(move |bytes: &[u8]| {
            cb(bytes.as_ptr(), bytes.len(), audio_userdata);
        })),
        None => None,
    };

    let mut on_boundary_closure: Option<BoxedBoundaryCb> = match boundary_cb {
        Some(cb) => Some(Box::new(move |word: &str, start: f32, end: f32| {
            if let Ok(c_word) = CString::new(word) {
                cb(c_word.as_ptr(), start, end, boundary_userdata);
            }
        })),
        None => None,
    };

    let engine = ctx_ref.engine.lock().unwrap();
    match engine.speak_sync(
        &text_str,
        voice.as_deref(),
        rate,
        pitch,
        volume,
        on_audio_closure
            .as_mut()
            .map(|f| &mut **f as &mut dyn FnMut(&[u8])),
        on_boundary_closure
            .as_mut()
            .map(|f| &mut **f as &mut dyn FnMut(&str, f32, f32)),
    ) {
        Ok(()) => 0,
        Err(e) => {
            *ctx_ref.last_error.lock().unwrap() = e.to_string();
            -1
        }
    }
}

/// Stop any in-progress speech.
///
/// # Safety
///
/// `ctx` must be a valid pointer from [`tts_create`].
#[no_mangle]
pub extern "C" fn tts_stop(ctx: *mut tts_ctx) {
    if ctx.is_null() {
        return;
    }
    let ctx_ref = unsafe { &*ctx };
    let engine = ctx_ref.engine.lock().unwrap();
    let _ = engine.stop();
}

/// Retrieve the list of available voices for the engine.
///
/// On success, writes a heap-allocated array to `*out_voices` and its length
/// to `*out_count`. Caller must free with [`tts_free_voices`].
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `ctx` must be valid. `out_voices` and `out_count` must be non-null.
#[no_mangle]
pub extern "C" fn tts_get_voices(
    ctx: *mut tts_ctx,
    out_voices: *mut *mut types::tts_voice,
    out_count: *mut i32,
) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tts_get_voices_inner(ctx, out_voices, out_count)
    }));
    match result {
        Ok(r) => r,
        Err(_) => -1,
    }
}

fn tts_get_voices_inner(
    ctx: *mut tts_ctx,
    out_voices: *mut *mut types::tts_voice,
    out_count: *mut i32,
) -> i32 {
    if ctx.is_null() || out_voices.is_null() || out_count.is_null() {
        return -1;
    }
    let ctx_ref = unsafe { &*ctx };
    let engine = ctx_ref.engine.lock().unwrap();
    match engine.get_voices() {
        Ok(voices) => {
            let len = voices.len();
            if len == 0 {
                unsafe {
                    *out_voices = ptr::null_mut();
                    *out_count = 0;
                }
                return 0;
            }
            let layout = std::alloc::Layout::array::<types::tts_voice>(len).unwrap();
            let arr_ptr = unsafe { std::alloc::alloc(layout).cast::<types::tts_voice>() };
            for (i, v) in voices.iter().enumerate() {
                unsafe {
                    let entry = arr_ptr.add(i);
                    std::ptr::write(
                        entry,
                        types::tts_voice {
                            id: CString::new(v.id.clone()).unwrap().into_raw(),
                            name: CString::new(v.name.clone()).unwrap().into_raw(),
                            language: CString::new(v.primary_language().to_string())
                                .unwrap()
                                .into_raw(),
                            gender: CString::new(v.gender.to_string()).unwrap().into_raw(),
                            engine: CString::new(v.provider.clone()).unwrap().into_raw(),
                        },
                    );
                }
            }
            unsafe {
                *out_voices = arr_ptr;
                *out_count = len as i32;
            }
            0
        }
        Err(e) => {
            *ctx_ref.last_error.lock().unwrap() = e.to_string();
            -1
        }
    }
}

/// Free a voice array previously returned by [`tts_get_voices`].
///
/// # Safety
///
/// `voices` must be a pointer from `tts_get_voices` with the matching `count`.
#[no_mangle]
pub extern "C" fn tts_free_voices(voices: *mut types::tts_voice, count: i32) {
    if voices.is_null() || count <= 0 {
        return;
    }
    for i in 0..count {
        unsafe {
            let v = voices.add(i as usize);
            if !(*v).id.is_null() {
                let _ = CString::from_raw((*v).id);
            }
            if !(*v).name.is_null() {
                let _ = CString::from_raw((*v).name);
            }
            if !(*v).language.is_null() {
                let _ = CString::from_raw((*v).language);
            }
            if !(*v).gender.is_null() {
                let _ = CString::from_raw((*v).gender);
            }
            if !(*v).engine.is_null() {
                let _ = CString::from_raw((*v).engine);
            }
        }
    }
    let layout = std::alloc::Layout::array::<types::tts_voice>(count as usize).unwrap();
    unsafe {
        std::alloc::dealloc(voices.cast::<u8>(), layout);
    }
}

/// Set the voice for subsequent speak calls.
///
/// # Safety
///
/// `ctx` must be valid. `voice_id` must be a valid null-terminated C string.
#[no_mangle]
pub extern "C" fn tts_set_voice(ctx: *mut tts_ctx, voice_id: *const c_char) {
    if ctx.is_null() || voice_id.is_null() {
        return;
    }
    let ctx_ref = unsafe { &*ctx };
    let id = unsafe { CStr::from_ptr(voice_id) }
        .to_string_lossy()
        .into_owned();
    *ctx_ref.voice_id.lock().unwrap() = Some(id);
}

/// Set the speech rate (1.0 = normal).
///
/// # Safety
///
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_rate(ctx: *mut tts_ctx, rate: f32) {
    if ctx.is_null() {
        return;
    }
    *unsafe { &*ctx }.rate.lock().unwrap() = rate;
}

/// Set the speech pitch (1.0 = normal).
///
/// # Safety
///
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_pitch(ctx: *mut tts_ctx, pitch: f32) {
    if ctx.is_null() {
        return;
    }
    *unsafe { &*ctx }.pitch.lock().unwrap() = pitch;
}

/// Set the speech volume (1.0 = normal).
///
/// # Safety
///
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_volume(ctx: *mut tts_ctx, volume: f32) {
    if ctx.is_null() {
        return;
    }
    *unsafe { &*ctx }.volume.lock().unwrap() = volume;
}

/// Set the callback for streaming audio chunks.
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_on_audio(
    ctx: *mut tts_ctx,
    cb: CAudioCb,
    userdata: *mut std::ffi::c_void,
) {
    if ctx.is_null() {
        return;
    }
    let ctx_ref = unsafe { &*ctx };
    *ctx_ref.on_audio.lock().unwrap() = cb;
    *ctx_ref.on_audio_userdata.lock().unwrap() = userdata;
}

/// Set the callback for word boundary events.
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_on_boundary(
    ctx: *mut tts_ctx,
    cb: CBoundaryCb,
    userdata: *mut std::ffi::c_void,
) {
    if ctx.is_null() {
        return;
    }
    let ctx_ref = unsafe { &*ctx };
    *ctx_ref.on_boundary.lock().unwrap() = cb;
    *ctx_ref.on_boundary_userdata.lock().unwrap() = userdata;
}

/// Return the number of registered engines.
#[no_mangle]
pub extern "C" fn tts_get_engine_count() -> i32 {
    factory::engine_count() as i32
}

/// Write engine descriptors into a caller-allocated array.
///
/// `out_engines` must point to at least [`tts_get_engine_count`] entries.
/// Caller must free each entry's strings and the array with [`tts_free_engine_info`].
///
/// # Safety
///
/// `out_engines` must be non-null and point to enough space.
#[no_mangle]
pub extern "C" fn tts_get_engines(out_engines: *mut types::tts_engine_info) {
    if out_engines.is_null() {
        return;
    }
    let engines = factory::engine_list();
    for (i, e) in engines.iter().enumerate() {
        unsafe {
            let entry = out_engines.add(i);
            std::ptr::write(
                entry,
                types::tts_engine_info {
                    id: CString::new(e.id.clone()).unwrap().into_raw(),
                    name: CString::new(e.name.clone()).unwrap().into_raw(),
                    needs_credentials: e.needs_credentials,
                    credential_keys_json: CString::new(e.credential_keys_json.clone())
                        .unwrap()
                        .into_raw(),
                },
            );
        }
    }
}

/// Free an engine info array previously returned by [`tts_get_engines`].
///
/// # Safety
///
/// `engines` must be a pointer from `tts_get_engines` with the matching `count`.
#[no_mangle]
pub extern "C" fn tts_free_engine_info(engines: *mut types::tts_engine_info, count: i32) {
    if engines.is_null() || count <= 0 {
        return;
    }
    for i in 0..count {
        unsafe {
            let e = engines.add(i as usize);
            if !(*e).id.is_null() {
                let _ = CString::from_raw((*e).id);
            }
            if !(*e).name.is_null() {
                let _ = CString::from_raw((*e).name);
            }
            if !(*e).credential_keys_json.is_null() {
                let _ = CString::from_raw((*e).credential_keys_json);
            }
        }
    }
    let layout = std::alloc::Layout::array::<types::tts_engine_info>(count as usize).unwrap();
    unsafe {
        std::alloc::dealloc(engines.cast::<u8>(), layout);
    }
}

/// Return the last error message as a C string, or null if none.
///
/// The returned pointer is valid until the next call to any TTS function.
#[no_mangle]
pub extern "C" fn tts_get_last_error() -> *const c_char {
    match LAST_ERROR.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(cs) => cs.as_ptr(),
            None => ptr::null(),
        },
        Err(_) => ptr::null(),
    }
}

/// Pause in-progress speech.
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_pause(ctx: *mut tts_ctx) {
    if ctx.is_null() {
        return;
    }
    let ctx_ref = unsafe { &*ctx };
    let engine = ctx_ref.engine.lock().unwrap();
    let _ = engine.pause();
}

/// Resume paused speech.
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_resume(ctx: *mut tts_ctx) {
    if ctx.is_null() {
        return;
    }
    let ctx_ref = unsafe { &*ctx };
    let engine = ctx_ref.engine.lock().unwrap();
    let _ = engine.resume();
}

/// Synthesize text to audio bytes without playback.
/// Writes a heap-allocated buffer to `*out_bytes` and its length to `*out_len`.
/// Caller must free with [`tts_free_bytes`].
/// Returns 0 on success, -1 on failure.
///
/// # Safety
/// `ctx` must be valid. `out_bytes` and `out_len` must be non-null.
#[no_mangle]
pub extern "C" fn tts_synth_to_bytes(
    ctx: *mut tts_ctx,
    text: *const c_char,
    out_bytes: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if ctx.is_null() || text.is_null() || out_bytes.is_null() || out_len.is_null() {
        return -1;
    }
    let ctx_ref = unsafe { &*ctx };
    let text_str = unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    let voice = ctx_ref.voice_id.lock().unwrap().clone();
    let rate = *ctx_ref.rate.lock().unwrap();
    let pitch = *ctx_ref.pitch.lock().unwrap();
    let volume = *ctx_ref.volume.lock().unwrap();

    let engine = ctx_ref.engine.lock().unwrap();
    match engine.synth_to_bytes(&text_str, voice.as_deref(), rate, pitch, volume) {
        Ok(data) => {
            if data.is_empty() {
                unsafe {
                    *out_bytes = ptr::null_mut();
                    *out_len = 0;
                }
                return 0;
            }
            let len = data.len();
            let layout = std::alloc::Layout::array::<u8>(len).unwrap();
            let ptr = unsafe { std::alloc::alloc(layout) };
            unsafe {
                ptr::copy_nonoverlapping(data.as_ptr(), ptr, len);
                *out_bytes = ptr;
                *out_len = len;
            }
            0
        }
        Err(e) => {
            *ctx_ref.last_error.lock().unwrap() = e.to_string();
            -1
        }
    }
}

/// Free a byte buffer returned by [`tts_synth_to_bytes`].
///
/// # Safety
/// `bytes` must be from `tts_synth_to_bytes` with the matching `len`.
#[no_mangle]
pub extern "C" fn tts_free_bytes(bytes: *mut u8, len: usize) {
    if bytes.is_null() || len == 0 {
        return;
    }
    let layout = std::alloc::Layout::array::<u8>(len).unwrap();
    unsafe {
        std::alloc::dealloc(bytes, layout);
    }
}
