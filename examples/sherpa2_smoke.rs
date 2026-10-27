//! Development smoke: synthesize with a sherpa model and report output.
//!
//! Writes the audio to `/tmp/smoke.wav` for listening checks.
//!
//! Usage: `cargo run --no-default-features --features sherpaonnx
//!        --example sherpa2_smoke -- <modelId> "text"`

use rust_tts_wrapper::engine::TtsEngine;
use rust_tts_wrapper::sherpaonnx_engine::SherpaOnnxEngine;

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let model_id = &args[1];
    let text = &args[2];
    let creds = format!(r#"{{"modelId": "{model_id}"}}"#);
    let engine = SherpaOnnxEngine::new(&creds);
    // (chunks, samples, peak, pcm bytes)
    let state = std::rc::Rc::new(std::cell::RefCell::new((
        0usize,
        0usize,
        0i16,
        Vec::<u8>::new(),
    )));
    let st = state.clone();
    let res = engine.speak(
        text,
        None,
        1.0,
        1.0,
        1.0,
        Some(&mut move |chunk: &[u8]| {
            let mut s = st.borrow_mut();
            s.0 += 1;
            s.1 += chunk.len() / 2;
            s.3.extend_from_slice(chunk);
            for b in chunk.chunks_exact(2) {
                let v = i16::from_le_bytes([b[0], b[1]]).abs();
                if v > s.2 {
                    s.2 = v;
                }
            }
        }),
        None,
        None,
    );
    let (chunks, samples, peak) = {
        let s = state.borrow();
        (s.0, s.1, s.2)
    };
    match res {
        Ok(()) => {
            // 16-bit mono PCM per the sherpa engine contract
            let data = state.borrow().3.clone();
            let sr: u32 = 24_000;
            let n = u32::try_from(data.len() / 2).unwrap_or(0);
            let mut wav: Vec<u8> = Vec::with_capacity(data.len() + 44);
            let size = u32::try_from(36 + data.len()).unwrap_or(u32::MAX);
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&size.to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16u32.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&sr.to_le_bytes());
            wav.extend_from_slice(&(sr * 2).to_le_bytes());
            wav.extend_from_slice(&2u16.to_le_bytes());
            wav.extend_from_slice(&16u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&n.to_le_bytes());
            wav.extend_from_slice(&data);
            std::fs::write("/tmp/smoke.wav", wav).ok();
            println!(
                "OK model={model_id} chunks={chunks} samples={samples} peak={peak} ({:.2}s)",
                samples as f64 / 24_000.0
            );
        }
        Err(e) => println!("FAIL model={model_id}: {}", e.0),
    }
}
