//! # rust-tts-wrapper
//!
//! Cross-platform TTS (Text-to-Speech) wrapper with a C ABI.
//! Mirrors [`js-tts-wrapper`] and `SwiftTTSWrapper`, supporting 21+ engines:
//! system (speech-dispatcher), Sherpa-ONNX (1300+ local models), and 19 cloud
//! providers. macOS adds AvSynth; Windows adds SAPI.
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

#[cfg(all(feature = "avsynth", target_os = "macos"))]
mod avsynth_engine;
#[cfg(feature = "cloud")]
mod cloud_engine;
pub mod engine;
pub mod factory;
#[cfg(all(feature = "sapi", target_os = "windows"))]
mod sapi_engine;
#[cfg(feature = "sherpaonnx")]
mod sherpaonnx_engine;
#[cfg(all(feature = "system", target_os = "linux"))]
mod system_engine;
pub mod types;

// Re-exports for user-friendly API
pub use engine::TtsEngine;
pub use factory::create_engine;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::sync::{Arc, Mutex};

/// Shared engine handle. Using `Arc<dyn TtsEngine>` instead of
/// `Mutex<Box<dyn TtsEngine>>` means synthesis no longer blocks
/// `tts_stop` / `tts_set_*` / `tts_destroy`. It also keeps the
/// engine alive if `tts_destroy` is called while synthesis is still
/// running — the Arc is cloned before speak() and dropped afterwards.
type BoxedEngine = Arc<dyn TtsEngine>;

/// Opaque context holding an engine instance and its per-instance settings.
pub type CAudioCb = Option<extern "C" fn(*const u8, usize, *mut std::ffi::c_void)>;
pub type CBoundaryCb = Option<extern "C" fn(*const c_char, f32, f32, *mut std::ffi::c_void)>;
pub type CBoundaryCb2 =
    Option<extern "C" fn(*const c_char, i32, i32, f32, f32, *mut std::ffi::c_void)>;
pub type CVisemeCb = Option<extern "C" fn(i32, f32, *mut std::ffi::c_void)>;
pub type CVoidCb = Option<extern "C" fn(*mut std::ffi::c_void)>;
pub type CErrorCb = Option<extern "C" fn(*const c_char, *mut std::ffi::c_void)>;
type BoxedAudioCb = Box<dyn FnMut(&[u8])>;
type BoxedBoundaryCb = Box<dyn FnMut(&str, f32, f32, i32, i32)>;

pub struct tts_ctx {
    // The TtsEngine trait already requires Send + Sync, and every engine
    // impl uses internal Mutexes (or atomic state) for thread safety. The
    // outer Mutex that used to wrap the engine was held for the entire
    // synthesis call, blocking concurrent tts_stop / tts_set_*. Removing
    // it lets those calls proceed while synthesis runs.
    engine: BoxedEngine,
    voice_id: Mutex<Option<String>>,
    rate: Mutex<f32>,
    pitch: Mutex<f32>,
    volume: Mutex<f32>,
    // Cached CString of the last error message. We must return a *const c_char
    // from tts_get_last_error whose backing allocation outlives the function
    // call; the previous version constructed a fresh CString inside the
    // getter and returned its as_ptr() while the CString was dropped at end
    // of scope — use-after-free, surfaced as empty reads on Windows.
    // Storing the CString here keeps the allocation alive until the next
    // error replaces it.
    last_error: Mutex<CString>,
    // Callback + userdata are bundled into a single Mutex each so a reader
    // can never observe a new callback paired with stale userdata (or vice
    // versa). The `Send` wrapper below lets us ship the raw pointer across
    // threads safely because access is mediated by the Mutex.
    on_audio: Mutex<AudioCallback>,
    on_boundary: Mutex<BoundaryCallback>,
    on_start: Mutex<VoidCallback>,
    on_end: Mutex<VoidCallback>,
    on_error: Mutex<ErrorCallback>,
    on_boundary2: Mutex<BoundaryCallback2>,
    on_viseme: Mutex<VisemeCallback>,
}

/// Bundled audio callback + userdata so updates are atomic.
#[derive(Clone, Copy)]
struct AudioCallback {
    cb: CAudioCb,
    userdata: *mut std::ffi::c_void,
}

/// Bundled boundary callback + userdata so updates are atomic.
#[derive(Clone, Copy)]
struct BoundaryCallback {
    cb: CBoundaryCb,
    userdata: *mut std::ffi::c_void,
}

