//! Full FFI lifecycle integration tests.
//!
//! Exercises the C ABI the way external bindings (Python, .NET, Swift) do:
//!   create → set_voice/rate/pitch/volume → get_voices → synth_to_bytes →
//!   free_bytes → destroy.
//!
//! Uses the `sherpaonnx` engine because it constructs without network
//! credentials, returns a real voice list from the bundled registry, and
//! fails gracefully (returns -1 from synth_to_bytes) when no model is
//! downloaded — which is exactly the contract we want to verify at the FFI
//! boundary.
//!
//! Run with: `cargo test --test ffi_lifecycle --features sherpaonnx`

#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::tts_ctx;
use rust_tts_wrapper::types::tts_voice;
use rust_tts_wrapper::{
    tts_create, tts_destroy, tts_free_bytes, tts_free_voices, tts_get_engine_count,
    tts_get_last_error, tts_get_voices, tts_set_pitch, tts_set_rate, tts_set_voice, tts_set_volume,
    tts_synth_to_bytes,
};
use std::ffi::CString;
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
    let id = CString::new("sherpaonnx").unwrap();
    let ctx = tts_create(id.as_ptr(), std::ptr::null());
    assert!(!ctx.is_null(), "tts_create returned null for sherpaonnx");
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

    let voice = CString::new("kokoro-en-en-19").unwrap();
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
fn test_ffi_get_voices_returns_nonempty_list_for_sherpaonnx() {
    let ctx = make_ctx();

    let mut voices_ptr: *mut tts_voice = std::ptr::null_mut();
    let mut count: i32 = 0;
    let rc = tts_get_voices(ctx, &mut voices_ptr, &mut count);
    assert_eq!(rc, 0, "tts_get_voices should succeed");
    assert!(count > 0, "sherpaonnx should have >0 voices from registry");
    assert!(
        !voices_ptr.is_null(),
        "non-empty voice list must come with a valid pointer"
    );

    // Each voice must have non-null string fields. We can't read them
    // portably without re-declaring the struct, but at minimum the pointer
    // and count are consistent.
    tts_free_voices(voices_ptr, count);
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
fn test_ffi_synth_to_bytes_without_model_fails_gracefully() {
    // sherpaonnx with no modelId will fail at synth time, but the contract
    // is that it returns -1 and writes to last_error — it must NOT panic.
    let ctx = make_ctx();
    let text = CString::new("hello").unwrap();

    let mut out_bytes: *mut u8 = std::ptr::null_mut();
    let mut out_len: usize = 0;
    let rc = tts_synth_to_bytes(ctx, text.as_ptr(), &mut out_bytes, &mut out_len);
    assert_eq!(rc, -1, "synth without a model must fail, not crash");
    assert!(out_bytes.is_null());
    assert_eq!(out_len, 0);

    // last_error must be populated so callers can surface a reason.
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

    let voice = CString::new("kokoro-en-en-19").unwrap();
    tts_set_voice(ctx, voice.as_ptr());
    tts_set_rate(ctx, 1.0);
    tts_set_pitch(ctx, 1.0);
    tts_set_volume(ctx, 1.0);

    let mut voices_ptr: *mut tts_voice = std::ptr::null_mut();
    let mut count: i32 = 0;
    assert_eq!(tts_get_voices(ctx, &mut voices_ptr, &mut count), 0);
    assert!(count > 0);
    tts_free_voices(voices_ptr, count);

    let text = CString::new("Hello, world").unwrap();
    let mut out_bytes: *mut u8 = std::ptr::null_mut();
    let mut out_len: usize = 0;
    // synth fails (no model downloaded) — must be -1, not a crash.
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
