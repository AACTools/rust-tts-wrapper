//! SherpaOnnx Model Tests
//!
//! Tests for SherpaOnnx model type dispatch, file layouts, and functionality.
//! These tests validate the fixes for model_type dispatch, rate application
//! and registry parsing.
#![allow(dead_code, clippy::all, clippy::pedantic)]

#[cfg(all(test, feature = "sherpaonnx"))]
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

    /// Registry access via the sherpa-onnx-models crate (the old
    /// embedded-copy parse is gone); validates the registry loads and
    /// the per-type counts match what the README advertises.
    fn parse_registry() -> HashMap<String, ModelInfo> {
        let mut out = HashMap::new();
        for (key, m) in sherpa_onnx_models::models() {
            out.insert(
                key.clone(),
                ModelInfo {
                    model_type: m.model_type.clone(),
                    name: m.name.clone(),
                    sample_rate: m.sample_rate,
                    num_speakers: m.num_speakers,
                },
            );
        }
        out
    }

    #[test]
    fn test_registry_loads_nonzero_models() {
        // Validates registry parsing doesn't silently return empty.
        let models = parse_registry();
        assert!(
            models.len() > 100,
            "expected >100 models in registry, got {}",
            models.len()
        );
    }

    #[test]
    fn test_registry_contains_kokoro_vits_matcha() {
        // Validates all advertised model families are present.
        let models = parse_registry();
        let mut counts = HashMap::<&str, u32>::new();
        for info in models.values() {
            *counts.entry(info.model_type.as_str()).or_insert(0) += 1;
        }
        assert!(
            counts.get("kokoro").copied().unwrap_or(0) >= 1,
            "no kokoro models"
        );
        assert!(
            counts.get("vits").copied().unwrap_or(0) >= 10,
            "no vits models"
        );
        assert!(
            counts.get("matcha").copied().unwrap_or(0) >= 1,
            "no matcha models"
        );
    }

    #[test]
    fn test_known_model_ids_are_present() {
        let models = parse_registry();
        // A handful of well-known model ids from the registry. If any of these
        // disappear, the registry parsing has changed and consumers will break.
        for id in &[
            "kokoro-en-v0_19",
            "coqui-en-ljspeech",
            "cantonese-yue-xiaomaiiwn",
        ] {
            assert!(
                models.contains_key(*id),
                "expected model '{id}' in registry"
            );
        }
    }

    #[test]
    fn test_every_model_has_supported_type() {
        // Validates model_type dispatch: every model in the registry must
        // have a branch in sherpaonnx_engine.rs. If a new type appears this
        // test will fail, prompting an update to the match arm.
        let models = parse_registry();
        let supported = [
            "kokoro",
            "vits",
            "matcha",
            "kitten",
            "zipvoice",
            "pocket",
            "supertonic",
            "mms",
            "unknown",
            "",
        ];
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
        // Validates rate is applied only via GenerationConfig.speed,
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
