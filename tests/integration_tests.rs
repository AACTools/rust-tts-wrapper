//! Integration tests for critical fixes
//!
//! Tests that validate fixes for the Critical/High issues identified in the
//! audit. Uses the public API surface so the tests exercise the same
//! code paths as external consumers.
#![allow(clippy::all, clippy::pedantic)]

#[cfg(test)]
mod factory_tests {
    use rust_tts_wrapper::factory::{create_engine, engine_count, engine_list};

    #[test]
    fn test_engine_list_contains_builtins() {
        let list = engine_list();
        let ids: Vec<&str> = list.iter().map(|e| e.id.as_str()).collect();
        // Cloud engines should always be present in the default build.
        assert!(ids.contains(&"openai"), "missing openai; got {ids:?}");
        assert!(ids.contains(&"azure"), "missing azure; got {ids:?}");
        // sherpaonnx is only present when the feature is on.
        #[cfg(feature = "sherpaonnx")]
        assert!(
            ids.contains(&"sherpaonnx"),
            "missing sherpaonnx; got {ids:?}"
        );
    }

    #[test]
    fn test_engine_count_matches_list_len() {
        assert_eq!(engine_count(), engine_list().len());
    }

    #[test]
    fn test_create_unknown_engine_returns_none() {
        // should return None with a helpful warning rather than panic.
        assert!(create_engine("does-not-exist", "").is_none());
    }

    #[test]
    #[cfg(feature = "sherpaonnx")]
    fn test_create_sherpaonnx_engine_succeeds() {
        // with no modelId set, the engine still constructs (it errors
        // at speak-time, not construction-time).
        let engine = create_engine("sherpaonnx", "");
        assert!(engine.is_some(), "sherpaonnx should construct");
    }

    #[test]
    #[cfg(feature = "sherpaonnx")]
    fn test_create_sherpaonnx_engine_reads_num_threads() {
        // numThreads should be parsed from credentials without panicking.
        let creds = r#"{"numThreads":"4","provider":"cpu"}"#;
        let engine = create_engine("sherpaonnx", creds);
        assert!(engine.is_some());
    }
}

#[cfg(all(test, feature = "cloud"))]
mod cloud_engine_smoke_tests {
    use rust_tts_wrapper::factory::create_engine;

    #[test]
    fn test_cloud_engines_construct_without_panicking() {
        // All cloud engines should be constructible with dummy credentials.
        // We don't speak — just ensure the config builds.
        for id in [
            "openai",
            "elevenlabs",
            "azure",
            "google",
            "cartesia",
            "deepgram",
            "playht",
            "fishaudio",
            "hume",
            "mistral",
            "murf",
            "resemble",
            "unrealspeech",
            "upliftai",
            "watson",
            "witai",
            "xai",
            "modelslab",
        ] {
            let creds = match id {
                "watson" => r#"{"apiKey":"k","region":"us-east","instanceId":"i"}"#,
                "playht" => r#"{"apiKey":"k","userId":"u"}"#,
                _ => r#"{"apiKey":"k"}"#,
            };
            assert!(
                create_engine(id, creds).is_some(),
                "failed to construct cloud engine '{id}'"
            );
        }
    }

    #[test]
    fn test_polly_is_not_constructable() {
        // Polly needs SigV4 — engine creation must surface as None.
        let creds = r#"{"accessKeyId":"a","secretAccessKey":"s","region":"us-east-1"}"#;
        assert!(create_engine("polly", creds).is_none());
    }
}

#[cfg(test)]
mod engine_helpers_tests {
    use rust_tts_wrapper::engine::estimate_word_boundaries;

    #[test]
    fn test_estimate_empty_input() {
        assert!(estimate_word_boundaries("").is_empty());
        assert!(estimate_word_boundaries("   ").is_empty());
    }

    #[test]
    fn test_estimate_single_word() {
        let b = estimate_word_boundaries("Hello");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].text, "Hello");
        assert_eq!(b[0].offset, 0);
        assert!(b[0].duration > 0);
    }

    #[test]
    fn test_estimate_offsets_monotonic() {
        let b = estimate_word_boundaries("one two three four");
        assert_eq!(b.len(), 4);
        for w in b.windows(2) {
            assert!(w[0].offset < w[1].offset);
            // Boundary text must be one of the words.
            assert!(!w[0].text.is_empty());
        }
    }
}

#[cfg(test)]
mod ffi_boundary_tests {
    // The FFI surface is exercised through the C entry points in lib.rs.
    // Since integration tests share the crate's public API, we can call the
    // extern "C" fns directly and assert they behave at the boundary.

    use rust_tts_wrapper::{tts_create, tts_destroy, tts_get_engine_count};

    #[test]
    fn test_ffi_null_engine_id_does_not_panic() {
        // passing null must be caught by catch_unwind and return null.
        let ptr = tts_create(std::ptr::null(), std::ptr::null());
        assert!(ptr.is_null());
    }

    #[test]
    fn test_ffi_create_unknown_engine_returns_null() {
        let id = std::ffi::CString::new("definitely-not-an-engine").unwrap();
        let ptr = tts_create(id.as_ptr(), std::ptr::null());
        assert!(ptr.is_null());
    }

    #[test]
    fn test_ffi_destroy_null_is_noop() {
        // tts_destroy accepts null as a no-op without panicking.
        tts_destroy(std::ptr::null_mut());
    }

    #[test]
    fn test_ffi_engine_count_positive() {
        assert!(tts_get_engine_count() > 0);
    }
}
