#![cfg(feature = "cloud")]
//! Live probes for issue #149: does the existing cloud `qwen` engine
//! (the shared `tts_v2` WS protocol) actually serve the Qwen-Audio-TTS
//! 3.1 models and the `CosyVoice` models with only a `modelId`
//! credential change? `#[ignore]`-d by default — real network, real
//! key. Run with:
//!
//! ```text
//! QWEN_API_KEY=sk-... \
//!     cargo test --features cloud --test qwen_model_probe -- --ignored --nocapture
//! ```
//!
//! Each probe synthesizes a short phrase and reports the PCM bytes
//! received (24 kHz mono PCM16 per the engine's wire format). The
//! audio is also written under /tmp/opencode for listening evidence.

use rust_tts_wrapper::factory::create_engine;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn creds(model_id: &str) -> Option<String> {
    let key = std::env::var("QWEN_API_KEY").ok()?;
    let region = std::env::var("QWEN_REGION").unwrap_or_default();
    let mut v = serde_json::json!({ "apiKey": key, "modelId": model_id });
    if !region.is_empty() {
        v["region"] = serde_json::Value::String(region);
    }
    Some(v.to_string())
}

/// One probe: synthesize `text` with `model_id` + `voice`, return the
/// PCM byte count. Errors carry the engine's message (the WS
/// task-failed detail — that's the data we want).
fn probe(model_id: &str, voice: &str, text: &str) -> Result<usize, String> {
    let creds = creds(model_id).ok_or_else(|| "QWEN_API_KEY not set".to_string())?;
    let engine = create_engine("qwen", &creds).map_err(|e| e.to_string())?;
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let a = Arc::clone(&audio);
    let t0 = Instant::now();
    engine
        .speak(
            text,
            Some(voice),
            1.0,
            1.0,
            1.0,
            Some(&mut move |pcm: &[u8]| {
                a.lock().map(|mut b| b.extend_from_slice(pcm)).ok();
            }),
            None,
            None,
        )
        .map_err(|e| format!("speak failed: {e}"))?;
    let bytes = audio.lock().map_or(0, |b| b.len());
    eprintln!(
        "  [{model_id} / {voice}] {bytes} bytes PCM in {:.1}s",
        t0.elapsed().as_secs_f32()
    );
    // Listening evidence: 24 kHz mono PCM16LE -> WAV.
    if bytes > 0 {
        let name = format!("/tmp/opencode/probe-{}.wav", voice.replace(['/', '.'], "_"));
        let _ = write_wav(&name, &audio.lock().unwrap());
        eprintln!("    wrote {name}");
    }
    Ok(bytes)
}

fn write_wav(path: &str, pcm16: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    #[allow(clippy::cast_possible_truncation)]
    let data_len = pcm16.len() as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36u32.saturating_add(data_len)).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&24_000u32.to_le_bytes())?;
    f.write_all(&48_000u32.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    f.write_all(pcm16)?;
    Ok(())
}

/// The #149 probe matrix. Each combo is independent: a failure in one
/// is reported, not fatal to the others (the errors are the data).
#[test]
#[ignore = "needs QWEN_API_KEY (real network, billed synthesis)"]
fn qwen_model_probe_matrix() {
    let probes: Vec<(&str, &str, &str)> = vec![
        // Control: the engine's actual default pair (the shipped
        // QWEN_DEFAULT_MODEL + QWEN_DEFAULT_VOICE).
        (
            "qwen-audio-3.0-tts-flash",
            "longanhuan_v3.6",
            "Hello, this is the control probe.",
        ),
        // 3.1 flagship system voice (multilingual family).
        (
            "qwen-audio-3.1-tts-flash",
            "longanlingxin_v3.1",
            "Hello, this is a three point one flash probe.",
        ),
        // 3.1 minority-language SYSTEM voice (Bulgarian) — the class
        // our offline fleet cannot cover.
        (
            "qwen-audio-3.1-tts-flash",
            "qwen-audio-3.1-tts-flash-zora_bg",
            "Здравей, това е проба.",
        ),
        // CosyVoice v3 flash, benchmark voice (v3 voice names carry a
        // _v3 suffix or are bare long* names — v2 names do NOT work).
        (
            "cosyvoice-v3-flash",
            "longanyang",
            "Hello, this is a CosyVoice probe.",
        ),
        // CosyVoice v2 with its own v2 voice name.
        (
            "cosyvoice-v2",
            "longwan",
            "Hello, this is a CosyVoice two probe.",
        ),
    ];
    let mut results: Vec<(String, Result<usize, String>)> = Vec::new();
    for (model, voice, text) in probes {
        eprintln!("PROBE {model} / {voice}");
        results.push((format!("{model}/{voice}"), probe(model, voice, text)));
    }
    eprintln!("\n== RESULTS ==");
    for (label, r) in &results {
        match r {
            Ok(n) if *n > 4_800 => eprintln!("PASS  {label}: {n} bytes"),
            Ok(n) => eprintln!("EMPTY {label}: {n} bytes"),
            Err(e) => eprintln!("ERROR {label}: {e}"),
        }
    }
    // The control must pass for the probe run to mean anything.
    let control = &results[0];
    assert!(
        matches!(control.1, Ok(n) if n > 4_800),
        "control probe failed: {:?} — the endpoint/key is broken, other results are moot",
        control.1
    );
}

