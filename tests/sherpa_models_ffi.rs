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
    // English: needs_bundle with the front-end compiled in (provision
    // the eng-US bundle for the codepoints path); without it the
    // #66/#67 auto-lexicon path still speaks → ready (unlike German,
    // where the absence of the front-end blocks quality speech)
    #[cfg(feature = "sherpa-g2p")]
    assert_eq!(
        registry["piper-en_US-amy-low"]["readiness"],
        serde_json::Value::String("needs_bundle".to_string())
    );
    #[cfg(not(feature = "sherpa-g2p"))]
    assert_eq!(
        registry["piper-en_US-amy-low"]["readiness"],
        serde_json::Value::String("ready".to_string())
    );
    assert_eq!(
        registry["piper-en_US-amy-low"]["g2p_enabled"],
        serde_json::Value::Bool(cfg!(feature = "sherpa-g2p"))
    );
    // French flipped with the v0.4.0 locale validation: needs_bundle
    // with the front-end, blocked:g2p-unavailable without (same
    // reason class as German — validated data, absent front-end)
    expect_readiness(&registry, "piper-fr_FR-siwis-medium");
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
    // pl flipped with the batch-1 campaign (#92): pol validated
    expect_readiness(&registry, "piper-pl_PL-gosia-medium");
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

/// A validated-flip locale: `needs_bundle` when the G2P front-end is
/// compiled in, `blocked:g2p-unavailable` without it (validated data,
/// absent front-end — the fix is a rebuild, not new data).
#[cfg(feature = "sherpaonnx")]
fn expect_readiness(registry: &serde_json::Value, id: &str) {
    let expected = if cfg!(feature = "sherpa-g2p") {
        "needs_bundle"
    } else {
        "blocked:g2p-unavailable"
    };
    assert_eq!(
        registry[id]["readiness"],
        serde_json::Value::String(expected.to_string()),
        "{id}"
    );
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

    // et model: bundle exists (base manifest) but the locale is not
    // validated — pl flipped with batch-1, so et carries the
    // published-but-unvalidated case now
    assert!(!tts_provision_sherpa_g2p_bundle(c"coqui-et-cv".as_ptr()));
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

#[cfg(all(feature = "sherpaonnx", feature = "sherpa-g2p"))]
#[test]
#[ignore = "live: provisions from the network or cache (set VOICEGARDEN_LEXICON_DIR to skip the download)"]
fn provision_fetches_the_english_bundle() {
    // the #92 English migration: eng-US (expanded), same ABI as German
    use rust_tts_wrapper::tts_provision_sherpa_g2p_bundle;
    assert!(tts_provision_sherpa_g2p_bundle(
        c"piper-en_US-amy-low".as_ptr()
    ));
}

#[cfg(all(feature = "sherpaonnx", feature = "sherpa-g2p"))]
#[test]
#[ignore = "live: provisions from the network or cache (set VOICEGARDEN_LEXICON_DIR to skip the download)"]
fn provision_fetches_the_v040_locale_bundles() {
    // the #92 v0.4.0 locale flips: one probe per archive family —
    // fra (expanded) and ru (base, the only base-manifest row)
    use rust_tts_wrapper::tts_provision_sherpa_g2p_bundle;
    assert!(tts_provision_sherpa_g2p_bundle(
        c"piper-fr_FR-siwis-medium".as_ptr()
    ));
    assert!(tts_provision_sherpa_g2p_bundle(
        c"piper-ru_RU-dmitri-medium".as_ptr()
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

/// Read the global error detail as (code, message) — file-level twin
/// of the local helper in the provision tests.
#[cfg(feature = "sherpaonnx")]
fn global_detail() -> (u32, String) {
    use rust_tts_wrapper::{tts_free_string, tts_get_last_error_detail_json};
    let err = tts_get_last_error_detail_json(std::ptr::null_mut());
    assert!(!err.is_null());
    // SAFETY: owned NUL-terminated error string.
    let detail = unsafe { CStr::from_ptr(err) }
        .to_string_lossy()
        .into_owned();
    tts_free_string(err);
    let code = serde_json::from_str::<serde_json::Value>(&detail)
        .ok()
        .and_then(|v| v["code"].as_u64())
        .and_then(|c| u32::try_from(c).ok())
        .unwrap_or(0);
    (code, detail)
}

// ══════════════════════════════════════════════════════════════════
// Engine-agnostic model surface (#102 Phase 1)
// ══════════════════════════════════════════════════════════════════

use rust_tts_wrapper::tts_installed_voice_catalog_json;
#[cfg(feature = "sherpaonnx")]
use rust_tts_wrapper::{
    tts_model_info_json, tts_models_dir, tts_models_json, tts_search_models_json,
};

/// Read + parse + free a JSON string from the FFI.
fn json_from_ffi(ptr: *mut std::os::raw::c_char) -> serde_json::Value {
    assert!(!ptr.is_null(), "expected a JSON payload");
    // SAFETY: the FFI contract returns an owned NUL-terminated string
    let bytes = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_bytes().to_vec();
    rust_tts_wrapper::tts_free_string(ptr);
    serde_json::from_slice(&bytes).expect("valid JSON")
}

#[cfg(feature = "sherpaonnx")]
#[test]
fn models_catalogue_has_unified_availability() {
    let all = json_from_ffi(tts_models_json(std::ptr::null()));
    let arr = all.as_array().expect("array catalogue");
    assert!(!arr.is_empty(), "the sherpa catalogue is non-empty");
    // every entry carries the unified field set
    for e in arr.iter().take(50) {
        assert!(e["engine"].is_string(), "{e}");
        assert!(e["id"].is_string());
        assert!(e["availability"].is_string());
        assert!(e["installed"].is_boolean());
    }
    // engine-scoped catalogue: a subset of the null (all-engines)
    // catalogue — additional engines (floravox) may add rows
    let sherpa = json_from_ffi(tts_models_json(c"sherpaonnx".as_ptr()));
    let sherpa_arr = sherpa.as_array().unwrap();
    assert!(sherpa_arr.len() <= arr.len());
    assert!(sherpa_arr.iter().all(|e| e["engine"] == "sherpaonnx"));
    assert!(!sherpa_arr.is_empty(), "sherpa rows present");
    // a wrong engine id is an EMPTY catalogue (not an error)
    let none = json_from_ffi(tts_models_json(c"no-such-engine".as_ptr()));
    assert_eq!(none.as_array().unwrap().len(), 0);
}

#[cfg(feature = "sherpaonnx")]
#[test]
fn model_info_and_search_agree() {
    let amy = json_from_ffi(tts_model_info_json(
        c"sherpaonnx".as_ptr(),
        c"piper-en_US-amy-low".as_ptr(),
    ));
    assert_eq!(amy["id"], "piper-en_US-amy-low");
    assert!(amy["availability"].is_string());

    // a needle unique to one id — "amy" alone also matches "Saamya"
    // names, and models() iterates a HashMap (per-process order), so
    // a take(limited) result must never be order-dependent
    let hits = json_from_ffi(tts_search_models_json(c"piper-en_US-amy-low".as_ptr(), 5));
    let arr = hits.as_array().unwrap();
    assert!(
        arr.iter().any(|e| e["id"] == "piper-en_US-amy-low"),
        "{hits}"
    );
    // language search hits Spanish voices
    let es = json_from_ffi(tts_search_models_json(c"spanish".as_ptr(), 50));
    assert!(es.as_array().is_some_and(|a| !a.is_empty()));

    // unknown model → NULL + ModelNotFound detail
    assert!(tts_model_info_json(c"sherpaonnx".as_ptr(), c"zzz".as_ptr()).is_null());
    let (code, detail) = global_detail();
    assert_eq!(code, 5, "{detail}");
}

#[cfg(feature = "sherpaonnx")]
#[test]
fn models_dir_points_at_the_sherpa_layout() {
    let dir = json_from_ffi_str(tts_models_dir(c"sherpaonnx".as_ptr()));
    assert!(dir.ends_with("sherpaonnx"), "{dir}");
    // unknown engine → NULL + EngineNotAvailable
    assert!(tts_models_dir(c"zzz".as_ptr()).is_null());
    let (code, _) = global_detail();
    assert_eq!(code, 7);
}

#[cfg(feature = "sherpaonnx")]
fn json_from_ffi_str(ptr: *mut std::os::raw::c_char) -> String {
    assert!(!ptr.is_null());
    // SAFETY: owned NUL-terminated string per the FFI contract
    let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    rust_tts_wrapper::tts_free_string(ptr);
    s
}

/// The voice catalogue flattens INSTALLED models only — with a clean
/// models dir it is empty (never phantom entries).
#[test]
fn installed_voice_catalogue_lists_only_installed() {
    let v = json_from_ffi(tts_installed_voice_catalog_json());
    let arr = v.as_array().unwrap();
    // a CI machine may have models from earlier suites; everything
    // listed must at least be well-formed
    for e in arr {
        assert!(e["engine"].is_string(), "{e}");
        assert!(e["model"].is_string());
        assert!(e["voice"].is_string());
    }
}

/// #102 Phase 1 end-to-end, against the real network + registry:
/// install the smallest checksummed piper model into a temp
/// `TTS_MODELS_DIR`, watch it appear installed in the catalogue, then
/// uninstall. ~13 MB download.
///
/// ```sh
/// TTS_MODELS_LIVE=1 cargo test --features sherpaonnx,sherpa-g2p \
///   --test sherpa_models_ffi -- --ignored install --test-threads=1
/// ```
#[cfg(all(feature = "sherpaonnx", feature = "sherpa-g2p"))]
#[test]
#[ignore = "live: downloads ~13 MB (TTS_MODELS_LIVE=1; --test-threads=1 — mutates TTS_MODELS_DIR)"]
fn install_uninstall_round_trip() {
    if std::env::var_os("TTS_MODELS_LIVE").is_none() {
        eprintln!("skipping: set TTS_MODELS_LIVE=1 to run the live install");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let prior = std::env::var_os("TTS_MODELS_DIR");
    std::env::set_var("TTS_MODELS_DIR", tmp.path());

    assert!(rust_tts_wrapper::tts_model_install(
        c"sherpaonnx".as_ptr(),
        c"piper-fr_FR-siwis-low-int8".as_ptr(),
        None,
        None,
        std::ptr::null_mut(),
    ));

    let info = json_from_ffi(tts_model_info_json(
        c"sherpaonnx".as_ptr(),
        c"piper-fr_FR-siwis-low-int8".as_ptr(),
    ));
    assert_eq!(info["installed"], serde_json::json!(true), "{info}");
    assert!(
        tmp.path()
            .join("sherpaonnx/piper-fr_FR-siwis-low-int8/.installed")
            .is_file(),
        "neutral manifest written"
    );
    // the voice catalogue now contains its speaker
    let v = json_from_ffi(tts_installed_voice_catalog_json());
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .any(|e| e["model"] == "piper-fr_FR-siwis-low-int8"),
        "{v}"
    );

    assert!(rust_tts_wrapper::tts_model_uninstall(
        c"sherpaonnx".as_ptr(),
        c"piper-fr_FR-siwis-low-int8".as_ptr(),
    ));
    assert!(
        !tmp.path()
            .join("sherpaonnx/piper-fr_FR-siwis-low-int8")
            .exists(),
        "uninstalled"
    );
    match prior {
        Some(p) => std::env::set_var("TTS_MODELS_DIR", p),
        None => std::env::remove_var("TTS_MODELS_DIR"),
    }
}

/// The MMS two-file layout (HF directory url): the smallest exercised
/// path for the tokens floor fix — model.onnx digest-verified,
/// tokens.txt floor+shape validated, both parts at the models-dir
/// root. ~109 MB download.
///
/// ```sh
/// TTS_MODELS_LIVE=1 cargo test --features sherpaonnx --test sherpa_models_ffi \
///   -- --ignored mms_install --test-threads=1
/// ```
#[cfg(feature = "sherpaonnx")]
#[test]
#[ignore = "live: downloads ~109 MB (TTS_MODELS_LIVE=1; --test-threads=1 — mutates TTS_MODELS_DIR)"]
fn mms_install_two_file_layout() {
    if std::env::var_os("TTS_MODELS_LIVE").is_none() {
        eprintln!("skipping: set TTS_MODELS_LIVE=1 to run the live install");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let prior = std::env::var_os("TTS_MODELS_DIR");
    std::env::set_var("TTS_MODELS_DIR", tmp.path());
    assert!(rust_tts_wrapper::tts_model_install(
        c"sherpaonnx".as_ptr(),
        c"mms_eng".as_ptr(),
        None,
        None,
        std::ptr::null_mut(),
    ));
    let dir = tmp.path().join("sherpaonnx/mms_eng");
    assert!(dir.join("model.onnx").is_file(), "model fetched");
    assert!(dir.join("tokens.txt").is_file(), "tokens fetched");
    let info = json_from_ffi(tts_model_info_json(
        c"sherpaonnx".as_ptr(),
        c"mms_eng".as_ptr(),
    ));
    assert_eq!(info["installed"], serde_json::json!(true));
    assert!(rust_tts_wrapper::tts_model_uninstall(
        c"sherpaonnx".as_ptr(),
        c"mms_eng".as_ptr()
    ));
    match prior {
        Some(p) => std::env::set_var("TTS_MODELS_DIR", p),
        None => std::env::remove_var("TTS_MODELS_DIR"),
    }
}
