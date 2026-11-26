#![cfg(feature = "qwen3-local")]
// The clone-roundtrip test additionally needs the cloning feature; the
// file as a whole only needs the engine (an --all-targets build without
// `cloning` must still compile).

//! Live local Qwen3-TTS tests — no network, but need the built C++
//! library and converted GGUF models. `#[ignore]`-d by default so
//! feature builds without models still test-compile. Run with:
//!
//! ```text
//! QWEN3_TTS_LIB=/path/to/qwen3-tts.cpp \
//! QWEN3_TTS_MODELS=/path/to/qwen3-tts.cpp/models \
//! QWEN3_TTS_REF=/path/to/reference.wav \
//!     cargo test --features qwen3-local,cloning --test qwen3_local_live -- --ignored --nocapture
//! ```
//!
//! `qwen3_local_clone_streams` also needs a build against a C++ library
//! with the streaming entry points for clones: the build sets
//! `qwen3_clone_streaming` when the library's header has them, and
//! without that the test is not compiled.

#[cfg(feature = "cloning")]
use rust_tts_wrapper::cloning::{create_cloner, AudioClip, CloneOutcome, VoiceIdentityBuilder};
use rust_tts_wrapper::factory::create_engine;
#[cfg(qwen3_clone_streaming)]
use rust_tts_wrapper::TtsEngine;
use std::sync::{Arc, Mutex};
#[cfg(qwen3_clone_streaming)]
use std::time::Instant;

fn creds() -> Option<String> {
    let models = std::env::var("QWEN3_TTS_MODELS").ok()?;
    Some(
        serde_json::json!({
            "modelsDir": models,
            "threads": std::env::var("QWEN3_TTS_THREADS").unwrap_or_else(|_| "6".into()),
        })
        .to_string(),
    )
}

#[test]
#[ignore = "needs QWEN3_TTS_LIB-linked build + QWEN3_TTS_MODELS (GGUFs on disk)"]
fn qwen3_local_synthesizes_and_estimates_boundaries() {
    let Some(creds) = creds() else {
        eprintln!("QWEN3_TTS_MODELS not set; skipping");
        return;
    };
    let engine = create_engine("qwen3-local", &creds).expect("engine");
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let words: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (a, w) = (Arc::clone(&audio), Arc::clone(&words));
    engine
        .speak(
            "Hello from the local Qwen three engine.",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut move |b: &[u8]| a.lock().unwrap().extend_from_slice(b)),
            Some(&mut move |word: &str, _s, _e, _o, _l, est| {
                w.lock()
                    .unwrap()
                    .push(format!("{word}{}", if est { "*" } else { "" }));
            }),
            None,
        )
        .expect("speak");
    let pcm = audio.lock().unwrap().clone();
    assert!(pcm.len() > 48_000, "expected >1 s of audio");
    assert_eq!(pcm.len() % 2, 0);
    eprintln!(
        "{} PCM bytes; boundaries: {:?}",
        pcm.len(),
        words.lock().unwrap()
    );
}

#[test]
#[cfg(feature = "cloning")]
#[ignore = "needs QWEN3_TTS_LIB + QWEN3_TTS_MODELS + QWEN3_TTS_REF (reference wav)"]
fn qwen3_local_zero_shot_clone_roundtrip() {
    let Some(creds) = creds() else {
        eprintln!("QWEN3_TTS_MODELS not set; skipping");
        return;
    };
    let reference = match std::env::var("QWEN3_TTS_REF") {
        Ok(r) if !r.is_empty() => r,
        _ => {
            eprintln!("QWEN3_TTS_REF not set; skipping");
            return;
        }
    };
    // Cloner: identity → embedding handle.
    let cloner = create_cloner("qwen3-local", &creds).expect("cloner");
    let identity = VoiceIdentityBuilder::new("banked")
        .add_clip(AudioClip::from_audio_file(&reference).expect("reference clip"))
        .finish()
        .expect("identity");
    let handle = match cloner.clone_voice(&identity).expect("clone") {
        CloneOutcome::Ready(h) => h,
        other @ CloneOutcome::Pending { .. } => panic!("expected Ready, got {other:?}"),
    };
    assert!(
        handle.voice_id.starts_with("emb:"),
        "{}",
        &handle.voice_id[..8.min(handle.voice_id.len())]
    );

    // Engine speaks with the embedding handle.
    let engine = create_engine("qwen3-local", &creds).expect("engine");
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&audio);
    engine
        .speak(
            "This is my cloned voice, running fully offline.",
            Some(&handle.voice_id),
            1.0,
            1.0,
            1.0,
            Some(&mut move |b: &[u8]| sink.lock().unwrap().extend_from_slice(b)),
            None,
            None,
        )
        .expect("speak with embedding");
    let pcm = audio.lock().unwrap().len();
    assert!(pcm > 48_000, "expected >1 s of cloned audio, got {pcm}");
}

