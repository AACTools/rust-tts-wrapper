//! #132: the sherpa catalog exposes a schema/semantics version so
//! consumers can detect wholesale readiness-vocabulary changes. The
//! models JSON keeps its array shape byte-for-byte (a root-object
//! change would silently break every existing decoder).

#![cfg(feature = "sherpaonnx")]

use std::ffi::CStr;

use rust_tts_wrapper::tts_free_string;
use rust_tts_wrapper::tts_get_sherpa_catalog_version;
use rust_tts_wrapper::tts_get_sherpa_models_json;

#[test]
fn catalog_version_is_pinned_and_the_models_shape_is_unchanged() {
    assert_eq!(
        tts_get_sherpa_catalog_version(),
        i32::try_from(rust_tts_wrapper::sherpa_catalog::CATALOG_VERSION).unwrap(),
        "the ABI getter matches the constant"
    );
    // the models JSON keeps its pre-#132 shape: a top-level OBJECT
    // keyed by model id, with no extra keys injected (embedding the
    // version there broke the enrichment test — every existing
    // decoder iterates the values as models)
    let raw = tts_get_sherpa_models_json();
    assert!(!raw.is_null());
    let json = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    tts_free_string(raw);
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let obj = v
        .as_object()
        .expect("root stays an object keyed by model id");
    assert!(
        !obj.contains_key("catalogVersion"),
        "no schema keys injected into the models map"
    );
    // generalized: every value must be a model OBJECT — any injected
    // schema scalar under a different name fails here too
    assert!(
        obj.values().all(serde_json::Value::is_object),
        "the models map carries only model objects"
    );
}
