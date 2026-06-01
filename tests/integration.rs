//! Integration tests for the engine factory.

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
