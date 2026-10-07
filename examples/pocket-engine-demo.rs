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
    let mut pcm_all: Vec<u8> = Vec::new();
    let mut on_audio = |pcm: &[u8]| {
        audio_bytes += pcm.len();
        pcm_all.extend_from_slice(pcm);
    };
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
    if let Some(out) = std::env::args().nth(4) {
        // pcm is 16-bit LE mono 24 kHz
        let mut wav: Vec<u8> = Vec::with_capacity(44 + pcm_all.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(
            &(36 + u32::try_from(pcm_all.len()).unwrap_or(u32::MAX)).to_le_bytes(),
        );
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&24_000u32.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(
            &u32::try_from(pcm_all.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        wav.extend_from_slice(&pcm_all);
        std::fs::write(&out, wav).expect("write wav");
        println!("wrote {out}");
    }
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