/// Bundled start/end callback (no payload, just a lifecycle signal).
#[derive(Clone, Copy)]
struct VoidCallback {
    cb: CVoidCb,
    userdata: *mut std::ffi::c_void,
}

/// Bundled error callback (error message + userdata).
#[derive(Clone, Copy)]
struct ErrorCallback {
    cb: CErrorCb,
    userdata: *mut std::ffi::c_void,
}

/// Extended boundary callback with source-text offsets.
#[derive(Clone, Copy)]
struct BoundaryCallback2 {
    cb: CBoundaryCb2,
    userdata: *mut std::ffi::c_void,
}

/// Bundled viseme callback.
#[derive(Clone, Copy)]
struct VisemeCallback {
    cb: CVisemeCb,
    userdata: *mut std::ffi::c_void,
}

// SAFETY: the raw userdata pointers are only dereferenced via the C callback
// signatures, and access is serialised by the surrounding Mutex on each
// `tts_ctx` field. The host is responsible for the userdata lifetime, which
// is the standard FFI callback contract.
unsafe impl Send for AudioCallback {}
unsafe impl Sync for AudioCallback {}
unsafe impl Send for BoundaryCallback {}
unsafe impl Sync for BoundaryCallback {}
unsafe impl Send for VoidCallback {}
unsafe impl Sync for VoidCallback {}
unsafe impl Send for ErrorCallback {}
unsafe impl Sync for ErrorCallback {}
unsafe impl Send for BoundaryCallback2 {}
unsafe impl Sync for BoundaryCallback2 {}
unsafe impl Send for VisemeCallback {}
unsafe impl Sync for VisemeCallback {}

static LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);

fn set_error(msg: &str) {
    if let Ok(mut guard) = LAST_ERROR.lock() {
        *guard = Some(safe_cstring(msg));
    }
}

/// Helper macro to wrap FFI functions with panic catching
macro_rules! ffi_catch {
    ($expr:expr) => {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $expr)).unwrap_or(-1)
    };
}

/// Create a CString from any string, replacing interior null bytes with a
/// replacement character so it never panics. Used for all FFI string
/// conversions where the source data comes from engine APIs (voice names,
/// error messages, etc.) and should never contain nulls, but we must not
/// panic across the FFI boundary if it does.
fn safe_cstring(s: impl AsRef<str>) -> CString {
    let s = s.as_ref();
    if let Ok(cs) = CString::new(s) {
        return cs;
    }
    // Replace interior nulls and retry.
    let cleaned: String = s.chars().filter(|&c| c != '\0').collect();
    CString::new(cleaned).unwrap_or_default()
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
    if let Ok(ptr) = result {
        ptr
    } else {
        set_error("engine creation panicked");
        ptr::null_mut()
    }
}

fn tts_create_inner(engine_id: *const c_char, credentials_json: *const c_char) -> *mut tts_ctx {
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
            engine,
            voice_id: Mutex::new(None),
            rate: Mutex::new(1.0),
            pitch: Mutex::new(1.0),
            volume: Mutex::new(1.0),
            last_error: Mutex::new(CString::new("").unwrap()),
            on_audio: Mutex::new(AudioCallback {
                cb: None,
                userdata: ptr::null_mut(),
            }),
            on_boundary: Mutex::new(BoundaryCallback {
                cb: None,
                userdata: ptr::null_mut(),
            }),
            on_start: Mutex::new(VoidCallback {
                cb: None,
                userdata: ptr::null_mut(),
            }),
            on_end: Mutex::new(VoidCallback {
                cb: None,
                userdata: ptr::null_mut(),
            }),
            on_error: Mutex::new(ErrorCallback {
                cb: None,
                userdata: ptr::null_mut(),
            }),
            on_boundary2: Mutex::new(BoundaryCallback2 {
                cb: None,
                userdata: ptr::null_mut(),
            }),
            on_viseme: Mutex::new(VisemeCallback {
                cb: None,
                userdata: ptr::null_mut(),
            }),
        });
        Box::into_raw(ctx)
    } else {
        set_error(&format!("Unknown engine: {engine_id_str}"));
        ptr::null_mut()
    }
}

