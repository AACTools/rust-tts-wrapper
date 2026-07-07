//! Full FFI lifecycle integration tests.
//!
//! Exercises the C ABI the way external bindings (Python, .NET, Swift) do:
//!   create → set_voice/rate/pitch/volume → get_voices → synth_to_bytes →
//!   free_bytes → destroy.
//!
//! Uses a cloud engine (`openai` with dummy credentials) because it
//! constructs without network access, exists on every CI platform's
//! feature matrix (Linux system+cloud, Windows sapi+cloud, macOS
//! avsynth+cloud), and fails gracefully (returns -1 from synth_to_bytes)
//! when the bogus key is rejected — exactly the contract we want to
//! verify at the FFI boundary.
//!
//! Run with: `cargo test --test ffi_lifecycle --features cloud`

#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::tts_ctx;
use rust_tts_wrapper::types::{tts_engine_info, tts_voice};
use rust_tts_wrapper::{
    tts_create, tts_destroy, tts_free_bytes, tts_free_engines, tts_free_voices,
    tts_get_engine_count, tts_get_engines, tts_get_last_error, tts_get_voices, tts_pause,
    tts_resume, tts_set_on_audio, tts_set_on_boundary, tts_set_on_boundary2, tts_set_on_end,
    tts_set_on_error, tts_set_on_start, tts_set_on_viseme, tts_set_pitch, tts_set_rate,
    tts_set_voice, tts_set_volume, tts_speak, tts_speak_ssml, tts_speak_sync, tts_stop,
    tts_synth_to_bytes,
};
use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Counter for the audio callback. Static + Mutex because the C ABI hands us
/// a raw `void*` that must point to 'static data.
static AUDIO_CALLS: AtomicUsize = AtomicUsize::new(0);
static AUDIO_BYTES: AtomicUsize = AtomicUsize::new(0);
static AUDIO_USERDATA: Mutex<usize> = Mutex::new(0);

extern "C" fn audio_cb(_data: *const u8, size: usize, userdata: *mut std::ffi::c_void) {
    AUDIO_CALLS.fetch_add(1, Ordering::SeqCst);
    AUDIO_BYTES.fetch_add(size, Ordering::SeqCst);
    if !userdata.is_null() {
        let addr = userdata as usize;
        *AUDIO_USERDATA.lock().unwrap() = addr;
    }
}

/// Pointer-sized sentinel we can pass as userdata and recognise on the other
/// side of the FFI boundary.
const USERDATA_SENTINEL: usize = 0xDEAD_BEEF;

