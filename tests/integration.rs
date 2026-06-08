use rust_tts_wrapper::factory;

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
        engines.len() >= 19,
        "Must have at least 19 engines, got {}",
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

#[cfg(feature = "system")]
#[test]
fn test_system_engine_no_credentials() {
    let engines = factory::engine_list();
    let system = engines
        .iter()
        .find(|e| e.id == "system")
        .expect("system engine");
    assert!(!system.needs_credentials);
}

#[cfg(feature = "sherpaonnx")]
#[test]
fn test_sherpaonnx_no_credentials() {
    let engines = factory::engine_list();
    let sherpa = engines
        .iter()
        .find(|e| e.id == "sherpaonnx")
        .expect("sherpaonnx engine");
    assert!(!sherpa.needs_credentials);
}

#[cfg(feature = "system")]
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

#[cfg(feature = "system")]
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

#[cfg(feature = "sherpaonnx")]
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
    use rust_tts_wrapper::types::{normalize_gender, Gender};
    assert_eq!(normalize_gender("Female"), Gender::Female);
    assert_eq!(normalize_gender("female"), Gender::Female);
    assert_eq!(normalize_gender("Male"), Gender::Male);
    assert_eq!(normalize_gender("male"), Gender::Male);
    assert_eq!(normalize_gender(""), Gender::Unknown);
    assert_eq!(normalize_gender("other"), Gender::Unknown);
}

#[test]
fn test_voice_struct_fields() {
    use rust_tts_wrapper::types::{Gender, LanguageCode, Voice};
    let voice = Voice {
        id: "test-voice".to_string(),
        name: "Test Voice".to_string(),
        gender: Gender::Female,
        provider: "test".to_string(),
        language_codes: vec![LanguageCode {
            bcp47: "en-US".to_string(),
            iso639_3: "eng".to_string(),
            display: "English (United States)".to_string(),
        }],
    };
    assert_eq!(voice.primary_language(), "en-US");
    assert_eq!(voice.language_codes.len(), 1);
    assert_eq!(voice.gender, Gender::Female);
}

#[test]
fn test_word_boundary_estimation() {
    use rust_tts_wrapper::engine::estimate_word_boundaries;
    let boundaries = estimate_word_boundaries("Hello world this is a test");
    assert_eq!(boundaries.len(), 6);
    assert_eq!(boundaries[0].text, "Hello");
    assert_eq!(boundaries[0].offset, 0);
    assert!(boundaries[0].duration > 0);
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

#[test]
fn test_speak_options_defaults() {
    use rust_tts_wrapper::types::SpeakOptions;
    let opts = SpeakOptions::default();
    assert_eq!(opts.effective_rate(), 1.0);
    assert_eq!(opts.effective_pitch(), 1.0);
    assert_eq!(opts.effective_volume(), 1.0);
    assert!(!opts.use_speech_markdown);
    assert!(!opts.use_word_boundary);
    assert!(!opts.raw_ssml);
}

#[test]
fn test_speak_options_with_presets() {
    use rust_tts_wrapper::types::{SpeakOptions, SpeechPitch, SpeechRate};
    let opts = SpeakOptions {
        speech_rate: Some(SpeechRate::Fast),
        speech_pitch: Some(SpeechPitch::Low),
        volume: Some(0.8),
        ..Default::default()
    };
    assert_eq!(opts.effective_rate(), 1.25);
    assert_eq!(opts.effective_pitch(), 0.75);
    assert_eq!(opts.effective_volume(), 0.8);
}

#[test]
fn test_speak_options_float_overrides_preset() {
    use rust_tts_wrapper::types::{SpeakOptions, SpeechRate};
    let opts = SpeakOptions {
        rate: Some(1.5),
        speech_rate: Some(SpeechRate::Slow),
        ..Default::default()
    };
    assert_eq!(opts.effective_rate(), 1.5);
}

#[test]
fn test_audio_format_display() {
    use rust_tts_wrapper::types::AudioFormat;
    assert_eq!(AudioFormat::Mp3.to_string(), "mp3");
    assert_eq!(AudioFormat::Wav.to_string(), "wav");
    assert_eq!(AudioFormat::Pcm.to_string(), "pcm");
}

#[test]
fn test_speech_rate_values() {
    use rust_tts_wrapper::types::SpeechRate;
    assert_eq!(SpeechRate::XSlow.rate_value(), 0.5);
    assert_eq!(SpeechRate::Medium.rate_value(), 1.0);
    assert_eq!(SpeechRate::XFast.rate_value(), 1.5);
}

#[test]
fn test_speech_pitch_values() {
    use rust_tts_wrapper::types::SpeechPitch;
    assert_eq!(SpeechPitch::XLow.pitch_value(), 0.5);
    assert_eq!(SpeechPitch::Medium.pitch_value(), 1.0);
    assert_eq!(SpeechPitch::XHigh.pitch_value(), 1.5);
}

#[test]
fn test_gender_enum() {
    use rust_tts_wrapper::types::Gender;
    assert_eq!(Gender::Male.to_string(), "Male");
    assert_eq!(Gender::Female.to_string(), "Female");
    assert_eq!(Gender::Unknown.to_string(), "Unknown");
}

#[test]
fn test_word_boundary_with_wpm() {
    use rust_tts_wrapper::engine::estimate_word_boundaries_with_wpm;
    let fast = estimate_word_boundaries_with_wpm("Hello world", 300.0);
    let slow = estimate_word_boundaries_with_wpm("Hello world", 75.0);
    assert!(fast[0].duration < slow[0].duration);
}
