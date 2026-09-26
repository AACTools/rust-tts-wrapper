//! End-to-end voice banking demo: Personal Voice export (or LJSpeech
//! corpus) → identity → clone to every cloning-capable engine with
//! credentials in the environment → register handles → speak a line
//! through each.
//!
//! ```text
//! # Qwen (DashScope):
//! QWEN_API_KEY=sk-... cargo run --features cloning --example voice-clone -- \
//!     --zip "Will's Personal Voice 1 - Recordings.zip"
//! # Add ELEVENLABS_API_KEY to fan out to ElevenLabs as well.
//! ```
//!
//! Handles are recorded in `~/.rust-tts-wrapper/clones.json`; the demo
//! speaks through `create_engine` exactly like any host app would.

#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::cloning::{create_cloner, default_registry_path, CloneRegistry, VoiceCorpus};
use rust_tts_wrapper::factory::create_engine;
use std::sync::{Arc, Mutex};

const DEMO_TEXT: &str =
    "Hello! This is my banked voice, cloned and speaking through rust tts wrapper.";

fn main() {
    let mut args = std::env::args().skip(1);
    let mut zip: Option<String> = None;
    let mut corpus_dir: Option<String> = None;
    let mut text = DEMO_TEXT.to_string();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--zip" => zip = args.next(),
            "--ljspeech" => corpus_dir = args.next(),
            "--text" => text = args.next().unwrap_or_default(),
            other => eprintln!("unknown arg {other}"),
        }
    }

    let corpus = if let Some(zip) = &zip {
        VoiceCorpus::from_personal_voice_zip(zip).expect("import Personal Voice zip")
    } else if let Some(dir) = &corpus_dir {
        VoiceCorpus::from_ljspeech_dir(dir).expect("import LJSpeech corpus")
    } else {
        eprintln!("usage: voice-clone --zip <PersonalVoice.zip> | --ljspeech <dir> [--text ...]");
        std::process::exit(2);
    };
    println!(
        "imported '{}' — {} clips, {} s of audio",
        corpus.name,
        corpus.clips.len(),
        corpus.total_duration_secs()
    );
    let identity = corpus.to_identity(Some("en"));

    // Engines with credentials present in the environment.
    let engines: Vec<(&str, String)> = [
        ("qwen", std::env::var("QWEN_API_KEY").ok()),
        ("elevenlabs", std::env::var("ELEVENLABS_API_KEY").ok()),
    ]
    .into_iter()
    .filter_map(|(id, key)| key.map(|k| (id, k)))
    .collect();
    if engines.is_empty() {
        eprintln!("no cloning credentials (set QWEN_API_KEY and/or ELEVENLABS_API_KEY)");
        std::process::exit(2);
    }

    let mut registry = default_registry_path()
        .map(|p| CloneRegistry::load(&p).expect("load clone registry"))
        .unwrap_or_else(CloneRegistry::in_memory);

    let mut speaking: Vec<(String, rust_tts_wrapper::cloning::CloneHandle)> = Vec::new();
    for (engine_id, key) in &engines {
        let creds = serde_json::json!({ "apiKey": key }).to_string();
        let Some(cloner) = create_cloner(engine_id, &creds) else {
            eprintln!("{engine_id}: cloning not supported in this build");
            continue;
        };
        match cloner.clone_voice(&identity) {
            Ok(rust_tts_wrapper::cloning::CloneOutcome::Ready(handle)) => {
                println!("{engine_id}: cloned -> {}", handle.voice_id);
                registry.add(&identity.name, handle.clone());
                speaking.push((creds.clone(), handle));
            }
            Ok(pending) => eprintln!("{engine_id}: pending job {pending:?} (not supported yet)"),
            Err(e) => eprintln!("{engine_id}: clone failed: {e}"),
        }
    }
    registry.save().expect("save clone registry");
    println!(
        "registry saved ({} identities)",
        registry.identities().len()
    );

    for (creds, handle) in speaking {
        let wav_path = format!(
            "{}-{}.wav",
            handle.engine,
            handle.voice_id.chars().take(24).collect::<String>()
        );
        // Qwen voices are model-bound; pass the recorded model through
        // the engine's modelId credential.
        let creds = if let Some(model) = &handle.model {
            let mut v: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&creds).unwrap();
            v.insert("modelId".into(), model.clone().into());
            serde_json::Value::Object(v).to_string()
        } else {
            creds
        };
        let engine = create_engine(&handle.engine, &creds).expect("create engine");
        let pcm: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&pcm);
        engine
            .speak(
                &text,
                Some(&handle.voice_id),
                1.0,
                1.0,
                1.0,
                Some(&mut move |b: &[u8]| sink.lock().unwrap().extend_from_slice(b)),
                None,
                None,
            )
            .expect("speak");
        let pcm = pcm.lock().unwrap().clone();
        let wav = wav_wrap(&pcm, 24_000);
        std::fs::write(&wav_path, wav).expect("write wav");
        println!(
            "{}: {:.1}s -> {wav_path}",
            handle.engine,
            pcm.len() as f32 / 48_000.0
        );
    }
}

fn wav_wrap(pcm: &[u8], rate: u32) -> Vec<u8> {
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}
