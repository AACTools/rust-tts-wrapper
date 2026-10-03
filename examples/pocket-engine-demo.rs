#![allow(clippy::cast_precision_loss)]
//! `PocketTtsEngine` through the `TtsEngine` trait: cloned voice + REAL
//! attention-measured word boundaries via `on_boundary`.
//!   cargo run --release --no-default-features --features pocket-timing \
//!     --example pocket-engine-demo -- `<bundle_dir>` `<ref_wav>` `[text]`

#[cfg(feature = "pocket-timing")]
fn main() {
    use rust_tts_wrapper::engine::TtsEngine;
    use rust_tts_wrapper::pocket::PocketTtsEngine;

    let dir = std::env::args().nth(1).expect("bundle dir");
    let ref_wav = std::env::args().nth(2).expect("reference wav");
    let text = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "Where is the bathroom".into());

    let engine = PocketTtsEngine::new(std::path::Path::new(&dir), std::path::Path::new(&ref_wav));
    println!(
        "engine: {:?} | voices: {}",
        engine.engine_id(),
        engine.get_voices().unwrap().len()
    );

    let t0 = std::time::Instant::now();
    let mut boundaries: Vec<String> = Vec::new();
    let mut audio_bytes = 0usize;
    let mut on_audio = |pcm: &[u8]| audio_bytes += pcm.len();
    let mut on_boundary = |word: &str, s: f32, e: f32, pos: i32, len: i32, est: bool| {
        boundaries.push(format!(
            "{word}: {s:.2}-{e}s (char {pos}+{len}, estimated={est})"
        ));
    };
    engine
        .speak(
            &text,
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut on_audio),
            Some(&mut on_boundary),
            None,
        )
        .expect("speak");
    println!(
        "spoke in {:.1}s: {} PCM bytes ({:.2}s audio), {} boundaries:",
        t0.elapsed().as_secs_f32(),
        audio_bytes,
        audio_bytes as f32 / 2.0 / 24000.0,
        boundaries.len()
    );
    for b in &boundaries {
        println!("  {b}");
    }

    // synth_with_boundaries: the typed API
    let (pcm, words) = engine
        .synth_with_boundaries("Can you help me with this please", None, 1.0, 1.0, 1.0)
        .expect("synth_with_boundaries");
    println!(
        "synth_with_boundaries: {} bytes, {} words:",
        pcm.len(),
        words.len()
    );
    for w in &words {
        println!(
            "  {} @{}ms +{}ms est={}",
            w.text, w.offset, w.duration, w.estimated
        );
    }
}

#[cfg(not(feature = "pocket-timing"))]
fn main() {
    eprintln!("requires --features pocket-timing");
}
