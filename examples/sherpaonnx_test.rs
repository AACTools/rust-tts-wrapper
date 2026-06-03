use rust_tts_wrapper::engine::TtsEngine;
use rust_tts_wrapper::factory::create_engine;
use std::io::Write;

fn main() {
    dotenv::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: sherpaonnx_test <model_id> <text> [model_path]");
        eprintln!("\nPopular English models:");
        eprintln!("  kokoro-en-en-19           — Kokoro v0.19 (304MB, best quality)");
        eprintln!("  piper-en-ryan-medium      — Piper Ryan medium (64MB)");
        eprintln!("  piper-en-lessac-medium    — Piper Lessac medium (64MB)");
        eprintln!("  piper-en-amy-medium       — Piper Amy medium (64MB)");
        eprintln!("\nDefault model path: ~/.rust-tts-wrapper/sherpaonnx/<model_id>/");
        std::process::exit(1);
    }

    let model_id = &args[1];
    let text = if args.len() > 3 {
        args[2..].join(" ")
    } else {
        args[2].clone()
    };

    let creds = if args.len() > 4 {
        format!(r#"{{"modelId":"{model_id}","modelPath":"{}"}}"#, args[3])
    } else {
        format!(r#"{{"modelId":"{model_id}"}}"#)
    };

    let engine = create_engine("sherpaonnx", &creds).expect("Failed to create SherpaONNX engine");

    print!("Checking model... ");
    match engine.check_credentials() {
        Ok(true) => println!("loaded"),
        Ok(false) => {
            eprintln!("model not found");
            eprintln!("\nDownload the model first:");
            eprintln!("  mkdir -p ~/.rust-tts-wrapper/sherpaonnx/{model_id}");
            eprintln!("  # Then extract the downloaded archive into that directory");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }

    let voices = engine.get_voices().unwrap_or_default();
    println!("Voices: {} speakers available", voices.len());

    let out_file = format!("/tmp/sherpaonnx_{model_id}.wav");

    print!("Synthesizing \"{text}\" ... ");
    let mut total_bytes = 0usize;
    let mut chunk_count = 0usize;

    let start = std::time::Instant::now();

    engine
        .speak(
            &text,
            None,
            1.0,
            1.0,
            1.0,
            Some(&mut |chunk: &[u8]| {
                total_bytes += chunk.len();
                chunk_count += 1;
            }),
            None,
        )
        .unwrap();

    let elapsed = start.elapsed();

    println!(
        "done in {:.1}ms ({} chunks, {} bytes)",
        elapsed.as_secs_f64() * 1000.0,
        chunk_count,
        total_bytes
    );

    let model_info = engine.get_voices().unwrap_or_default();
    let _ = model_info;

    println!(
        "Streaming: {} chunks delivered via callback (chunked streaming)",
        chunk_count
    );
    println!("Word boundaries: estimated (Sherpa-ONNX offline TTS does not provide word timing)");

    let pcm_data = engine.synth_to_bytes(&text, None, 1.0, 1.0, 1.0).unwrap();
    let wav_header = build_wav_header(pcm_data.len(), 24000);
    let mut f = std::fs::File::create(&out_file).unwrap();
    f.write_all(&wav_header).unwrap();
    f.write_all(&pcm_data).unwrap();
    println!("Saved WAV to {out_file}");
}

fn build_wav_header(data_len: usize, sample_rate: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    let file_size = (36 + data_len) as u32;
    header.extend_from_slice(&file_size.to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&1u16.to_le_bytes()); // mono
    header.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * 2;
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&2u16.to_le_bytes()); // block align
    header.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    header.extend_from_slice(b"data");
    header.extend_from_slice(&(data_len as u32).to_le_bytes());
    header
}