/// Destroy a TTS context and free all associated resources.
///
/// Attempts to stop any in-progress speech before dropping the engine so the
/// underlying resources (speech-dispatcher connection, COM objects, etc.) get
/// a chance to clean up
///
/// # Safety
///
/// `ctx` must be a pointer previously returned by [`tts_create`],
/// or null (no-op).
#[no_mangle]
pub extern "C" fn tts_destroy(ctx: *mut tts_ctx) {
    if ctx.is_null() {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Safety: ctx is non-null and was produced by Box::into_raw above.
        let boxed = unsafe { Box::from_raw(ctx) };
        // Best-effort stop before drop. The engine's Drop impl is responsible
        // for any additional cleanup (closing sockets, releasing COM refs).
        let _ = boxed.engine.stop();
        drop(boxed);
    }));
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
    tts_speak_impl(ctx, text, false)
}

/// Speak pre-built SSML using the engine in `ctx`.
///
/// The SSML is passed directly to the engine without SpeechMarkdown
/// conversion or rate/pitch/volume wrapping. Callers are responsible
/// for embedding all prosody in the SSML.
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `ctx` must be a valid pointer from [`tts_create`].
/// `ssml` must be a valid null-terminated C string.
#[no_mangle]
pub extern "C" fn tts_speak_ssml(ctx: *mut tts_ctx, ssml: *const c_char) -> i32 {
    tts_speak_impl(ctx, ssml, true)
}

fn tts_speak_impl(ctx: *mut tts_ctx, text: *const c_char, raw_ssml: bool) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tts_speak_impl_inner(ctx, text, raw_ssml)
    }))
    .unwrap_or(-1)
}

