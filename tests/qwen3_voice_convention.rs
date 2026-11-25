//! qwen3-local voice-convention parsing + stream conversion tests
//! (pure — no C++ library needed).

#![cfg(feature = "qwen3-local")]

use rust_tts_wrapper::qwen3_local_engine::parse_voice_spec;
use rust_tts_wrapper::qwen3_local_engine::VoiceSpec;

#[test]
fn voice_spec_plain_forms() {
    assert_eq!(parse_voice_spec(None).unwrap(), VoiceSpec::Plain);
    assert_eq!(parse_voice_spec(Some("")).unwrap(), VoiceSpec::Plain);
    assert_eq!(parse_voice_spec(Some("default")).unwrap(), VoiceSpec::Plain);
    // language codes are Plain (they set language_id separately)
    assert_eq!(parse_voice_spec(Some("en")).unwrap(), VoiceSpec::Plain);
    assert_eq!(parse_voice_spec(Some("de")).unwrap(), VoiceSpec::Plain);
}

#[test]
fn voice_spec_reference_file() {
    assert_eq!(
        parse_voice_spec(Some("/refs/voice.wav")).unwrap(),
        VoiceSpec::ReferenceFile("/refs/voice.wav".into())
    );
    // a path that LOOKS like a language code is a path when it has
    // a suffix... "en-GB" IS a language; a file path with dirs wins
    assert_eq!(
        parse_voice_spec(Some("./en.wav")).unwrap(),
        VoiceSpec::ReferenceFile("./en.wav".into())
    );
}

#[test]
fn voice_spec_icl() {
    let spec = parse_voice_spec(Some("icl:/refs/me.wav|Hello, I am Will.")).unwrap();
    assert_eq!(
        spec,
        VoiceSpec::InContext {
            path: "/refs/me.wav".into(),
            transcript: "Hello, I am Will.".into()
        }
    );
    // transcript may contain pipes: split on the FIRST
    let spec = parse_voice_spec(Some("icl:/r.wav|one|two|three")).unwrap();
    match spec {
        VoiceSpec::InContext { transcript, .. } => assert_eq!(transcript, "one|two|three"),
        other => panic!("expected ICL: {other:?}"),
    }
}

#[test]
fn voice_spec_icl_requires_both_parts() {
    // missing transcript
    assert!(parse_voice_spec(Some("icl:/refs/me.wav")).is_err());
    // empty transcript
    assert!(parse_voice_spec(Some("icl:/refs/me.wav|")).is_err());
    // empty path
    assert!(parse_voice_spec(Some("icl:|hello")).is_err());
}

#[test]
fn chunk_conversion_with_gain() {
    use rust_tts_wrapper::qwen3_stream::chunk_to_pcm16;
    let pcm = chunk_to_pcm16(&[0.0, 0.5, -0.5, 1.0], 1.0);
    assert_eq!(pcm, vec![0, 16_383, -16_383, 32_767]);
    // gain 2.0 clamps the extremes
    let pcm = chunk_to_pcm16(&[0.5, -0.5], 2.0);
    assert_eq!(pcm, vec![32_767, -32_767]);
    // gain 0 silences
    let pcm = chunk_to_pcm16(&[0.9], 0.0);
    assert_eq!(pcm, vec![0]);
}

/// The stream trampoline's conversion + accounting, driven directly
/// (the C++ call site uses it via the same function pointer).
#[test]
fn stream_trampoline_delivers_and_counts() {
    use rust_tts_wrapper::qwen3_stream::{stream_trampoline, StreamState};
    use std::sync::Arc;
    let deliveries = std::sync::Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    {
        let sink_hits = Arc::clone(&deliveries);
        let mut sink = move |b: &[u8]| sink_hits.lock().unwrap().push(b.len());
        let mut state = StreamState::new(&mut sink, 1.0);
        let chunk = [0.25_f32, -0.25, 0.5];
        let ud = std::ptr::addr_of_mut!(state).cast::<std::ffi::c_void>();
        let rc = stream_trampoline(chunk.as_ptr(), i32::try_from(chunk.len()).unwrap(), ud);
        assert_eq!(rc, 1, "continue");
        assert_eq!(state.total_samples, 3);
        // zero-length chunk: continue, no delivery, no count
        let rc = stream_trampoline(std::ptr::null(), 0, ud);
        assert_eq!(rc, 1);
        assert_eq!(state.total_samples, 3, "unchanged by the empty chunk");
    }
    let d = deliveries.lock().unwrap();
    assert_eq!(d.len(), 1, "one delivery");
    assert_eq!(d[0], 6, "3 samples × 2 bytes, PCM16 LE");
}

/// Cancellation: `stop()` → the trampoline returns 0 (the C++ contract
/// for "stop generating").
#[test]
fn stream_trampoline_honours_cancel() {
    use rust_tts_wrapper::qwen3_stream::{stream_trampoline, StreamState};
    rust_tts_wrapper::qwen3_local_engine::stop_for_tests();
    let mut sink = |_b: &[u8]| {};
    let mut state = StreamState::new(&mut sink, 1.0);
    let chunk = [0.1_f32; 4];
    let ud = std::ptr::addr_of_mut!(state).cast::<std::ffi::c_void>();
    let rc = stream_trampoline(chunk.as_ptr(), i32::try_from(chunk.len()).unwrap(), ud);
    assert_eq!(rc, 0, "cancelled → stop generating");
    assert!(state.cancelled);
    assert_eq!(state.total_samples, 0, "no audio after cancel");
}

#[test]
fn params_struct_abi_pinned_at_40_bytes() {
    // The hand-declared layout must match upstream's
    // qwen3tts_c_api.h exactly (7 scalars + padding + ref_code_path
    // pointer on LP64). The compile-time const asserts it too; this
    // test makes a regression VISIBLE by name.
    assert_eq!(
        rust_tts_wrapper::qwen3_local_engine::params_size_probe(),
        40
    );
}