#[test]
#[ignore = "needs QWEN3_TTS_LIB + QWEN3_TTS_MODELS"]
fn qwen3_local_language_voice_ssml_and_volume() {
    let Some(creds) = creds() else {
        eprintln!("QWEN3_TTS_MODELS not set; skipping");
        return;
    };
    let engine = create_engine("qwen3-local", &creds).expect("engine");
    assert!(
        engine.check_credentials().unwrap(),
        "models should be loaded"
    );
    let voices = engine.get_voices().expect("voices");
    assert_eq!(voices.len(), 11, "default + 10 languages");
    assert!(voices
        .iter()
        .any(|v| v.id == "fr" && v.language_codes[0].bcp47 == "fr"));

    // Language voice + SSML-in (tags must be stripped, not spoken) +
    // volume gain path.
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&audio);
    engine
        .speak(
            "<speak>Bonjour, ceci est un test <break time=\"200ms\"/> en français.</speak>",
            Some("fr"),
            1.0,
            1.0,
            0.5,
            Some(&mut move |b: &[u8]| sink.lock().unwrap().extend_from_slice(b)),
            None,
            None,
        )
        .expect("speak fr");
    let pcm = audio.lock().unwrap().len();
    assert!(pcm > 16_000, "expected some audio, got {pcm}");

    // PLAIN-TEXT language voice (the loop-review regression pin): a
    // refactor once dropped the language_id override from speak()'s
    // plain path, so fr/de/etc. silently synthesized English — the
    // SSML case above masked it because the segments path kept its
    // own override. This case exercises the plain path; the assert
    // is existence-only (audio produced), but the voice's reach is
    // verifiable with an LD_PRELOAD interposer logging language_id
    // at the FFI boundary (fr → 2061, not 2050).
    let audio2: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink2 = Arc::clone(&audio2);
    engine
        .speak(
            "Bonjour, ceci est un test en français.",
            Some("fr"),
            1.0,
            1.0,
            1.0,
            Some(&mut move |b: &[u8]| sink2.lock().unwrap().extend_from_slice(b)),
            None,
            None,
        )
        .expect("speak fr (plain text)");
    let pcm2 = audio2.lock().unwrap().len();
    assert!(pcm2 > 16_000, "expected some audio, got {pcm2}");
}

/// Credentials for the streaming test: greedy decoding, so that it repeats
/// as closely as the engine allows, and a cap of 160 audio tokens (13 s).
/// With a normal sentence every clone here has ended on its own. One with
/// a run of seventeen spaces in it did not: it ran to whatever cap was
/// set, even under greedy decoding. So the test is bounded by a cap, and
/// the cap is well above what a real utterance needs.
#[cfg(qwen3_clone_streaming)]
fn streaming_creds() -> Option<String> {
    let models = std::env::var("QWEN3_TTS_MODELS").ok()?;
    Some(
        serde_json::json!({
            "modelsDir": models,
            "threads": std::env::var("QWEN3_TTS_THREADS").unwrap_or_else(|_| "4".into()),
            "temperature": "0",
            "maxTokens": "160",
        })
        .to_string(),
    )
}

/// What one `speak` delivered, and when.
#[cfg(qwen3_clone_streaming)]
struct Timed {
    bytes: usize,
    deliveries: usize,
    first_ms: f64,
    last_ms: f64,
}

/// `speak` with a callback that notes when each delivery arrives.
#[cfg(qwen3_clone_streaming)]
fn speak_timed(engine: &Arc<dyn TtsEngine>, text: &str, voice: Option<&str>) -> Timed {
    let started = Instant::now();
    let mut timed = Timed {
        bytes: 0,
        deliveries: 0,
        first_ms: 0.0,
        last_ms: 0.0,
    };
    engine
        .speak(
            text,
            voice,
            1.0,
            1.0,
            1.0,
            Some(&mut |b: &[u8]| {
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                if timed.deliveries == 0 {
                    timed.first_ms = ms;
                }
                timed.last_ms = ms;
                timed.deliveries += 1;
                timed.bytes += b.len();
            }),
            None,
            None,
        )
        .expect("speak");
    timed
}

/// Audio came in more than one delivery, and the first came well before
/// the last. A clone made whole is handed over in a run of deliveries
/// that takes microseconds; a streamed one over most of the time the
/// model spends generating.
#[cfg(qwen3_clone_streaming)]
#[allow(clippy::cast_precision_loss)] // a byte count as seconds, for the log line
fn assert_streams(what: &str, timed: &Timed) {
    eprintln!(
        "{what}: {:.2} s of audio in {} deliveries; first at {:.0} ms, last at {:.0} ms",
        timed.bytes as f64 / 48_000.0,
        timed.deliveries,
        timed.first_ms,
        timed.last_ms
    );
    assert!(
        timed.bytes > 48_000,
        "{what}: expected >1 s of audio, got {} bytes",
        timed.bytes
    );
    assert!(
        timed.deliveries >= 2,
        "{what}: expected audio in more than one delivery, got {}",
        timed.deliveries
    );
    assert!(
        timed.last_ms > 0.0 && timed.first_ms < 0.8 * timed.last_ms,
        "{what}: the first delivery came at {:.0} ms of {:.0} ms, which is a whole clone handed          over at the end, not a stream",
        timed.first_ms, timed.last_ms
    );
}

