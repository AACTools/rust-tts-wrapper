// Streaming proof through the wrapper's speak(): time-to-first-audio vs
// total with a real-time-paced consumer (sleeps playback-length between
// chunks), plus boundary-callback timing relative to audio.
#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::floravox_engine::FloravoxEngine;
use rust_tts_wrapper::TtsEngine;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::var("MODEL_DIR").unwrap_or_else(|_| "/tmp/opencode/vg-piper-de".into());
    let model = std::env::var("MODEL").unwrap_or_else(|_| "thorsten".into());
    let creds = std::env::var("CREDS").unwrap_or_else(|_| {
        format!("{{\"modelsDir\":\"{dir}\",\"modelId\":\"{model}\",\"lang\":\"de\"}}")
    });
    let text = std::env::var("TEXT").unwrap_or_else(|_| {
        "Der erste Satz beginnt sofort. Der zweite Satz folgt live. Der dritte Satz laeuft bereits im Hintergrund. Ein vierter beendet den Test."
            .into()
    });
    let engine = FloravoxEngine::new(&creds);

    let t0 = Instant::now();
    let first_audio = Arc::new(AtomicU64::new(0));
    let boundaries_at_first: Arc<AtomicU64> = Arc::new(AtomicU64::new(u64::MAX));
    let n_boundaries = Arc::new(AtomicU64::new(0));
    let total_ms_of_audio = Arc::new(AtomicU64::new(0));

    let fa = Arc::clone(&first_audio);
    let bf = Arc::clone(&boundaries_at_first);
    let nb = Arc::clone(&n_boundaries);
    let ta = Arc::clone(&total_ms_of_audio);

    eprintln!("[demo] engine constructed, calling speak...");
    engine.speak(
        &text,
        None,
        1.0,
        1.0,
        1.0,
        Some(&mut |chunk: &[u8]| {
            if fa.load(Ordering::SeqCst) == 0 {
                fa.store(t0.elapsed().as_millis() as u64, Ordering::SeqCst);
                bf.store(nb.load(Ordering::SeqCst), Ordering::SeqCst);
            }
            ta.fetch_add(chunk.len() as u64 / 2 / 16, Ordering::SeqCst); // samples -> ms @16 kHz
                                                                         // real-time paced consumer
            std::thread::sleep(std::time::Duration::from_millis(
                chunk.len() as u64 / 2 / 16,
            ));
        }),
        Some(&mut |_w, _s, _e, _o, _l, _est| {
            nb.fetch_add(1, Ordering::SeqCst);
        }),
        None,
    )?;
    let total = t0.elapsed().as_millis() as u64;
    let fa = first_audio.load(Ordering::SeqCst);
    println!("first audio after   {fa} ms");
    println!(
        "boundaries fired before first audio: {}",
        boundaries_at_first.load(Ordering::SeqCst)
    );
    println!(
        "total (realtime-paced): {total} ms, audio {} ms, boundaries {}",
        total_ms_of_audio.load(Ordering::SeqCst),
        n_boundaries.load(Ordering::SeqCst)
    );
    println!(
        "synthesis finished {:.0}% ahead of playback",
        100.0 - 100.0 * fa as f64 / total.max(1) as f64
    );
    Ok(())
}
