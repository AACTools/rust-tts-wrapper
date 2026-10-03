//! Generate with the tapped pocket model: cloned voice + REAL word timings
//! (attention-based, no aligner). Run:
//!   cargo run --release --no-default-features --features pocket-timing \
//!     --example pocket-timing-demo -- `<bundle_dir>` `<ref_wav>` `[text]`

// Pragmatic demo: audio math casts + resampling kept simple on purpose.
#[cfg(feature = "pocket-timing")]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::needless_return
)]
fn main() {
    use rust_tts_wrapper::pocket::{timings::word_boundaries, PocketConfig, PocketTtsModel};
    use std::path::PathBuf;

    let dir = std::env::args().nth(1).expect("bundle dir");
    let ref_wav = std::env::args().nth(2).expect("reference wav");
    let text = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "Where is the bathroom".into());

    let cfg = PocketConfig::from_dir(&PathBuf::from(&dir)).expect("bundle layout");
    let mut model = PocketTtsModel::load(&cfg).expect("load");

    // read the reference (16-bit PCM wav via hound), convert to f32 mono
    let mut reader = hound::WavReader::open(&ref_wav).expect("wav");
    let sr_in = reader.spec().sample_rate;
    let chans: u16 = reader.spec().channels;
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .filter_map(Result::ok)
        .collect::<Vec<i16>>()
        .chunks(channels_usize(chans))
        .map(|c| c.iter().map(|s| f32::from(*s) / 32767.0).sum::<f32>() / c.len() as f32)
        .collect();
    // crude linear resample to 24k if needed
    let ref24 = if sr_in == 24_000_u32 {
        samples
    } else {
        let ratio = 24000.0 / f64::from(sr_in);
        let n_out = (samples.len() as f64 * ratio) as usize;
        (0..n_out)
            .map(|i| {
                let p = i as f64 / ratio;
                let i0 = p.floor() as usize;
                let i1 = (i0 + 1).min(samples.len() - 1);
                let f = (p - i0 as f64) as f32;
                samples[i0] * (1.0 - f) + samples[i1] * f
            })
            .collect()
    };

    let t0 = std::time::Instant::now();
    let out = model
        .generate(&text, &ref24, 0.7, 4, 500)
        .expect("generate");
    let dt = t0.elapsed().as_secs_f32();
    println!(
        "generated {:.2}s audio in {:.1}s ({:.1}x realtime), {} steps of attention",
        out.samples.len() as f32 / 24000.0,
        dt,
        out.samples.len() as f32 / 24000.0 / dt,
        out.text_attention.len()
    );

    // word timings from the model's own attention
    let words = word_boundaries(
        &model.tokenizer,
        &out.target_token_ids,
        &out.text_attention,
        0.08,
    );
    println!("word timings (attention):");
    for w in &words {
        println!("  {:?}: {:.2}-{:.2}s", w.word, w.start_s, w.end_s);
    }

    // write 24k 16-bit wav
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: out.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create("/tmp/pocket-rust.wav", spec).expect("writer");
    for s in &out.samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        w.write_sample(v).expect("write");
    }
    println!("written /tmp/pocket-rust.wav");
}

#[cfg(feature = "pocket-timing")]
fn channels_usize(c: u16) -> usize {
    c as usize
}

#[cfg(not(feature = "pocket-timing"))]
fn main() {
    eprintln!("requires --features pocket-timing");
}
