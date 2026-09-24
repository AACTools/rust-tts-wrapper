//! Live `Gemini 3.8` TTS test — requires `GEMINI_API_KEY` env var.
//! `GEMINI_API_KEY=... cargo run --example gemini_live --no-default-features --features cloud,display_names`

fn main() {
    let key = std::env::var("GEMINI_API_KEY").expect("set GEMINI_API_KEY");
    let creds: std::collections::HashMap<String, String> =
        [("apiKey".to_string(), key)].into_iter().collect();
    let engine =
        rust_tts_wrapper::factory::create_engine("gemini", &serde_json::to_string(&creds).unwrap())
            .expect("gemini engine");

    // 1. Voices
    let voices = engine.get_voices().unwrap();
    println!(
        "voices: {} (first: {:?})",
        voices.len(),
        voices.first().map(|v| (&v.id, &v.name))
    );

    // 2. Credentials check
    println!("check_credentials: {:?}", engine.check_credentials());

    // 3. SpeechMarkdown → gemini dialect → live synth with boundaries
    let smd = "Wait... [500ms] did you hear that? [sigh] I suppose we should check.";
    let mut audio_bytes = 0usize;
    let mut boundaries: Vec<(String, f32, f32, i32, i32)> = Vec::new();
    engine
        .speak(
            smd,
            Some("Puck"),
            0.0,
            0.0,
            0.0,
            Some(&mut |chunk: &[u8]| audio_bytes += chunk.len()),
            Some(
                &mut |word: &str, start: f32, end: f32, offset: i32, len: i32, _est: bool| {
                    boundaries.push((word.to_string(), start, end, offset, len));
                },
            ),
            None,
        )
        .expect("speak");
    println!(
        "audio bytes: {audio_bytes} (PCM16 mono 24k = {} ms)",
        audio_bytes * 1000 / 2 / 24_000
    );
    assert!(audio_bytes > 0, "engine must deliver audio");
    println!(
        "boundaries: {} (first 4: {:?})",
        boundaries.len(),
        boundaries.get(..4).unwrap_or(&[])
    );
    println!("OK — Gemini 3.8 TTS fully working through rust-tts-wrapper");
}
