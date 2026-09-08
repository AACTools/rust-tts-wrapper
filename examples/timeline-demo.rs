//! Demo: build a playback timeline from word boundaries and use a
//! playback clock to track which word is being spoken — the reader /
//! highlighter pattern. Runs fully offline using the estimator, but the
//! same shape works with real boundaries from `synth_with_boundaries`
//! or the `speak()` boundary callback.
//!
//! ```text
//! cargo run --example timeline-demo
//! ```

use std::thread::sleep;
use std::time::Duration;

use rust_tts_wrapper::engine::estimate_word_boundaries;
use rust_tts_wrapper::timeline::{PlaybackClock, PlaybackTimeline};

fn main() {
    let text = "The quick brown fox jumps over the lazy dog, and then \
                the timeline tells you exactly which word is speaking.";

    // 1. Word boundaries. Real ones come from `synth_with_boundaries`
    //    (cloud engines / floravox) or the speak() boundary callback;
    //    the estimator stands in here so the demo needs no engine.
    let boundaries = estimate_word_boundaries(text);
    println!(
        "{} word boundaries (estimated); first few:",
        boundaries.len()
    );
    for b in boundaries.iter().take(3) {
        println!("  {:>6} ms  {}", b.offset, b.text);
    }

    // 2. Timeline keyed in playback time. 1.5 = playing 50% faster
    //    (e.g. an ffmpeg atempo=1.5 filter); pass 1.0 for normal speed.
    let speed = 1.5;
    let timeline = PlaybackTimeline::from_word_boundaries(&boundaries, text, speed);

    // 3. Playback clock driving highlight lookups while audio plays.
    let mut clock = PlaybackClock::start(speed);
    let step = Duration::from_millis(250);
    for _ in 0..24 {
        // In a real app this is your UI tick, not a sleep.
        sleep(step);
        let elapsed = clock.elapsed_playback_secs();
        let entry = timeline.word_at(elapsed);
        let offset = timeline.byte_offset_at(elapsed);
        match entry {
            Some(e) => println!(
                "{elapsed:5.2}s  word={:<10} char_offset={offset:<3} {}",
                e.word,
                if e.estimated { "(estimated)" } else { "" }
            ),
            None => println!("{elapsed:5.2}s  (before first word)"),
        }

        // Pause demo mid-way: paused time is excluded from the clock.
        if (elapsed - 1.0).abs() < 0.2 && !clock.is_paused() {
            clock.pause();
            println!("        -- paused --");
            sleep(Duration::from_millis(750));
            clock.resume();
        }
    }

    // 4. Seek: after jumping the audio player (e.g. ffplay -ss), rebase
    //    the clock; the timeline immediately reports the new position.
    clock.seek(1.8);
    println!(
        "after seek to 1.8s: word={:?} char_offset={}",
        timeline.word_at(1.8).map(|e| e.word.clone()),
        timeline.byte_offset_at(1.8)
    );
}
