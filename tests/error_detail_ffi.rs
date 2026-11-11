//! FFI suite for the GH #68/#69 surface: stable error codes, the
//! structured error detail JSON, the typed engine setup schema, and the
//! tristate credential check.
//!
//! Same offline-deterministic strategy as `ffi_conformance.rs`: cloud
//! engines construct without network access; a `synthUrl` override
//! pointing at a closed local port makes synthesis failures
//! deterministic (connection refused, no DNS, no timeouts).

#![allow(clippy::all, clippy::pedantic)]
#![cfg(feature = "cloud")]

use rust_tts_wrapper::{
    tts_check_credentials, tts_create, tts_destroy, tts_free_string, tts_get_engine_setups_json,
    tts_get_last_error_code, tts_get_last_error_detail_json, tts_speak_sync,
};
use std::ffi::{CStr, CString};

fn ctx_for(engine: &str, creds: &str) -> *mut rust_tts_wrapper::tts_ctx {
    let id = CString::new(engine).unwrap();
    let c = CString::new(creds).unwrap();
    let ctx = tts_create(id.as_ptr(), c.as_ptr());
    assert!(!ctx.is_null(), "tts_create({engine}) must succeed");
    ctx
}

fn last_code(ctx: *mut rust_tts_wrapper::tts_ctx) -> u32 {
    tts_get_last_error_code(ctx)
}

fn detail_json(ctx: *mut rust_tts_wrapper::tts_ctx) -> Option<String> {
    let p = tts_get_last_error_detail_json(ctx);
    if p.is_null() {
        return None;
    }
    // SAFETY: p is a heap C string owned by the caller after return.
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    rust_tts_wrapper::tts_free_string(p);
    Some(s)
}

// ---------------------------------------------------------------------------
// tts_create failures → stable codes + structured detail (GH #69)
// ---------------------------------------------------------------------------

#[test]
fn unknown_engine_yields_engine_not_available() {
    let id = CString::new("definitely-not-an-engine").unwrap();
    let ctx = tts_create(id.as_ptr(), std::ptr::null());
    assert!(ctx.is_null(), "unknown engine must fail create");

    // Global code (ctx is null) classifies as EngineNotAvailable = 7.
    assert_eq!(tts_get_last_error_code(std::ptr::null_mut()), 7);

    // Detail JSON carries the stable name + engineId, and a
    // display-friendly message.
    let json = detail_json(std::ptr::null_mut()).expect("detail after create failure");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["code"], 7, "code field: {json}");
    assert_eq!(v["name"], "EngineNotAvailable", "name field: {json}");
    assert_eq!(
        v["engineId"], "definitely-not-an-engine",
        "engineId field: {json}"
    );
    assert!(
        v["displayMessage"]
            .as_str()
            .unwrap()
            .contains("not available"),
        "displayMessage: {json}"
    );
}

#[test]
fn azure_missing_key_reports_missing_configuration_with_field() {
    // Create succeeds (cloud engines are lazy); the credential check
    // surfaces the missing required field with its key (GH #69).
    let ctx = ctx_for("azure", r#"{}"#);
    let rc = tts_check_credentials(ctx);
    assert_eq!(rc, -1, "missing subscriptionKey must be an error");
    assert_eq!(last_code(ctx), 1, "MissingConfiguration");
    let json = detail_json(ctx).expect("detail after missing-field check");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["fieldKey"], "subscriptionKey", "fieldKey: {json}");
    assert_eq!(v["engineId"], "azure", "engineId: {json}");
    tts_destroy(ctx);
}

// ---------------------------------------------------------------------------
// Tristate credential check (GH #69)
// ---------------------------------------------------------------------------

