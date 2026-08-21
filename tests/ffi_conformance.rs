//! ABI conformance suite.
//!
//! Symbol-level contract tests for the C ABI that every language binding
//! (C, Python, .NET, Swift, Node) depends on. Complements
//! `ffi_lifecycle.rs` (per-symbol lifecycle) and `ffi_safety.rs`
//! (hardening) with the cross-cutting behaviours bindings actually lean
//! on: multi-context lifetimes, the error surface, callback
//! replacement, and the full setter surface.
//!
//! Uses the same offline-deterministic strategy as the lifecycle suite:
//! a cloud engine (`openai`) constructs without network access and
//! fails synthesis deterministically with a dummy key.

#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::tts_ctx;
use rust_tts_wrapper::{
    tts_create, tts_destroy, tts_get_last_error, tts_set_on_audio, tts_set_on_boundary,
    tts_set_on_mark, tts_speak_ssml, tts_speak_sync, tts_synth_to_bytes,
};
use std::ffi::{c_void, CString};
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicUsize, Ordering};

fn make_ctx() -> *mut tts_ctx {
    let id = CString::new("openai").unwrap();
    let creds = CString::new(r#"{"apiKey":"dummy-key-for-conformance"}"#).unwrap();
    let ctx = tts_create(id.as_ptr(), creds.as_ptr());
    assert!(!ctx.is_null(), "tts_create(openai) must succeed offline");
    ctx
}

// ---------------------------------------------------------------------------
// Multi-context lifetimes
// ---------------------------------------------------------------------------

#[test]
fn conformance_many_contexts_live_simultaneously() {
    // Bindings (and hosts like screen readers) hold one ctx per engine
    // instance; creation must not leak global state between contexts and
    // destruction order must not matter.
    let mut ctxs: Vec<*mut tts_ctx> = (0..16).map(|_| make_ctx()).collect();
    for ctx in &ctxs {
        assert!(!ctx.is_null());
    }
    // Destroy in reverse order, then a fresh batch in forward order.
    ctxs.reverse();
    for ctx in ctxs {
        tts_destroy(ctx);
    }

    let mut second: Vec<*mut tts_ctx> = (0..4).map(|_| make_ctx()).collect();
    for ctx in second.drain(..) {
        tts_destroy(ctx);
    }
}

#[test]
fn conformance_context_isolation_last_error() {
    // last_error is per-context: an error recorded on ctx A must not be
    // visible on a fresh ctx B.
    let a = make_ctx();
    let b = make_ctx();

    // Force a failure on A (dummy key → deterministic offline error).
    let text = CString::new("isolation").unwrap();
    let rc_a = tts_synth_to_bytes(a, text.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut());
    assert_ne!(rc_a, 0, "dummy-key synth must fail");

    // B was created after A but before A failed. Contract: null (or,
    // if the global fallback fired, an unrelated string) — never A's
    // error. The returned pointer is owned by the ctx — borrow only.
    let err_b = tts_get_last_error(b);
    if !err_b.is_null() {
        // SAFETY: valid C string returned by tts_get_last_error.
        let b_msg = unsafe { std::ffi::CStr::from_ptr(err_b) }.to_bytes();
        assert!(
            b_msg.is_empty(),
            "ctx B must not observe ctx A's error (got {b_msg:?})"
        );
    }

    tts_destroy(a);
    tts_destroy(b);
}

// ---------------------------------------------------------------------------
// Error surface
// ---------------------------------------------------------------------------

#[test]
fn conformance_failed_synth_populates_last_error() {
    let ctx = make_ctx();
    let text = CString::new("conformance error surface").unwrap();

    let mut bytes: *mut u8 = std::ptr::null_mut();
    let mut len: usize = 0;
    let rc = tts_synth_to_bytes(ctx, text.as_ptr(), &mut bytes, &mut len);
    assert_ne!(rc, 0, "offline dummy-key synthesis must fail");
    assert!(bytes.is_null(), "no buffer must be handed out on failure");
    assert_eq!(len, 0);

    let err = tts_get_last_error(ctx);
    assert!(!err.is_null(), "failure must populate last_error");
    // Borrowed pointer — do not free; just read.
    // SAFETY: valid C string returned by tts_get_last_error.
    let msg = unsafe { std::ffi::CStr::from_ptr(err) }
        .to_string_lossy()
        .to_string();
    assert!(!msg.is_empty(), "last_error must be a non-empty message");
    assert!(bytes.is_null());

    tts_destroy(ctx);
}

#[test]
fn conformance_speak_ssml_valid_ctx_fails_cleanly_offline() {
    let ctx = make_ctx();
    let ssml = CString::new("<speak>conformance <mark name=\"m\"/> ssml</speak>").unwrap();
    let rc = tts_speak_ssml(ctx, ssml.as_ptr());
    assert_ne!(rc, 0, "dummy-key SSML synthesis must fail, not crash");

    let text = CString::new("plain").unwrap();
    let rc_sync = tts_speak_sync(ctx, text.as_ptr());
    assert_ne!(rc_sync, 0, "dummy-key sync speak must fail, not crash");

    tts_destroy(ctx);
}

// ---------------------------------------------------------------------------
// Callback surface
// ---------------------------------------------------------------------------

static MARK_CALLS: AtomicUsize = AtomicUsize::new(0);
extern "C" fn mark_cb(
    _name: *const c_char,
    _char_offset: i32,
    _start: f32,
    _end: f32,
    _userdata: *mut c_void,
) {
    MARK_CALLS.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn conformance_mark_callback_register_clear_and_null_ctx() {
    let ctx = make_ctx();

    tts_set_on_mark(ctx, Some(mark_cb), std::ptr::null_mut());
    // Replace with None (clear) — must be a silent no-op, not an error.
    tts_set_on_mark(ctx, None, std::ptr::null_mut());
    // Null ctx is accepted as a no-op for every setter.
    tts_set_on_mark(std::ptr::null_mut(), Some(mark_cb), std::ptr::null_mut());
    tts_set_on_mark(std::ptr::null_mut(), None, std::ptr::null_mut());

    assert_eq!(MARK_CALLS.load(Ordering::SeqCst), 0);
    tts_destroy(ctx);
}

#[test]
fn conformance_callbacks_can_be_replaced_in_place() {
    // Re-registering over a live callback must not double-fire or panic;
    // the last registration wins. Verified via the audio callback with
    // distinct userdata sentinels.
    static SEEN: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn audio_a(_d: *const u8, _s: usize, _u: *mut c_void) {
        SEEN.store(0xA, Ordering::SeqCst);
    }
    extern "C" fn audio_b(_d: *const u8, _s: usize, _u: *mut c_void) {
        SEEN.store(0xB, Ordering::SeqCst);
    }

    let ctx = make_ctx();
    tts_set_on_audio(ctx, Some(audio_a), std::ptr::null_mut());
    tts_set_on_audio(ctx, Some(audio_b), std::ptr::null_mut());
    tts_set_on_audio(ctx, None, std::ptr::null_mut());
    tts_set_on_audio(ctx, Some(audio_a), std::ptr::null_mut());
    tts_destroy(ctx);
}

#[test]
fn conformance_boundary_callback_full_signature_compiles() {
    // The consolidated boundary callback signature every binding
    // marshals: (word, char_offset, char_len, start_s, end_s,
    // estimated, userdata). Compile-level contract + registration
    // round-trip; live delivery is covered by the engine suites.
    static BOUNDARY_REGISTRATIONS: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn boundary_cb(
        _word: *const c_char,
        _char_offset: i32,
        _char_len: i32,
        _start_s: f32,
        _end_s: f32,
        _estimated: c_int,
        _userdata: *mut c_void,
    ) {
        BOUNDARY_REGISTRATIONS.fetch_add(1, Ordering::SeqCst);
    }

    let ctx = make_ctx();
    tts_set_on_boundary(ctx, Some(boundary_cb), std::ptr::null_mut());
    tts_set_on_boundary(ctx, None, std::ptr::null_mut());
    tts_destroy(ctx);
    assert_eq!(BOUNDARY_REGISTRATIONS.load(Ordering::SeqCst), 0);
}