#[allow(clippy::too_many_lines)]
fn tts_speak_impl_inner(ctx: *mut tts_ctx, text: *const c_char, raw_ssml: bool) -> i32 {
    ffi_catch!({
        if ctx.is_null() || text.is_null() {
            return -1;
        }
        let ctx_ref = unsafe { &*ctx };
        let text_str = unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned();
        let voice = ctx_ref.voice_id.lock().unwrap().clone();
        let rate = if raw_ssml {
            1.0
        } else {
            *ctx_ref.rate.lock().unwrap()
        };
        let pitch = if raw_ssml {
            1.0
        } else {
            *ctx_ref.pitch.lock().unwrap()
        };
        let volume = if raw_ssml {
            1.0
        } else {
            *ctx_ref.volume.lock().unwrap()
        };

        let audio = { *ctx_ref.on_audio.lock().unwrap() };
        let boundary = { *ctx_ref.on_boundary.lock().unwrap() };
        let boundary2 = { *ctx_ref.on_boundary2.lock().unwrap() };

        let mut on_audio_closure: Option<BoxedAudioCb> = match audio.cb {
            Some(cb) => Some(Box::new(move |bytes: &[u8]| {
                cb(bytes.as_ptr(), bytes.len(), audio.userdata);
            })),
            None => None,
        };

        let mut on_boundary_closure: Option<BoxedBoundaryCb> = match (boundary.cb, boundary2.cb) {
            (None, None) => None,
            _ => Some(Box::new(
                move |word: &str, start: f32, end: f32, char_offset: i32, char_len: i32| {
                    if let Some(cb) = boundary.cb {
                        if let Ok(c_word) = CString::new(word) {
                            cb(c_word.as_ptr(), start, end, boundary.userdata);
                        }
                    }
                    if let Some(cb) = boundary2.cb {
                        if let Ok(c_word) = CString::new(word) {
                            cb(
                                c_word.as_ptr(),
                                char_offset,
                                char_len,
                                start,
                                end,
                                boundary2.userdata,
                            );
                        }
                    }
                },
            )),
        };

        let start_cb = { *ctx_ref.on_start.lock().unwrap() };
        let end_cb = { *ctx_ref.on_end.lock().unwrap() };
        let error_cb = { *ctx_ref.on_error.lock().unwrap() };

        if let Some(cb) = start_cb.cb {
            cb(start_cb.userdata);
        }

        #[cfg(feature = "cloud")]
        {
            let vis = { *ctx_ref.on_viseme.lock().unwrap() };
            let vbox: Option<Box<dyn FnMut(i32, f32)>> = vis.cb.map(|cb| {
                let ud = vis.userdata;
                Box::new(move |id: i32, off: f32| {
                    cb(id, off, ud);
                }) as Box<dyn FnMut(i32, f32)>
            });
            crate::cloud_engine::set_viseme_callback(vbox);
        }

        let engine = &ctx_ref.engine;
        let result = engine.speak(
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
                .map(|f| &mut **f as &mut dyn FnMut(&str, f32, f32, i32, i32)),
        );

        match result {
            Ok(()) => {
                if let Some(cb) = end_cb.cb {
                    cb(end_cb.userdata);
                }
                0
            }
            Err(e) => {
                let msg = e.to_string();
                *ctx_ref.last_error.lock().unwrap() =
                    CString::new(msg.clone()).unwrap_or_else(|_| CString::new("error").unwrap());
                if let Some(cb) = error_cb.cb {
                    if let Ok(c_msg) = CString::new(msg) {
                        cb(c_msg.as_ptr(), error_cb.userdata);
                    }
                }
                -1
            }
        }
    })
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
    ffi_catch!({
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

        let audio = { *ctx_ref.on_audio.lock().unwrap() };
        let boundary = { *ctx_ref.on_boundary.lock().unwrap() };
        let boundary2 = { *ctx_ref.on_boundary2.lock().unwrap() };

        let mut on_audio_closure: Option<BoxedAudioCb> = match audio.cb {
            Some(cb) => Some(Box::new(move |bytes: &[u8]| {
                cb(bytes.as_ptr(), bytes.len(), audio.userdata);
            })),
            None => None,
        };

        let mut on_boundary_closure: Option<BoxedBoundaryCb> = match (boundary.cb, boundary2.cb) {
            (None, None) => None,
            _ => Some(Box::new(
                move |word: &str, start: f32, end: f32, char_offset: i32, char_len: i32| {
                    if let Some(cb) = boundary.cb {
                        if let Ok(c_word) = CString::new(word) {
                            cb(c_word.as_ptr(), start, end, boundary.userdata);
                        }
                    }
                    if let Some(cb) = boundary2.cb {
                        if let Ok(c_word) = CString::new(word) {
                            cb(
                                c_word.as_ptr(),
                                char_offset,
                                char_len,
                                start,
                                end,
                                boundary2.userdata,
                            );
                        }
                    }
                },
            )),
        };

        // Snapshot lifecycle callbacks atomically.
        let start_cb = { *ctx_ref.on_start.lock().unwrap() };
        let end_cb = { *ctx_ref.on_end.lock().unwrap() };
        let error_cb = { *ctx_ref.on_error.lock().unwrap() };

        // Fire on_start before synthesis.
        if let Some(cb) = start_cb.cb {
            cb(start_cb.userdata);
        }

        #[cfg(feature = "cloud")]
        {
            let vis = { *ctx_ref.on_viseme.lock().unwrap() };
            let vbox: Option<Box<dyn FnMut(i32, f32)>> = vis.cb.map(|cb| {
                let ud = vis.userdata;
                Box::new(move |id: i32, off: f32| {
                    cb(id, off, ud);
                }) as Box<dyn FnMut(i32, f32)>
            });
            crate::cloud_engine::set_viseme_callback(vbox);
        }

        let engine = &ctx_ref.engine;
        let result = engine.speak_sync(
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
                .map(|f| &mut **f as &mut dyn FnMut(&str, f32, f32, i32, i32)),
        );

        match result {
            Ok(()) => {
                if let Some(cb) = end_cb.cb {
                    cb(end_cb.userdata);
                }
                0
            }
            Err(e) => {
                let msg = e.to_string();
                *ctx_ref.last_error.lock().unwrap() =
                    CString::new(msg.clone()).unwrap_or_else(|_| CString::new("error").unwrap());
                if let Some(cb) = error_cb.cb {
                    if let Ok(c_msg) = CString::new(msg) {
                        cb(c_msg.as_ptr(), error_cb.userdata);
                    }
                }
                -1
            }
        }
    })
}

