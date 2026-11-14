//! ABI coverage for the embedded Sherpa-ONNX model catalog.

use rust_tts_wrapper::{tts_free_string, tts_get_sherpa_models_json};
use std::ffi::CStr;

#[cfg(feature = "sherpaonnx")]
#[test]
fn returns_the_registry_enriched_with_g2p_eligibility() {
    let ptr = tts_get_sherpa_models_json();
    assert!(!ptr.is_null());
    // SAFETY: the export returns an owned, NUL-terminated C string.
    let json = unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec();
    tts_free_string(ptr);
    let registry: serde_json::Value = serde_json::from_slice(&json).unwrap();

    // the snapshot's own fields are preserved
    let model = &registry["inflect-en-nano-v2"];
    assert_eq!(model["id"], "inflect-en-nano-v2");
    assert_eq!(model["engines"], "sherpa-onnx");
    assert!(model["url"]
        .as_str()
        .is_some_and(|url| url.starts_with("https://")));

    // the enrichment: g2p_enabled + readiness on every model (Dasher
    // decodes snake_case → g2pEnabled/readiness for eligibility)
    for (_id, m) in registry.as_object().unwrap() {
        assert!(
            m.get("g2p_enabled")
                .and_then(serde_json::Value::as_bool)
                .is_some(),
            "every model carries g2p_enabled"
        );
        let r = m["readiness"]
            .as_str()
            .expect("every model carries readiness");
        assert!(
            r == "ready"
                || r == "needs_bundle"
                || r == "unverified"
                || r.starts_with("needs_assets:")
                || r.starts_with("blocked:"),
            "documented value set, got {r}"
        );
    }
    assert_eq!(
        registry["piper-de_DE-thorsten-medium"]["g2p_enabled"],
        serde_json::Value::Bool(cfg!(feature = "sherpa-g2p"))
    );
    // readiness: German = needs_bundle when G2P is compiled in
    // (provision at install); without the feature the validated
    // bundle exists but this build lacks the front-end → its own
    // reason, NOT "no-bundle"
    #[cfg(feature = "sherpa-g2p")]
    assert_eq!(
        registry["piper-de_DE-thorsten-medium"]["readiness"],
        serde_json::Value::String("needs_bundle".to_string())
    );
    #[cfg(not(feature = "sherpa-g2p"))]
    assert_eq!(
        registry["piper-de_DE-thorsten-medium"]["readiness"],
        serde_json::Value::String("blocked:g2p-unavailable".to_string())
    );
    assert_eq!(
        registry["piper-en_US-amy-low"]["readiness"],
        serde_json::Value::String("ready".to_string())
    );
    assert_eq!(
        registry["piper-fr_FR-siwis-medium"]["readiness"],
        serde_json::Value::String("blocked:bundle-not-validated".to_string())
    );
    // Hungarian: in neither voicegarden manifest
    assert_eq!(
        registry["piper-hu_HU-berta-medium"]["readiness"],
        serde_json::Value::String("blocked:no-bundle".to_string())
    );
    // deprecated supertonic: ready-looking archive, retired upstream
    assert_eq!(
        registry["supertonic-3-multilingual"]["readiness"],
        serde_json::Value::String("blocked:deprecated".to_string())
    );
    // pl HAS an expanded bundle (pol.tar.gz) — not validated yet
    assert_eq!(
        registry["piper-pl_PL-gosia-medium"]["readiness"],
        serde_json::Value::String("blocked:bundle-not-validated".to_string())
    );
    assert_eq!(
        registry["kokoro-zh_en-int8"]["readiness"],
        serde_json::Value::String("ready".to_string())
    );
    // MMS: no sha256 in the registry, native front-end
    assert_eq!(
        registry["mms_abp"]["readiness"],
        serde_json::Value::String("unverified".to_string())
    );

    // additive shape: re-serializing the raw snapshot minus g2p fields
    // equals the export minus g2p fields (no registry field changed)
    let mut raw: serde_json::Value = serde_json::from_str(sherpa_onnx_models::MODELS_JSON).unwrap();
    let mut got = registry.clone();
    for m in raw.as_object_mut().unwrap().values_mut() {
        m.as_object_mut().unwrap().remove("g2p_enabled");
    }
    for m in got.as_object_mut().unwrap().values_mut() {
        m.as_object_mut().unwrap().remove("g2p_enabled");
        m.as_object_mut().unwrap().remove("readiness");
    }
    assert_eq!(raw, got, "the enrichment must be purely additive");
}

#[cfg(all(feature = "sherpaonnx", feature = "sherpa-g2p"))]
#[test]
fn provision_reports_errors_for_ineligible_models() {
    use rust_tts_wrapper::{
        tts_free_string, tts_get_last_error_detail_json, tts_provision_sherpa_g2p_bundle,
    };

    fn last_detail() -> (u32, String) {
        let err = tts_get_last_error_detail_json(std::ptr::null_mut());
        assert!(!err.is_null());
        // SAFETY: owned NUL-terminated error string.
        let detail = unsafe { CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned();
        tts_free_string(err);
        // the detail JSON carries a numeric code — assert it so
        // programmatic consumers see the documented codes, not vibes
        let code = serde_json::from_str::<serde_json::Value>(&detail)
            .ok()
            .and_then(|d| d["code"].as_u64())
            .and_then(|c| u32::try_from(c).ok())
            .unwrap_or_default();
        (code, detail)
    }

    // unknown id → ModelNotFound (5)
    assert!(!tts_provision_sherpa_g2p_bundle(c"does-not-exist".as_ptr()));
    let (code, detail) = last_detail();
    assert_eq!(code, 5, "ModelNotFound: {detail}");
    assert!(detail.contains("not found"), "{detail}");

    // English model (in the registry, no validated bundle row) →
    // ModelNotFound per the header contract, with a distinct message
    assert!(!tts_provision_sherpa_g2p_bundle(
        c"piper-en_US-amy-low".as_ptr()
    ));
    let (code, detail) = last_detail();
    assert_eq!(code, 5, "{detail}");
    assert!(detail.contains("no validated G2P bundle"), "{detail}");

    // null pointer → InvalidConfiguration (2), no crash
    assert!(!tts_provision_sherpa_g2p_bundle(std::ptr::null()));
    let (code, detail) = last_detail();
    assert_eq!(code, 2, "InvalidConfiguration: {detail}");
}

#[cfg(all(feature = "sherpaonnx", feature = "sherpa-g2p"))]
#[test]
#[ignore = "live: provisions from the network or cache (set VOICEGARDEN_LEXICON_DIR to skip the download)"]
fn provision_fetches_the_german_bundle() {
    use rust_tts_wrapper::tts_provision_sherpa_g2p_bundle;
    assert!(tts_provision_sherpa_g2p_bundle(
        c"piper-de_DE-thorsten-medium".as_ptr()
    ));
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
