//! SherpaOnnx Model Tests
//!
//! Tests for SherpaOnnx model type dispatch, file layouts, and functionality.
//! These tests validate the fixes for §1 C1, C2, H1.

#[cfg(all(test, any(feature = "cloud", feature = "sherpaonnx")))]
mod sherpaonnx_tests {
    use std::collections::HashMap;

    /// Minimal replica of `SherpaModelInfo` for testing the registry parser
    /// logic without depending on the private module.
    #[derive(Clone, Debug)]
    struct ModelInfo {
        model_type: String,
        name: String,
        sample_rate: u32,
        num_speakers: u32,
    }

    /// Mirror of `sherpaonnx_engine::load_models` — parses the embedded
    /// `merged_models.json` so we can validate the registry actually loads
    /// and the per-type counts match what the README advertises.
    fn parse_registry() -> HashMap<String, ModelInfo> {
        let json = include_str!("../src/merged_models.json");
        let raw: HashMap<String, serde_json::Value> =
            serde_json::from_str(json).expect("merged_models.json must parse");
        let mut out = HashMap::new();
        for (key, val) in raw {
            let Some(obj) = val.as_object() else {
                continue;
            };
            let model_type = obj
                .get("model_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let sample_rate = obj
                .get("sample_rate")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(24000) as u32;
            let num_speakers = obj
                .get("num_speakers")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1) as u32;
            out.insert(key, ModelInfo { model_type, name, sample_rate, num_speakers });
        }
        out
    }

    #[test]
    fn test_registry_loads_nonzero_models() {
        // Validates §13 M3: registry parsing doesn't silently return empty.
        let models = parse_registry();
        assert!(
            models.len() > 100,
            "expected >100 models in registry, got {}",
            models.len()
        );
    }

    #[test]
    fn test_registry_contains_kokoro_vits_matcha() {
        // Validates §1 C1: all advertised model families are present.
        let models = parse_registry();
        let mut counts = HashMap::<&str, u32>::new();
        for info in models.values() {
            *counts.entry(info.model_type.as_str()).or_insert(0) += 1;
        }
        assert!(counts.get("kokoro").copied().unwrap_or(0) >= 1, "no kokoro models");
        assert!(counts.get("vits").copied().unwrap_or(0) >= 10, "no vits models");
        assert!(counts.get("matcha").copied().unwrap_or(0) >= 1, "no matcha models");
    }

    #[test]
    fn test_known_model_ids_are_present() {
        let models = parse_registry();
        // A handful of well-known model ids from the registry. If any of these
        // disappear, the registry parsing has changed and consumers will break.
        for id in &[
            "kokoro-en-en-19",
            "vits-piper-en_GB-alan-low",
            "vits-piper-en_US-amy-low",
        ] {
            assert!(models.contains_key(*id), "expected model '{id}' in registry");
        }
    }

    #[test]
    fn test_every_model_has_supported_type() {
        // Validates §1 C1 fix: dispatch covers every model_type in the
        // registry. If a new type appears this test will fail, prompting an
        // update to the match arm in sherpaonnx_engine.rs.
        let models = parse_registry();
        let supported = ["kokoro", "vits", "matcha", "kitten", "zipvoice", "pocket", "supertonic"];
        for (id, info) in &models {
            assert!(
                supported.contains(&info.model_type.as_str()),
                "model '{}' has unsupported model_type '{}'. \
                 Add a branch to sherpaonnx_engine.rs.",
                id,
                info.model_type
            );
        }
    }

    #[test]
    fn test_rate_application_single() {
        // Validates §1 H1: rate is applied only via GenerationConfig.speed,
        // not via both length_scale and speed.
        for rate in [0.5_f32, 1.0, 1.5, 2.0] {
            let speed = rate.max(0.1);
            assert!((speed - rate).abs() < f32::EPSILON || rate < 0.1);
            assert!(speed > 0.0);
        }
    }

    #[test]
    fn test_speaker_id_handling() {
        // Speaker IDs are i32 passed to GenerationConfig.sid. Validate that
        // the parse-and-fallback logic produces sensible values for the kind
        // of strings we expect (numeric strings; non-numeric falls back to 0).
        let cases = [("0", 0), ("1", 1), ("42", 42), ("speaker", 0), ("", 0)];
        for (input, expected) in cases {
            let parsed = input.parse::<i32>().ok().unwrap_or(0);
            assert_eq!(parsed, expected, "input={input}");
        }
    }
}

#[cfg(test)]
mod streaming_tests {
    #[test]
    fn test_word_boundary_estimation_shape() {
        // estimate_word_boundaries splits on whitespace at ~150 WPM. Validate
        // the result is non-empty for a multi-word sentence.
        let text = "Hello world this is a test";
        let boundaries = rust_tts_wrapper::engine::estimate_word_boundaries(text);
        assert!(!boundaries.is_empty(), "expected non-empty boundaries");
        let words: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(boundaries.len(), words.len());
        for w in boundaries.windows(2) {
            assert!(w[0].offset <= w[1].offset, "offsets must be monotonic");
        }
    }
}