/// A clone streams as the default voice does: audio reaches the callback
/// while the model is still generating.
#[test]
#[cfg(qwen3_clone_streaming)]
#[allow(clippy::cast_precision_loss)] // byte counts as seconds, for the log line
#[ignore = "needs QWEN3_TTS_LIB with the clone-streaming C API + QWEN3_TTS_MODELS + QWEN3_TTS_REF"]
fn qwen3_local_clone_streams() {
    let Some(creds) = streaming_creds() else {
        eprintln!("QWEN3_TTS_MODELS not set; skipping");
        return;
    };
    let reference = match std::env::var("QWEN3_TTS_REF") {
        Ok(r) if !r.is_empty() => r,
        _ => {
            eprintln!("QWEN3_TTS_REF not set; skipping");
            return;
        }
    };
    let engine = create_engine("qwen3-local", &creds).expect("engine");
    let text = "Streaming keeps the cloned voice, and the listener hears the first words long before the whole sentence has been made.";

    let from_file = speak_timed(&engine, text, Some(&reference));
    assert_streams("a clone from a reference recording", &from_file);

    // `stop()` from inside the first delivery ends the synthesis at the
    // next delivery, and what arrived is kept.
    let mut deliveries = 0usize;
    let kept = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&kept);
    let stopper = Arc::clone(&engine);
    let started = Instant::now();
    engine
        .speak(
            text,
            Some(&reference),
            1.0,
            1.0,
            1.0,
            Some(&mut |b: &[u8]| {
                *sink.lock().unwrap() += b.len();
                deliveries += 1;
                if deliveries == 1 {
                    stopper.stop().expect("stop");
                }
            }),
            None,
            None,
        )
        .expect("speak with a stop");
    let stopped_ms = started.elapsed().as_secs_f64() * 1000.0;
    let kept = *kept.lock().unwrap();
    eprintln!(
        "stopped in the first delivery of a clone: {:.2} s kept, after {:.0} ms",
        kept as f64 / 48_000.0,
        stopped_ms
    );
    assert!(kept > 0, "a stopped clone keeps what arrived");
    assert!(
        kept < from_file.bytes && stopped_ms < from_file.last_ms,
        "stopping ends a clone early: {kept} bytes in {stopped_ms:.0} ms, against {} bytes in {:.0} ms",
        from_file.bytes, from_file.last_ms
    );

    // The same clone as one buffer is made in one go, and is the same
    // speech: with greedy decoding, stream and batch differ only in how
    // the vocoder decodes.
    let started = Instant::now();
    let whole = engine
        .synth_to_bytes(text, Some(&reference), 1.0, 1.0, 1.0)
        .expect("synth_to_bytes with a clone");
    let whole_ms = started.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "the same clone as one buffer: {:.2} s of audio in {:.0} ms",
        whole.len() as f64 / 48_000.0,
        whole_ms
    );
    assert_eq!(
        whole.len(),
        from_file.bytes,
        "the clone as one buffer is the same speech as the clone streamed"
    );

    // The same voice as the cloner's cached embedding.
    #[cfg(feature = "cloning")]
    {
        let cloner = create_cloner("qwen3-local", &creds).expect("cloner");
        let identity = VoiceIdentityBuilder::new("banked")
            .add_clip(AudioClip::from_audio_file(&reference).expect("reference clip"))
            .finish()
            .expect("identity");
        let handle = match cloner.clone_voice(&identity).expect("clone") {
            CloneOutcome::Ready(h) => h,
            other @ CloneOutcome::Pending { .. } => panic!("expected Ready, got {other:?}"),
        };
        let from_embedding = speak_timed(&engine, text, Some(&handle.voice_id));
        assert_streams("a clone from a cached embedding", &from_embedding);

        // The same clone as one buffer is the same speech, streamed or not.
        let whole = engine
            .synth_to_bytes(text, Some(&handle.voice_id), 1.0, 1.0, 1.0)
            .expect("synth_to_bytes with an embedding");
        eprintln!(
            "the same clone from an embedding as one buffer: {:.2} s of audio",
            whole.len() as f64 / 48_000.0
        );
        assert_eq!(
            whole.len(),
            from_embedding.bytes,
            "the clone from an embedding as one buffer is the same speech as streamed"
        );
    }
}
