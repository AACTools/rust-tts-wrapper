//! Quick profile harness: model load vs generate wall time on CPU.
//! ```text
//! cargo run --release --no-default-features --features pocket-timing \
//!   --example pocket-prof -- <bundle_dir> <reference.wav> [phoneme text]
//! ```

#[cfg(feature = "pocket-timing")]
fn main() {
    use rust_tts_wrapper::pocket::{PocketConfig, PocketTtsModel};
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let wav = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "reference.wav".into());
    let text = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "h ˈɛ l oʊ|ð ɪ s|ɪ z|m aɪ|v ɔɪ s".into());

    let t0 = std::time::Instant::now();
    let cfg = PocketConfig::from_dir(std::path::Path::new(&dir)).expect("cfg");
    let mut m = PocketTtsModel::load(&cfg).expect("load");
    println!("load: {:.2}s", t0.elapsed().as_secs_f32());

    let reader = hound::WavReader::open(&wav).expect("wav");
    let audio: Vec<f32> = reader
        .into_samples::<i16>()
        .map(|s| f32::from(s.expect("s")) / 32768.0)
        .collect();
    let t1 = std::time::Instant::now();
    let out = m.generate(&text, &audio, 0.3, 1, 500).expect("gen");
    #[allow(clippy::cast_precision_loss)]
    let dur = out.samples.len() as f64 / 24000.0;
    let wall = t1.elapsed().as_secs_f64();
    println!(
        "generate({dur:.2}s audio): {wall:.2}s ({:.1}x realtime)",
        wall / dur
    );
}

#[cfg(not(feature = "pocket-timing"))]
fn main() {}
