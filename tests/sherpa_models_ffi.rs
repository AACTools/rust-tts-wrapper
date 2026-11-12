//! ABI coverage for the embedded Sherpa-ONNX model catalog.

use rust_tts_wrapper::{tts_free_string, tts_get_sherpa_models_json};
use std::ffi::CStr;

#[cfg(feature = "sherpaonnx")]
#[test]
fn returns_the_embedded_registry_snapshot() {
    let ptr = tts_get_sherpa_models_json();
    assert!(!ptr.is_null());
    // SAFETY: the export returns an owned, NUL-terminated C string.
    let json = unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec();
    tts_free_string(ptr);
    assert!(
        json == sherpa_onnx_models::MODELS_JSON.as_bytes(),
        "the export must preserve the exact registry snapshot"
    );
    let registry: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let model = &registry["inflect-en-nano-v2"];
    assert_eq!(model["id"], "inflect-en-nano-v2");
    assert_eq!(model["engines"], "sherpa-onnx");
    assert!(model["url"]
        .as_str()
        .is_some_and(|url| url.starts_with("https://")));
}

#[cfg(not(feature = "sherpaonnx"))]
#[test]
fn reports_when_sherpa_is_disabled() {
    use rust_tts_wrapper::tts_get_last_error_detail_json;

    assert!(tts_get_sherpa_models_json().is_null());
    let ptr = tts_get_last_error_detail_json(std::ptr::null_mut());
    assert!(!ptr.is_null());
    // SAFETY: the error detail is an owned, NUL-terminated C string.
    let detail: serde_json::Value =
        serde_json::from_slice(unsafe { CStr::from_ptr(ptr) }.to_bytes()).unwrap();
    tts_free_string(ptr);
    assert_eq!(detail["name"], "EngineNotAvailable");
    assert_eq!(detail["engineId"], "sherpaonnx");
}
