//! Live check: `SpeechMarkdown` through Edge, including [mark:] (whose Azure-
//! dialect output is <bookmark>, zero-audio on the free Edge endpoint unless
//! stripped) and emphasis. cargo run --no-default-features --features cloud
//! --example edge-speechmarkdown
use rust_tts_wrapper::factory::create_engine;

fn main() {
    let engine = create_engine("edge", "{}").expect("edge engine");
    for text in [
        "Plain text reference.",
        "This is (very)[emphasis:\"strong\"] emphasised.",
        "A (mark)[mark:\"m1\"] in speech markdown.",
    ] {
        let mut bytes = 0usize;
        engine
            .speak(
                text,
                Some("en-GB-SoniaNeural"),
                1.0,
                1.0,
                1.0,
                Some(&mut |chunk: &[u8]| bytes += chunk.len()),
                None,
                None,
            )
            .unwrap_or_else(|e| panic!("{text}: {e}"));
        println!("{text:?}: {bytes} bytes");
        assert!(bytes > 0, "{text}: no audio");
    }
    println!("PASS");
}