/// Stop any in-progress speech.
///
/// # Safety
///
/// `ctx` must be a valid pointer from [`tts_create`].
#[no_mangle]
pub extern "C" fn tts_stop(ctx: *mut tts_ctx) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        let _ = ctx_ref.engine.stop();
    }));
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
    result.unwrap_or(-1)
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
    let engine = &ctx_ref.engine;
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
            let Ok(layout) = std::alloc::Layout::array::<types::tts_voice>(len) else {
                set_error(&format!("Voice array too large: {len} entries"));
                unsafe {
                    *out_voices = ptr::null_mut();
                    *out_count = 0;
                }
                return -1;
            };
            let arr_ptr = unsafe { std::alloc::alloc(layout).cast::<types::tts_voice>() };
            for (i, v) in voices.iter().enumerate() {
                unsafe {
                    let entry = arr_ptr.add(i);
                    std::ptr::write(
                        entry,
                        types::tts_voice {
                            id: safe_cstring(&v.id).into_raw(),
                            name: safe_cstring(&v.name).into_raw(),
                            language: safe_cstring({
                                let lc = v.language_codes.first();
                                match lc {
                                    Some(lc) if !lc.display.is_empty() => {
                                        format!("{} [{}]", lc.display, lc.bcp47)
                                    }
                                    _ => v.primary_language().to_string(),
                                }
                            })
                            .into_raw(),
                            gender: safe_cstring(v.gender.to_string()).into_raw(),
                            engine: safe_cstring(&v.provider).into_raw(),
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
            *ctx_ref.last_error.lock().unwrap() =
                CString::new(e.to_string()).unwrap_or_else(|_| CString::new("error").unwrap());
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
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        let Ok(layout) = std::alloc::Layout::array::<types::tts_voice>(count as usize) else {
            return;
        };
        unsafe {
            std::alloc::dealloc(voices.cast::<u8>(), layout);
        }
    }));
}

/// Set the voice for subsequent speak calls.
///
/// # Safety
///
/// `ctx` must be valid. `voice_id` must be a valid null-terminated C string.
#[no_mangle]
pub extern "C" fn tts_set_voice(ctx: *mut tts_ctx, voice_id: *const c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() || voice_id.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        let id = unsafe { CStr::from_ptr(voice_id) }
            .to_string_lossy()
            .into_owned();
        *ctx_ref.voice_id.lock().unwrap() = Some(id);
    }));
}

/// Set the speech rate (1.0 = normal).
///
/// # Safety
///
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_rate(ctx: *mut tts_ctx, rate: f32) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        *unsafe { &*ctx }.rate.lock().unwrap() = rate;
    }));
}

/// Set the speech pitch (1.0 = normal).
///
/// # Safety
///
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_pitch(ctx: *mut tts_ctx, pitch: f32) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        *unsafe { &*ctx }.pitch.lock().unwrap() = pitch;
    }));
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
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        // Single critical section: a reader can never observe a new cb
        // paired with stale userdata.
        *ctx_ref.on_audio.lock().unwrap() = AudioCallback { cb, userdata };
    }));
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
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        *ctx_ref.on_boundary.lock().unwrap() = BoundaryCallback { cb, userdata };
    }));
}

/// Extended boundary callback with source-text char offset and length.
/// cb(word, char_offset, char_len, start_s, end_s, userdata)
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_on_boundary2(
    ctx: *mut tts_ctx,
    cb: CBoundaryCb2,
    userdata: *mut std::ffi::c_void,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        *ctx_ref.on_boundary2.lock().unwrap() = BoundaryCallback2 { cb, userdata };
    }));
}

/// Viseme callback for lip-sync / facial animation.
/// cb(viseme_id, audio_offset_sec, userdata)
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_on_viseme(
    ctx: *mut tts_ctx,
    cb: CVisemeCb,
    userdata: *mut std::ffi::c_void,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        *ctx_ref.on_viseme.lock().unwrap() = VisemeCallback { cb, userdata };
    }));
}

/// Set the callback fired when speech starts.
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_on_start(
    ctx: *mut tts_ctx,
    cb: CVoidCb,
    userdata: *mut std::ffi::c_void,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        *ctx_ref.on_start.lock().unwrap() = VoidCallback { cb, userdata };
    }));
}

/// Set the callback fired when speech completes successfully.
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_on_end(ctx: *mut tts_ctx, cb: CVoidCb, userdata: *mut std::ffi::c_void) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        *ctx_ref.on_end.lock().unwrap() = VoidCallback { cb, userdata };
    }));
}

/// Set the callback fired when speech fails.
///
/// The error message is a null-terminated C string valid for the duration
/// of the callback only.
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_set_on_error(
    ctx: *mut tts_ctx,
    cb: CErrorCb,
    userdata: *mut std::ffi::c_void,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        *ctx_ref.on_error.lock().unwrap() = ErrorCallback { cb, userdata };
    }));
}

/// Return the number of registered engines.
#[no_mangle]
pub extern "C" fn tts_get_engine_count() -> i32 {
    factory::engine_count() as i32
}

