//! Live regression check (not part of CI): Edge must synthesise audio for
//! the bare `<speak>` envelope speech-dispatcher sends. Run with:
//!   cargo run --no-default-features --features cloud --example edge-bare-envelope
//! Exits non-zero when either variant produces no audio.

use rust_tts_wrapper::factory::create_engine;

fn main() {
    let engine = create_engine("edge", "{}").expect("edge engine");

    for text in [
        "Plain text reference.",
        "<speak>Repeat test number 3</speak>",
        "<speak>With a mark</speak>",
        "<speak>With <mark name=\"i1\"/> a mark</speak>",
    ] {
        let mut bytes = 0usize;
        let mut words = 0usize;
        engine
            .speak(
                text,
                Some("en-GB-SoniaNeural"),
                1.0,
                1.0,
                1.0,
                Some(&mut |chunk: &[u8]| bytes += chunk.len()),
                Some(&mut |_w, _s, _e, _o, _l, _est| words += 1),
                None,
            )
            .unwrap_or_else(|e| panic!("{text}: speak failed: {e}"));
        println!("{text:?}: {bytes} PCM bytes, {words} word boundaries");
        assert!(bytes > 0, "{text}: no audio!");
    }
    println!("PASS");
}
