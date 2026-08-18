//! Live check: `SpeechMarkdown` style sections through Edge (mstts:express-as
//! is zero-audio on the free endpoint, so the Edge path strips the tags and
//! keeps the text).
use rust_tts_wrapper::factory::create_engine;
fn main() {
    let engine = create_engine("edge", "{}").expect("edge engine");
    for text in ["Plain control.", "#[angry] I am angry!"] {
        let mut bytes = 0usize;
        engine
            .speak(
                text,
                Some("en-GB-SoniaNeural"),
                1.0,
                1.0,
                1.0,
                Some(&mut |c: &[u8]| bytes += c.len()),
                None,
            )
            .unwrap_or_else(|e| panic!("{text}: {e}"));
        println!("{text:?}: {bytes} bytes");
        assert!(bytes > 0, "{text}: no audio");
    }
    println!("PASS");
}