/// Get the list of available engine descriptors.
///
/// On success, writes a heap-allocated array to `*out_engines` and its length
/// to `*out_count`. Caller must free with [`tts_free_engines`].
///
/// Returns 0 on success, -1 on failure.
///
/// # Safety
///
/// `out_engines` and `out_count` must be non-null.
#[no_mangle]
pub extern "C" fn tts_get_engines(
    out_engines: *mut *mut types::tts_engine_info,
    out_count: *mut i32,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if out_engines.is_null() || out_count.is_null() {
            return -1;
        }

        let engines = factory::engine_list();
        let count = engines.len();

        if count == 0 {
            unsafe {
                *out_engines = ptr::null_mut();
                *out_count = 0;
            }
            return 0;
        }

        let Ok(layout) = std::alloc::Layout::array::<types::tts_engine_info>(count) else {
            return -1;
        };
        let ptr = unsafe { std::alloc::alloc(layout) } as *mut types::tts_engine_info;

        if ptr.is_null() {
            return -1;
        }

        for (i, e) in engines.iter().enumerate() {
            unsafe {
                let entry = ptr.add(i);
                std::ptr::write(
                    entry,
                    types::tts_engine_info {
                        id: safe_cstring(&e.id).into_raw(),
                        name: safe_cstring(&e.name).into_raw(),
                        needs_credentials: e.needs_credentials,
                        credential_keys_json: safe_cstring(&e.credential_keys_json).into_raw(),
                    },
                );
            }
        }

        unsafe {
            *out_engines = ptr;
            *out_count = count as i32;
        }

        0
    }))
    .unwrap_or(-1)
}

/// Free an engine info array previously returned by [`tts_get_engines`].
///
/// # Safety
///
/// `engines` must be a pointer from `tts_get_engines` with the matching `count`.
#[no_mangle]
pub extern "C" fn tts_free_engines(engines: *mut types::tts_engine_info, count: i32) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        let Ok(layout) = std::alloc::Layout::array::<types::tts_engine_info>(count as usize) else {
            return;
        };
        unsafe {
            std::alloc::dealloc(engines.cast::<u8>(), layout);
        }
    }));
}

/// Return the last error message as a C string, or null if none.
///
/// If ctx is provided, returns the per-context error. If ctx is null,
/// returns the global error (for tts_create failures).
///
/// The returned pointer is valid until the next call to any TTS function.
///
/// # Safety
///
/// `ctx` may be null (returns global error), or a valid context pointer.
#[no_mangle]
pub extern "C" fn tts_get_last_error(ctx: *mut tts_ctx) -> *const c_char {
    // If context provided and valid, return per-context error.
    //
    // The CString is stored in the ctx (Mutex<CString>) so the returned
    // pointer's backing allocation lives until the next error replaces it.
    // The caller must copy if they need the string beyond the next
    // synth/speak call on this ctx.
    if !ctx.is_null() {
        let ctx_ref = unsafe { &*ctx };
        if let Ok(guard) = ctx_ref.last_error.lock() {
            if !guard.is_empty() {
                return guard.as_ptr();
            }
        }
    }

    // Fallback to global error (for tts_create failures or null context)
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
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        let _ = ctx_ref.engine.pause();
    }));
}

/// Resume paused speech.
///
/// # Safety
/// `ctx` must be valid.
#[no_mangle]
pub extern "C" fn tts_resume(ctx: *mut tts_ctx) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let ctx_ref = unsafe { &*ctx };
        let _ = ctx_ref.engine.resume();
    }));
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
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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

        let engine = &ctx_ref.engine;
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
                let Ok(layout) = std::alloc::Layout::array::<u8>(len) else {
                    set_error(&format!("Audio buffer too large: {len} bytes"));
                    return -1;
                };
                let ptr = unsafe { std::alloc::alloc(layout) };
                unsafe {
                    ptr::copy_nonoverlapping(data.as_ptr(), ptr, len);
                    *out_bytes = ptr;
                    *out_len = len;
                }
                0
            }
            Err(e) => {
                *ctx_ref.last_error.lock().unwrap() =
                    CString::new(e.to_string()).unwrap_or_else(|_| CString::new("error").unwrap());
                -1
            }
        }
    }))
    .unwrap_or(-1)
}

/// Free a byte buffer returned by [`tts_synth_to_bytes`].
///
/// # Safety
/// `bytes` must be from `tts_synth_to_bytes` with the matching `len`.
#[no_mangle]
pub extern "C" fn tts_free_bytes(bytes: *mut u8, len: usize) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if bytes.is_null() || len == 0 {
            return;
        }
        let Ok(layout) = std::alloc::Layout::array::<u8>(len) else {
            return;
        };
        unsafe {
            std::alloc::dealloc(bytes, layout);
        }
    }));
}