#[test]
fn openai_reports_unverified_without_network() {
    // OpenAI has no authenticated voice-list endpoint: the check must
    // answer Unverified (=2) deterministically, offline — never
    // "verified" on the strength of a static catalog.
    let ctx = ctx_for("openai", r#"{"apiKey":"dummy-key"}"#);
    assert_eq!(tts_check_credentials(ctx), 2, "openai → Unverified");
    tts_destroy(ctx);
}

#[test]
fn rejected_credentials_report_deterministically() {
    // GH #69 acceptance: "rejected credentials" covered end-to-end and
    // deterministically — a voicesUrl override pointing at a local
    // server that answers 401 (no DNS, no timeouts, works offline).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            use std::io::{Read, Write};
            let mut s = stream;
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf); // drain the request line/headers
            let _ = s.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let ctx = ctx_for(
        "elevenlabs",
        &format!(r#"{{"apiKey":"dummy-key","voicesUrl":"http://127.0.0.1:{port}/voices"}}"#),
    );
    assert_eq!(tts_check_credentials(ctx), 0, "401 → Rejected");
    tts_destroy(ctx);
    let _ = server.join();
}

#[test]
fn service_outage_is_not_a_rejection() {
    // 5xx must NOT tell the user to fix their credentials: the check
    // errors (retry) instead of returning Rejected.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            use std::io::{Read, Write};
            let mut s = stream;
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let ctx = ctx_for(
        "elevenlabs",
        &format!(r#"{{"apiKey":"dummy-key","voicesUrl":"http://127.0.0.1:{port}/voices"}}"#),
    );
    let rc = tts_check_credentials(ctx);
    assert_eq!(rc, -1, "503 → check failed (retryable), not Rejected");
    // And the classified detail names a service error, not a rejection.
    let json = detail_json(ctx).unwrap_or_default();
    assert!(
        json.contains("service error"),
        "expected service-error detail: {json}"
    );
    tts_destroy(ctx);
    let _ = server.join();
}

#[test]
fn malformed_credentials_json_is_invalid_configuration() {
    // GH #69: broken JSON must not masquerade as a missing field (the
    // user did fill the fields in).
    let ctx = ctx_for("azure", r#"{"subscriptionKey": "k", "region": tru}"#);
    let rc = tts_check_credentials(ctx);
    assert_eq!(rc, -1);
    assert_eq!(last_code(ctx), 2, "InvalidConfiguration");
    let json = detail_json(ctx).unwrap_or_default();
    assert!(json.contains("not valid JSON"), "{json}");
    tts_destroy(ctx);
}

#[test]
fn check_credentials_null_ctx_is_error() {
    assert_eq!(tts_check_credentials(std::ptr::null_mut()), -1);
}

// ---------------------------------------------------------------------------
// Engine setup schema (GH #68)
// ---------------------------------------------------------------------------

#[test]
fn engine_setups_json_is_parseable_and_covers_azure() {
    let p = tts_get_engine_setups_json();
    assert!(!p.is_null(), "setups JSON must not be null");
    // SAFETY: p is a heap C string owned by the caller after return.
    let json = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    tts_free_string(p);

    let arr: serde_json::Value = serde_json::from_str(&json).expect("valid JSON array");
    let arr = arr.as_array().expect("top-level array");
    assert!(!arr.is_empty(), "at least one engine in every build");

    let azure = arr
        .iter()
        .find(|v| v["id"] == "azure")
        .expect("azure compiled in under the cloud feature");
    assert_eq!(azure["needsCredentials"], true);
    let fields = azure["fields"].as_array().expect("azure fields");
    let key = fields
        .iter()
        .find(|f| f["key"] == "subscriptionKey")
        .expect("subscriptionKey field");
    assert_eq!(key["kind"], "secret");
    assert_eq!(key["required"], true);
    let region = fields
        .iter()
        .find(|f| f["key"] == "region")
        .expect("region field");
    assert_ne!(region["choices"].as_array().unwrap().len(), 0);
    assert_eq!(
        azure["capabilities"]["supportsStreaming"], true,
        "azure capabilities present"
    );
}

#[test]
fn free_string_accepts_null() {
    // Bindings call tts_free_string(null) defensively; it must be a
    // safe no-op.
    tts_free_string(std::ptr::null_mut());
}

// ---------------------------------------------------------------------------
// Classified synthesis failures (GH #69)
// ---------------------------------------------------------------------------

#[test]
fn unreachable_provider_yields_network_error_code() {
    // synthUrl override → 127.0.0.1:1 is closed on every CI runner:
    // deterministic connection-refused without DNS or timeouts.
    let ctx = ctx_for(
        "openai",
        r#"{"apiKey":"dummy-key","synthUrl":"http://127.0.0.1:1/v1/audio/speech"}"#,
    );
    let text = CString::new("network classification probe").unwrap();
    let rc = tts_speak_sync(ctx, text.as_ptr());
    assert_ne!(rc, 0, "closed-port synth must fail");
    assert_eq!(
        last_code(ctx),
        4,
        "NetworkError, got {}/{}",
        last_code(ctx),
        detail_json(ctx).unwrap_or_default()
    );
    tts_destroy(ctx);
}

#[test]
fn error_detail_redacts_secrets() {
    // A message echoing the key back must not leak it through the
    // detail JSON.
    let ctx = ctx_for(
        "openai",
        r#"{"apiKey":"sk-abcdef1234567890","synthUrl":"http://127.0.0.1:1/v1/audio/speech"}"#,
    );
    let text = CString::new("redaction probe").unwrap();
    let _ = tts_speak_sync(ctx, text.as_ptr());
    let json = detail_json(ctx).unwrap_or_default();
    assert!(
        !json.contains("abcdef1234567890"),
        "secret leaked into detail: {json}"
    );
    tts_destroy(ctx);
}