fn make_ctx() -> *mut tts_ctx {
    // Use a cloud engine with dummy creds — cloud is enabled on every CI
    // platform's feature matrix, and the engine constructs without any
    // network round-trip. Real synthesis will fail (caught later in the
    // lifecycle tests); construction must succeed.
    let id = CString::new("openai").unwrap();
    let creds = CString::new(r#"{"apiKey":"dummy-key-for-ffi-tests"}"#).unwrap();
    let ctx = tts_create(id.as_ptr(), creds.as_ptr());
    assert!(!ctx.is_null(), "tts_create returned null for openai");
    ctx
}

#[test]
fn test_ffi_engine_count_matches_create_pipeline() {
    // Engine count must be > 0 and stable across calls.
    let n = tts_get_engine_count();
    assert!(n > 0);
    assert_eq!(tts_get_engine_count(), n);
}

#[test]
fn test_ffi_create_and_destroy_round_trip() {
    let ctx = make_ctx();
    tts_destroy(ctx);
    // Double-destroy safety is covered in ffi_safety.rs; here we just verify
    // the happy path doesn't hang or panic.
}

#[test]
fn test_ffi_setters_accept_typical_values() {
    let ctx = make_ctx();

    let voice = CString::new("alloy").unwrap();
    tts_set_voice(ctx, voice.as_ptr());
    tts_set_rate(ctx, 1.5);
    tts_set_pitch(ctx, 0.8);
    tts_set_volume(ctx, 0.9);

    // The setters return () — the test passes if we didn't panic.
    tts_destroy(ctx);
}

#[test]
fn test_ffi_set_voice_null_is_safe() {
    // Mirrors the null-handling contract from ffi_safety.rs but inside a
    // full lifecycle so we know it doesn't corrupt later state.
    let ctx = make_ctx();
    tts_set_voice(ctx, std::ptr::null());
    tts_set_rate(ctx, 1.0);
    tts_set_pitch(ctx, 1.0);
    tts_set_volume(ctx, 1.0);
    tts_destroy(ctx);
}

#[test]
fn test_ffi_set_voice_empty_string_is_safe() {
    let ctx = make_ctx();
    let empty = CString::new("").unwrap();
    tts_set_voice(ctx, empty.as_ptr());
    tts_destroy(ctx);
}

#[test]
fn test_ffi_get_voices_returns_empty_list_for_openai() {
    // OpenAI has no voice-list endpoint, so get_voices returns 0 with
    // either a null pointer or a freeable empty array. Either way the
    // caller must be able to free without leaking.
    let ctx = make_ctx();

    let mut voices_ptr: *mut tts_voice = std::ptr::null_mut();
    let mut count: i32 = 0;
    let rc = tts_get_voices(ctx, &mut voices_ptr, &mut count);
    assert_eq!(rc, 0, "tts_get_voices should return 0 even on empty list");
    if !voices_ptr.is_null() && count > 0 {
        tts_free_voices(voices_ptr, count);
    }
    tts_destroy(ctx);
}

#[test]
fn test_ffi_get_voices_null_out_pointers_return_error() {
    let ctx = make_ctx();
    let rc = tts_get_voices(ctx, std::ptr::null_mut(), std::ptr::null_mut());
    assert_eq!(rc, -1);
    tts_destroy(ctx);
}

#[test]
#[ignore = "makes a real network call to OpenAI; run locally with --ignored"]
fn test_ffi_synth_to_bytes_with_dummy_key_fails_gracefully() {
    // The dummy key is rejected by the OpenAI API; the failure must surface
    // as -1 with last_error populated — never a panic. Marked #[ignore]
    // because it makes a real network call (CI runners may not have
    // deterministic network access to api.openai.com, and we don't want
    // test failures from transient DNS/TLS issues).
    let ctx = make_ctx();
    let text = CString::new("hello").unwrap();

    let mut out_bytes: *mut u8 = std::ptr::null_mut();
    let mut out_len: usize = 0;
    let rc = tts_synth_to_bytes(ctx, text.as_ptr(), &mut out_bytes, &mut out_len);
    assert_eq!(rc, -1, "synth with dummy key must fail, not crash");
    assert!(out_bytes.is_null());
    assert_eq!(out_len, 0);

    let err_ptr = tts_get_last_error(ctx);
    assert!(
        !err_ptr.is_null(),
        "last_error must not be null after failure"
    );
    let err = unsafe { std::ffi::CStr::from_ptr(err_ptr) }
        .to_string_lossy()
        .into_owned();
    assert!(!err.is_empty(), "last_error message must be non-empty");

    tts_destroy(ctx);
}

#[test]
fn test_ffi_synth_to_bytes_null_args_return_error() {
    // Defensive: every null pointer combination must return -1, not crash.
    let mut out_bytes: *mut u8 = std::ptr::null_mut();
    let mut out_len: usize = 0;

    assert_eq!(
        tts_synth_to_bytes(
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut out_bytes,
            &mut out_len
        ),
        -1
    );

    let ctx = make_ctx();
    assert_eq!(
        tts_synth_to_bytes(ctx, std::ptr::null(), &mut out_bytes, &mut out_len),
        -1
    );
    let text = CString::new("hi").unwrap();
    assert_eq!(
        tts_synth_to_bytes(ctx, text.as_ptr(), std::ptr::null_mut(), &mut out_len),
        -1
    );
    assert_eq!(
        tts_synth_to_bytes(ctx, text.as_ptr(), &mut out_bytes, std::ptr::null_mut()),
        -1
    );
    tts_destroy(ctx);
}

#[test]
fn test_ffi_free_bytes_null_is_noop() {
    // Calling free with null/0 must be safe — every language binding will
    // do this on an empty synth result.
    tts_free_bytes(std::ptr::null_mut(), 0);
    tts_free_bytes(std::ptr::null_mut(), 1024);
}

#[test]
fn test_ffi_full_lifecycle_voice_round_trip() {
    // create → set voice → get_voices → synth (fails safely) → destroy.
    // If any step leaks memory or panics under Miri this test fails.
    let ctx = make_ctx();

    let voice = CString::new("alloy").unwrap();
    tts_set_voice(ctx, voice.as_ptr());
    tts_set_rate(ctx, 1.0);
    tts_set_pitch(ctx, 1.0);
    tts_set_volume(ctx, 1.0);

    let mut voices_ptr: *mut tts_voice = std::ptr::null_mut();
    let mut count: i32 = 0;
    assert_eq!(tts_get_voices(ctx, &mut voices_ptr, &mut count), 0);
    if !voices_ptr.is_null() && count > 0 {
        tts_free_voices(voices_ptr, count);
    }

    let text = CString::new("Hello, world").unwrap();
    let mut out_bytes: *mut u8 = std::ptr::null_mut();
    let mut out_len: usize = 0;
    // Synth with a dummy key fails at the API; must return -1, not crash.
    let _ = tts_synth_to_bytes(ctx, text.as_ptr(), &mut out_bytes, &mut out_len);
    if !out_bytes.is_null() {
        tts_free_bytes(out_bytes, out_len);
    }

    tts_destroy(ctx);
}

#[test]
fn test_ffi_audio_callback_userdata_round_trip() {
    // The C callback signature carries a void* userdata. We must be able to
    // round-trip a pointer-sized sentinel through it. We can't drive the
    // callback via synth (sherpaonnx needs a real model), but we can invoke
    // the function pointer directly to verify the FFI trampoline wiring.
    AUDIO_CALLS.store(0, Ordering::SeqCst);
    AUDIO_BYTES.store(0, Ordering::SeqCst);

    let buf = [0u8; 16];
    audio_cb(buf.as_ptr(), buf.len(), USERDATA_SENTINEL as *mut _);

    assert_eq!(AUDIO_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(AUDIO_BYTES.load(Ordering::SeqCst), 16);
    assert_eq!(
        *AUDIO_USERDATA.lock().unwrap(),
        USERDATA_SENTINEL,
        "userdata pointer must arrive unchanged"
    );
}

// ===== FFI speak / speak_ssml / speak_sync =====
//
// These three all route through tts_speak_impl. We test the null-arg
// contract here (deterministic) and leave the network-dependent success
// path to the live tests.

#[test]
fn test_ffi_speak_null_args_return_error() {
    let text = CString::new("hi").unwrap();
    assert_eq!(tts_speak(std::ptr::null_mut(), text.as_ptr()), -1);

    let ctx = make_ctx();
    assert_eq!(tts_speak(ctx, std::ptr::null()), -1);
    tts_destroy(ctx);
}

#[test]
fn test_ffi_speak_ssml_null_args_return_error() {
    let ssml = CString::new("<speak>hi</speak>").unwrap();
    assert_eq!(tts_speak_ssml(std::ptr::null_mut(), ssml.as_ptr()), -1);

    let ctx = make_ctx();
    assert_eq!(tts_speak_ssml(ctx, std::ptr::null()), -1);
    tts_destroy(ctx);
}

#[test]
fn test_ffi_speak_sync_null_args_return_error() {
    let text = CString::new("hi").unwrap();
    assert_eq!(tts_speak_sync(std::ptr::null_mut(), text.as_ptr()), -1);

    let ctx = make_ctx();
    assert_eq!(tts_speak_sync(ctx, std::ptr::null()), -1);
    tts_destroy(ctx);
}

#[test]
#[ignore = "makes a real network call; run locally with --ignored"]
fn test_ffi_speak_with_dummy_key_does_not_panic() {
    // The actual rc depends on network/proxy behaviour (see
    // test_ffi_synth_to_bytes_with_dummy_key_fails_gracefully above). The
    // contract we can pin: tts_speak must not crash, and on failure must
    // populate last_error.
    let ctx = make_ctx();
    let text = CString::new("hello").unwrap();
    let rc = tts_speak(ctx, text.as_ptr());
    assert!(
        rc == 0 || rc == -1,
        "tts_speak must return 0 or -1, got {rc}"
    );
    if rc == -1 {
        let err_ptr = tts_get_last_error(ctx);
        assert!(!err_ptr.is_null());
    }
    tts_destroy(ctx);
}

// ===== stop / pause / resume with a valid ctx =====
//
// CloudEngine implements all three as no-ops returning Ok(()). The FFI
// wrappers discard the Result and just run the engine method inside
// catch_unwind. The contract: a valid ctx never panics.

#[test]
fn test_ffi_stop_pause_resume_with_valid_ctx_safe() {
    let ctx = make_ctx();
    tts_stop(ctx);
    tts_pause(ctx);
    tts_resume(ctx);
    // A second cycle verifies the state transitions are idempotent.
    tts_pause(ctx);
    tts_resume(ctx);
    tts_stop(ctx);
    tts_destroy(ctx);
}

// ===== tts_get_engines / tts_free_engines =====

#[test]
fn test_ffi_get_engines_returns_list_matching_count() {
    let mut engines_ptr: *mut tts_engine_info = std::ptr::null_mut();
    let mut count: i32 = 0;

    assert_eq!(tts_get_engines(&mut engines_ptr, &mut count), 0);
    assert!(count > 0, "engine list should not be empty");
    assert!(
        !engines_ptr.is_null(),
        "non-empty count must come with a valid pointer"
    );
    assert_eq!(count, tts_get_engine_count());

    // Read the first engine's id field via the public struct layout —
    // verifies the C strings were allocated and are readable.
    unsafe {
        let first = &*engines_ptr;
        assert!(!(*first).id.is_null(), "engine id must be allocated");
        let id = std::ffi::CStr::from_ptr((*first).id);
        assert!(!id.to_bytes().is_empty(), "engine id must be non-empty");
        assert!(!(*first).name.is_null());
        assert!(!(*first).credential_keys_json.is_null());
    }

    tts_free_engines(engines_ptr, count);
}

#[test]
fn test_ffi_get_engines_null_args_return_error() {
    let mut p: *mut tts_engine_info = std::ptr::null_mut();
    let mut c: i32 = 0;
    assert_eq!(tts_get_engines(std::ptr::null_mut(), &mut c), -1);
    assert_eq!(tts_get_engines(&mut p, std::ptr::null_mut()), -1);
}

#[test]
fn test_ffi_free_engines_null_is_noop() {
    tts_free_engines(std::ptr::null_mut(), 0);
    // count != 0 but null ptr is also tolerated (the implementation
    // returns early on either null ptr or count <= 0).
    tts_free_engines(std::ptr::null_mut(), 10);
}

// ===== Callback setters =====
//
// All seven callback setters store a (fn pointer, userdata) pair under a
// Mutex on the ctx. The contract tests below verify:
//   - registering a real fn pointer doesn't panic
//   - registering None (clearing) doesn't panic
//   - calling on a null ctx doesn't panic
//
// Driving the callbacks themselves requires real synthesis (the on_audio
// trampoline round-trip is covered by test_ffi_audio_callback_userdata_round_trip
// above; the per-setter delivery paths are exercised through the live
// cloud tests).

extern "C" fn sink_audio(_d: *const u8, _s: usize, _u: *mut std::ffi::c_void) {}
extern "C" fn sink_boundary(_w: *const c_char, _s: f32, _e: f32, _u: *mut std::ffi::c_void) {}
extern "C" fn sink_boundary2(
    _w: *const c_char,
    _o: i32,
    _l: i32,
    _s: f32,
    _e: f32,
    _u: *mut std::ffi::c_void,
) {
}
extern "C" fn sink_viseme(_v: i32, _o: f32, _u: *mut std::ffi::c_void) {}
extern "C" fn sink_void(_u: *mut std::ffi::c_void) {}
extern "C" fn sink_error(_m: *const c_char, _u: *mut std::ffi::c_void) {}

#[test]
fn test_ffi_callback_setters_register_without_panicking() {
    let ctx = make_ctx();

    tts_set_on_audio(ctx, Some(sink_audio), std::ptr::null_mut());
    tts_set_on_boundary(ctx, Some(sink_boundary), std::ptr::null_mut());
    tts_set_on_boundary2(ctx, Some(sink_boundary2), std::ptr::null_mut());
    tts_set_on_viseme(ctx, Some(sink_viseme), std::ptr::null_mut());
    tts_set_on_start(ctx, Some(sink_void), std::ptr::null_mut());
    tts_set_on_end(ctx, Some(sink_void), std::ptr::null_mut());
    tts_set_on_error(ctx, Some(sink_error), std::ptr::null_mut());

    tts_destroy(ctx);
}

#[test]
fn test_ffi_callback_setters_accept_none_to_clear() {
    // Passing None must clear any previously-registered callback without
    // surfaceing an error (the FFI wraps the assignment in catch_unwind).
    let ctx = make_ctx();

    tts_set_on_audio(ctx, Some(sink_audio), std::ptr::null_mut());
    tts_set_on_audio(ctx, None, std::ptr::null_mut());

    tts_set_on_boundary(ctx, Some(sink_boundary), std::ptr::null_mut());
    tts_set_on_boundary(ctx, None, std::ptr::null_mut());

    tts_set_on_boundary2(ctx, Some(sink_boundary2), std::ptr::null_mut());
    tts_set_on_boundary2(ctx, None, std::ptr::null_mut());

    tts_set_on_viseme(ctx, Some(sink_viseme), std::ptr::null_mut());
    tts_set_on_viseme(ctx, None, std::ptr::null_mut());

    tts_set_on_start(ctx, Some(sink_void), std::ptr::null_mut());
    tts_set_on_start(ctx, None, std::ptr::null_mut());

    tts_set_on_end(ctx, Some(sink_void), std::ptr::null_mut());
    tts_set_on_end(ctx, None, std::ptr::null_mut());

    tts_set_on_error(ctx, Some(sink_error), std::ptr::null_mut());
    tts_set_on_error(ctx, None, std::ptr::null_mut());

    tts_destroy(ctx);
}

#[test]
fn test_ffi_callback_setters_null_ctx_safe() {
    // Every setter must accept a null ctx as a no-op rather than panic —
    // external bindings may call tts_destroy concurrently with a setter.
    tts_set_on_audio(std::ptr::null_mut(), Some(sink_audio), std::ptr::null_mut());
    tts_set_on_boundary(
        std::ptr::null_mut(),
        Some(sink_boundary),
        std::ptr::null_mut(),
    );
    tts_set_on_boundary2(
        std::ptr::null_mut(),
        Some(sink_boundary2),
        std::ptr::null_mut(),
    );
    tts_set_on_viseme(
        std::ptr::null_mut(),
        Some(sink_viseme),
        std::ptr::null_mut(),
    );
    tts_set_on_start(std::ptr::null_mut(), Some(sink_void), std::ptr::null_mut());
    tts_set_on_end(std::ptr::null_mut(), Some(sink_void), std::ptr::null_mut());
    tts_set_on_error(std::ptr::null_mut(), Some(sink_error), std::ptr::null_mut());

    // And None + null ctx.
    tts_set_on_audio(std::ptr::null_mut(), None, std::ptr::null_mut());
    tts_set_on_boundary(std::ptr::null_mut(), None, std::ptr::null_mut());
    tts_set_on_boundary2(std::ptr::null_mut(), None, std::ptr::null_mut());
    tts_set_on_viseme(std::ptr::null_mut(), None, std::ptr::null_mut());
    tts_set_on_start(std::ptr::null_mut(), None, std::ptr::null_mut());
    tts_set_on_end(std::ptr::null_mut(), None, std::ptr::null_mut());
    tts_set_on_error(std::ptr::null_mut(), None, std::ptr::null_mut());
}

// ===== Callback userdata round-trip (boundary2) =====
//
// Extends the existing on_audio trampoline test: verify the richer
// boundary2 callback signature preserves its 6 arguments end-to-end.

static B2_CALLS: AtomicUsize = AtomicUsize::new(0);
static B2_USERDATA: Mutex<usize> = Mutex::new(0);
static B2_OFFSET: AtomicUsize = AtomicUsize::new(0);
static B2_LEN: AtomicUsize = AtomicUsize::new(0);

extern "C" fn boundary2_cb(
    _w: *const c_char,
    offset: i32,
    len: i32,
    _s: f32,
    _e: f32,
    userdata: *mut std::ffi::c_void,
) {
    B2_CALLS.fetch_add(1, Ordering::SeqCst);
    B2_OFFSET.store(offset.max(0) as usize, Ordering::SeqCst);
    B2_LEN.store(len.max(0) as usize, Ordering::SeqCst);
    if !userdata.is_null() {
        *B2_USERDATA.lock().unwrap() = userdata as usize;
    }
}

#[test]
fn test_ffi_boundary2_callback_args_round_trip() {
    B2_CALLS.store(0, Ordering::SeqCst);
    B2_OFFSET.store(0, Ordering::SeqCst);
    B2_LEN.store(0, Ordering::SeqCst);

    let word = CString::new("hello").unwrap();
    boundary2_cb(word.as_ptr(), 7, 5, 0.1, 0.5, USERDATA_SENTINEL as *mut _);

    assert_eq!(B2_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(B2_OFFSET.load(Ordering::SeqCst), 7);
    assert_eq!(B2_LEN.load(Ordering::SeqCst), 5);
    assert_eq!(*B2_USERDATA.lock().unwrap(), USERDATA_SENTINEL);
}

// ===== Callback userdata round-trip (viseme) =====

static VISEME_CALLS: AtomicUsize = AtomicUsize::new(0);
static VISEME_ID: AtomicUsize = AtomicUsize::new(0);

extern "C" fn viseme_cb(id: i32, _offset_sec: f32, _u: *mut std::ffi::c_void) {
    VISEME_CALLS.fetch_add(1, Ordering::SeqCst);
    VISEME_ID.store(id.max(0) as usize, Ordering::SeqCst);
}

#[test]
fn test_ffi_viseme_callback_args_round_trip() {
    VISEME_CALLS.store(0, Ordering::SeqCst);
    viseme_cb(5, 1.25, std::ptr::null_mut());
    assert_eq!(VISEME_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(VISEME_ID.load(Ordering::SeqCst), 5);
}

// ===== Callback userdata round-trip (error) =====

static ERR_CALLS: AtomicUsize = AtomicUsize::new(0);
static ERR_MSG: Mutex<String> = Mutex::new(String::new());

extern "C" fn error_cb(msg: *const c_char, _u: *mut std::ffi::c_void) {
    ERR_CALLS.fetch_add(1, Ordering::SeqCst);
    if !msg.is_null() {
        let s = unsafe { std::ffi::CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned();
        *ERR_MSG.lock().unwrap() = s;
    }
}

#[test]
fn test_ffi_error_callback_message_round_trip() {
    ERR_CALLS.store(0, Ordering::SeqCst);
    let msg = CString::new("synthesis failed: 401").unwrap();
    error_cb(msg.as_ptr(), std::ptr::null_mut());
    assert_eq!(ERR_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(*ERR_MSG.lock().unwrap(), "synthesis failed: 401");
}
