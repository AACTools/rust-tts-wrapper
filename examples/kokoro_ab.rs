// A/B: synthesize the same sentence with whatever lexicon.txt currently
// sits in the model dir; save a wav. Swap lexicons between runs to compare.
// usage: cargo run --features sherpaonnx --example kokoro-ab -- <model-id> <out.wav> [sentence]
fn main() {
    use rust_tts_wrapper::create_engine;
    use std::sync::{Arc, Mutex};

    let model_id = std::env::args()
        .nth(1)
        .expect("model id, e.g. kokoro-en-v0_19");
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

    // model sample rate from its .onnx.json config (`audio.sample_rate`)
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let model_dir = std::path::Path::new(&home)
        .join(".rust-tts-wrapper")
        .join("sherpaonnx")
        .join(&model_id);
    let sample_rate = std::fs::read_dir(&model_dir)
        .ok()
        .and_then(|entries| {
            entries.filter_map(Result::ok).map(|e| e.path()).find(|p| {
                p.extension().is_some_and(|e| e == "json")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".onnx.json"))
            })
        })
        .and_then(|cfg| std::fs::read_to_string(cfg).ok())
        .and_then(|text| {
            text.split("\"sample_rate\"")
                .nth(1)?
                .split(':')
                .nth(1)?
                .split(|c: char| !c.is_ascii_digit())
                .find(|s| !s.is_empty())
                .and_then(|s| s.parse::<u32>().ok())
        })
        .unwrap_or(24000);

    // the callback emits i16le mono PCM — wrap in a matching WAV header
    let mut wav: Vec<u8> = Vec::with_capacity(44 + data.len());
    let n = u32::try_from(data.len()).expect("wav under 4 GiB");
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + n).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM16
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&n.to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(&out_path, wav).unwrap();
    println!(
        "wrote {out_path} ({} bytes pcm @ {sample_rate} Hz)",
        data.len()
    );
}
