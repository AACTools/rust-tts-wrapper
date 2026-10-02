//! Smoke: pocket-tts through the sherpaonnx engine, cloning from a bundled
//! reference wav. Run with:
//!   cargo run --release --features sherpaonnx --example pocket-smoke -- `<model_dir>` `[ref_wav]`

#[cfg(feature = "sherpaonnx")]
use rust_tts_wrapper::{engine::TtsEngine, sherpaonnx_engine::SherpaOnnxEngine};

#[cfg(feature = "sherpaonnx")]
fn main() {
    let dir = std::env::args().nth(1).expect("model dir");
    let reference = std::env::args().nth(2);
    let creds = serde_json::json!({
        "modelId": "kyutai-en-pocket-tts",
        "modelPath": dir,
        "referenceAudio": reference,
    })
    .to_string();
    let engine = SherpaOnnxEngine::new(&creds);
    let mut s: Vec<f32> = Vec::new();
    let mut on_audio = |chunk: &[u8]| {
        // 16-bit PCM LE frames
        let (pairs, _) = chunk.as_chunks::<2>();
        for pair in pairs {
            let v = i16::from_le_bytes(*pair) as f32 / 32767.0;
            s.push(v);
        }
    };
    engine
        .speak(
            "Where is the bathroom",
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut on_audio),
            None,
            None,
        )
        .expect("speak");
    println!("got {} samples", s.len());
    // write wav
    let mut wav = Vec::with_capacity(44 + s.len() * 2);
    wav.extend_from_slice(b"RIFF");
    let data_len = (s.len() * 2) as u32;
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&24000u32.to_le_bytes());
    wav.extend_from_slice(&48000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for x in s.iter() {
        let v = (x.clamp(-1.0, 1.0) * 32767.0) as i16;
        wav.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write("/tmp/opencode/wrapper-pocket-smoke.wav", wav).unwrap();
    println!("written /tmp/opencode/wrapper-pocket-smoke.wav");
}

#[cfg(not(feature = "sherpaonnx"))]
fn main() {
    eprintln!("this example requires --features sherpaonnx");
}