/// GH #149: with NO `modelId` credential, a 3.1-style voice id infers
/// its model (`qwen_voice_inferred_model`) — any listed voice just works.
#[test]
#[ignore = "needs QWEN_API_KEY (real network, billed synthesis)"]
fn qwen_voice_infers_model_without_model_id() {
    let Ok(key) = std::env::var("QWEN_API_KEY") else {
        eprintln!("QWEN_API_KEY not set; skipping");
        return;
    };
    // Deliberately NO modelId: the minority-language voice id must
    // infer qwen-audio-3.1-tts-flash.
    let creds = serde_json::json!({ "apiKey": key }).to_string();
    let engine = create_engine("qwen", &creds).expect("engine");
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let a = Arc::clone(&audio);
    engine
        .speak(
            "Здравей от пробата.",
            Some("qwen-audio-3.1-tts-flash-zora_bg"),
            1.0,
            1.0,
            1.0,
            Some(&mut move |pcm: &[u8]| {
                a.lock().map(|mut b| b.extend_from_slice(pcm)).ok();
            }),
            None,
            None,
        )
        .expect("voice-inferred synthesis");
    let bytes = audio.lock().map_or(0, |b| b.len());
    eprintln!("inference probe (minority voice): {bytes} bytes");
    assert!(bytes > 4_800, "expected audio, got {bytes} bytes");

    // The 3.0-plus-only flagship name: previously 411 against the
    // flash default (probed before the fix); now inferred to plus.
    let engine2 = create_engine("qwen", &creds).expect("engine");
    let audio2: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let a2 = Arc::clone(&audio2);
    engine2
        .speak(
            "Hello, the flagship voice works without a model id.",
            Some("longanlingxin"),
            1.0,
            1.0,
            1.0,
            Some(&mut move |pcm: &[u8]| {
                a2.lock().map(|mut b| b.extend_from_slice(pcm)).ok();
            }),
            None,
            None,
        )
        .expect("plus-inferred synthesis");
    let bytes2 = audio2.lock().map_or(0, |b| b.len());
    eprintln!("inference probe (3.0-plus flagship): {bytes2} bytes");
    assert!(bytes2 > 4_800, "expected audio, got {bytes2} bytes");
}

/// GH #149: `stop()` mid-synthesis cancels the task (the documented
/// `finish-task` + `directive=cancel`) and speak returns Ok with the
/// partial audio already delivered.
#[test]
#[ignore = "needs QWEN_API_KEY (real network, billed synthesis)"]
fn qwen_stop_mid_synthesis_truncates() {
    use std::sync::atomic::AtomicBool;
    let Ok(key) = std::env::var("QWEN_API_KEY") else {
        eprintln!("QWEN_API_KEY not set; skipping");
        return;
    };
    let creds =
        serde_json::json!({ "apiKey": key, "modelId": "qwen-audio-3.1-tts-flash" }).to_string();
    let engine = create_engine("qwen", &creds).expect("engine");
    let stopper = std::sync::Arc::clone(&engine);
    let audio: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let got_first = Arc::new(AtomicBool::new(false));
    let a = Arc::clone(&audio);
    let g = Arc::clone(&got_first);
    let text = "This is a longer passage of text, with several sentences, \
                so that synthesis takes a while and the stop request lands \
                somewhere in the middle rather than after completion. \
                The quick brown fox jumps over the lazy dog again and again, \
                and the engine should deliver some audio before the stop \
                cancels the remaining sentences of this utterance.";
    let handle = std::thread::spawn(move || {
        engine.speak(
            text,
            Some("longanlingxin_v3.1"),
            1.0,
            1.0,
            1.0,
            Some(&mut move |pcm: &[u8]| {
                if !pcm.is_empty() {
                    g.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                a.lock().map(|mut b| b.extend_from_slice(pcm)).ok();
            }),
            None,
            None,
        )
    });
    // Wait (up to 30 s) for the first audio bytes, then stop.
    let mut saw_audio = false;
    for _ in 0..300 {
        if got_first.load(std::sync::atomic::Ordering::Relaxed) {
            saw_audio = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(saw_audio, "no audio arrived to stop mid-stream");
    stopper.stop().expect("stop");
    let result = handle.join().expect("speak thread");
    let bytes = audio.lock().map_or(0, |b| b.len());
    eprintln!("stop probe: {bytes} bytes after mid-stream stop");
    assert!(result.is_ok(), "stopped speak is Ok: {result:?}");
    assert!(bytes > 0, "partial audio stands after stop");
}
