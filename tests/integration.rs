//! Integration tests for the TTS wrapper.

use rust_tts_wrapper::factory;

#[test]
fn test_engine_list_contains_system() {
    let engines = factory::engine_list();
    assert!(
        engines.iter().any(|e| e.id == "system"),
        "System engine must be registered"
    );
}

#[test]
fn test_engine_list_contains_cloud() {
    let engines = factory::engine_list();
    let cloud_ids = [
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
        "polly",
    ];
    for id in &cloud_ids {
        assert!(
            engines.iter().any(|e| e.id == *id),
            "Cloud engine '{id}' must be registered"
        );
    }
}

#[test]
fn test_engine_count() {
    let engines = factory::engine_list();
    assert!(
        engines.len() >= 21,
        "Must have at least 21 engines, got {}",
        engines.len()
    );
}

#[test]
fn test_cloud_engine_needs_credentials() {
    let engines = factory::engine_list();
    let openai = engines
        .iter()
        .find(|e| e.id == "openai")
        .expect("openai engine");
    assert!(openai.needs_credentials);
    assert!(openai.credential_keys_json.contains("apiKey"));
}

#[test]
fn test_system_engine_no_credentials() {
    let engines = factory::engine_list();
    let system = engines
        .iter()
        .find(|e| e.id == "system")
        .expect("system engine");
    assert!(!system.needs_credentials);
}

#[test]
fn test_sherpaonnx_no_credentials() {
    let engines = factory::engine_list();
    let sherpa = engines
        .iter()
        .find(|e| e.id == "sherpaonnx")
        .expect("sherpaonnx engine");
    assert!(!sherpa.needs_credentials);
}

#[test]
fn test_create_system_engine() {
    let engine = factory::create_engine("system", "");
    assert!(engine.is_some(), "System engine must be creatable");
    assert_eq!(engine.unwrap().engine_id(), "system");
}

#[test]
fn test_create_cloud_engine() {
    let engine = factory::create_engine("openai", r#"{"apiKey":"test-key"}"#);
    assert!(
        engine.is_some(),
        "OpenAI engine must be creatable with credentials"
    );
}

#[test]
fn test_create_unknown_engine() {
    let engine = factory::create_engine("nonexistent_engine", "");
    assert!(engine.is_none(), "Unknown engines should return None");
}

#[test]
fn test_system_engine_stop_graceful() {
    let engine = factory::create_engine("system", "").expect("system engine");
    let result = engine.stop();
    if result.is_err() {
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("not connected"),
            "Expected 'not connected' error, got: {err}"
        );
    }
}

#[test]
fn test_create_all_cloud_engines() {
    let cloud_ids = [
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
        "xai",
        "modelslab",
    ];
    for id in &cloud_ids {
        let engine = factory::create_engine(id, r#"{"apiKey":"test-key"}"#);
        assert!(engine.is_some(), "Engine '{id}' should be creatable");
    }
}

#[test]
fn test_create_azure_with_region() {
    let engine = factory::create_engine(
        "azure",
        r#"{"subscriptionKey":"test-key","region":"eastus"}"#,
    );
    assert!(engine.is_some());
}

#[test]
fn test_create_watson_with_all_creds() {
    let engine = factory::create_engine(
        "watson",
        r#"{"apiKey":"test-key","region":"us-east","instanceId":"test-id"}"#,
    );
    assert!(engine.is_some());
}

#[test]
fn test_create_polly_with_all_creds() {
    let engine = factory::create_engine(
        "polly",
        r#"{"accessKeyId":"test","secretAccessKey":"test","region":"us-east-1"}"#,
    );
    assert!(engine.is_some());
}

#[test]
fn test_sherpaonnx_engine_has_voices() {
    let engine = factory::create_engine("sherpaonnx", "").expect("sherpaonnx engine");
    let voices = engine.get_voices().expect("voices");
    assert!(!voices.is_empty(), "SherpaONNX should have voices");
    assert_eq!(voices[0].provider, "sherpaonnx");
}

#[test]
fn test_engine_id_matches() {
    let engine = factory::create_engine("openai", r#"{"apiKey":"test-key"}"#).unwrap();
    assert_eq!(engine.engine_id(), "openai");
}

#[test]
fn test_normalize_gender() {
    use rust_tts_wrapper::types::normalize_gender;
    assert_eq!(normalize_gender("Female"), "Female");
    assert_eq!(normalize_gender("female"), "Female");
    assert_eq!(normalize_gender("Male"), "Male");
    assert_eq!(normalize_gender("male"), "Male");
    assert_eq!(normalize_gender(""), "Unknown");
    assert_eq!(normalize_gender("other"), "Unknown");
}

#[test]
fn test_voice_struct_fields() {
    use rust_tts_wrapper::types::{LanguageCode, Voice};
    let voice = Voice {
        id: "test-voice".to_string(),
        name: "Test Voice".to_string(),
        gender: "Female".to_string(),
        provider: "test".to_string(),
        language_codes: vec![LanguageCode {
            bcp47: "en-US".to_string(),
            iso639_3: "eng".to_string(),
            display: "English (United States)".to_string(),
        }],
    };
    assert_eq!(voice.primary_language(), "en-US");
    assert_eq!(voice.language_codes.len(), 1);
}

#[test]
fn test_word_boundary_estimation() {
    use rust_tts_wrapper::engine::estimate_word_boundaries;
    let boundaries = estimate_word_boundaries("Hello world this is a test");
    assert_eq!(boundaries.len(), 6);
    assert_eq!(boundaries[0].text, "Hello");
    assert_eq!(boundaries[0].offset, 0);
    assert!(boundaries[0].duration > 0);
    // Offsets should be monotonically increasing
    for i in 1..boundaries.len() {
        assert!(boundaries[i].offset > boundaries[i - 1].offset);
    }
}

#[test]
fn test_word_boundary_empty_text() {
    use rust_tts_wrapper::engine::estimate_word_boundaries;
    let boundaries = estimate_word_boundaries("");
    assert!(boundaries.is_empty());
}

#[test]
fn test_speech_markdown_detection() {
    use rust_tts_wrapper::engine::preprocess_speech_markdown;
    let (result, is_ssml) =
        preprocess_speech_markdown("Hello (world)[emphasis:\"strong\"]", "azure");
    assert!(is_ssml);
    assert!(result.contains("<speak>"));
}

#[test]
fn test_speech_markdown_plain_text_passthrough() {
    use rust_tts_wrapper::engine::preprocess_speech_markdown;
    let (result, is_ssml) = preprocess_speech_markdown("Hello world", "azure");
    assert!(!is_ssml);
    assert_eq!(result, "Hello world");
}

#[test]
fn test_speech_markdown_azure_platform() {
    use rust_tts_wrapper::engine::preprocess_speech_markdown;
    let (result, is_ssml) = preprocess_speech_markdown("This is +important+", "azure");
    assert!(is_ssml);
    assert!(result.contains("microsoft") || result.contains("<speak>"));
}

#[test]
fn test_speech_markdown_google_platform() {
    use rust_tts_wrapper::engine::preprocess_speech_markdown;
    let (_result, is_ssml) = preprocess_speech_markdown("This is +important+", "google");
    assert!(is_ssml);
}
