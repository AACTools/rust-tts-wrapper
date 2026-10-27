// A/B: synthesize the same sentence with whatever lexicon.txt currently
// sits in the model dir; save a wav. Swap lexicons between runs to compare.
// usage: cargo run --features sherpaonnx --example kokoro-ab -- <model-dir-id> <out.wav> [sentence]
fn main() {
    use rust_tts_wrapper::create_engine;
    use std::sync::{Arc, Mutex};

    let model_id = std::env::args()
        .nth(1)
        .expect("model id, e.g. kokoro-int8-en-v0_19");
    let out_path = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "/tmp/kokoro_ab.wav".into());
    let sentence = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "Hello world, this is a test of the converted lexicon.".into());

    let creds = format!(r#"{{"modelId":"{model_id}"}}"#);
    let engine = create_engine("sherpaonnx", &creds).expect("engine");
    let audio = Arc::new(Mutex::new(Vec::<u8>::new()));
    let a2 = audio.clone();
    let mut cb = move |c: &[u8]| a2.lock().unwrap().extend_from_slice(c);
    engine
        .speak(&sentence, None, 1.0, 1.0, 1.0, Some(&mut cb), None, None)
        .expect("speak");
    let data = audio.lock().unwrap().clone();

    // f32le mono → WAV (kokoro-en is 24 kHz)
    let mut wav: Vec<u8> = Vec::with_capacity(44 + data.len());
    let n = u32::try_from(data.len()).expect("wav under 4 GiB");
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + n).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&24000u32.to_le_bytes());
    wav.extend_from_slice(&96000u32.to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&32u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&n.to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(&out_path, wav).unwrap();
    println!("wrote {out_path} ({} bytes pcm)", data.len());
}
