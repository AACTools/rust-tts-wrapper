//! `PocketTtsEngine` through the `TtsEngine` trait: cloned voice + REAL
//! attention-measured word boundaries via `on_boundary`.
//!   cargo run --release --no-default-features --features pocket-timing //!     --example pocket-engine-demo -- `<bundle_dir>` `<ref_wav>` `[text]`
#![allow(clippy::cast_precision_loss)]
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
    let mut marks: Vec<String> = Vec::new();
    let mut on_mark = |name: &str, t: f32, _e: f32, _pos: i32| {
        marks.push(format!("mark {name} @ {t:.2}s"));
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
            Some(&mut on_mark),
        )
        .expect("speak");
    println!(
        "spoke in {:.1}s: {} PCM bytes ({:.2}s audio), {} boundaries, {} marks:",
        t0.elapsed().as_secs_f32(),
        audio_bytes,
        audio_bytes as f32 / 48000.0,
        boundaries.len(),
        marks.len()
    );
    for b in &boundaries {
        println!("  {b}");
    }
    for m in &marks {
        println!("  {m}");
    }
}

#[cfg(not(feature = "pocket-timing"))]
fn main() {
    eprintln!("requires --features pocket-timing");
}
