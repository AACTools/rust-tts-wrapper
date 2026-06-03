use rust_tts_wrapper::factory::create_engine;

fn main() {
    dotenv::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: speak_test <engine_id> [text]");
        eprintln!("\nEngines with env-configured credentials:");
        eprintln!(
            "  polly       — Amazon Polly (POLLY_AWS_KEY_ID, POLLY_AWS_ACCESS_KEY, POLLY_REGION)"
        );
        eprintln!("  azure       — Azure TTS (MICROSOFT_TOKEN, MICROSOFT_REGION)");
        eprintln!("  elevenlabs  — ElevenLabs (ELEVENLABS_API_KEY)");
        eprintln!("  witai       — Wit.ai (WITAI_TOKEN)");
        eprintln!("  playht      — PlayHT (PLAYHT_API_KEY, PLAYHT_USER_ID)");
        eprintln!("  upliftai    — UpliftAI (UPLIFTAI_API_KEY)");
        eprintln!("  openai      — OpenAI (OPENAI_API_KEY)");
        eprintln!("  system      — System speech-dispatcher (no credentials)");
        std::process::exit(1);
    }

    let engine_id = &args[1];
    let text = if args.len() > 2 {
        args[2..].join(" ")
    } else {
        "Hello, this is a test of text to speech.".to_string()
    };

    let creds = get_credentials(engine_id);
    let engine =
        create_engine(engine_id, &creds).unwrap_or_else(|| panic!("Unknown engine: {engine_id}"));

    let out_file = format!("/tmp/tts_test_{engine_id}.mp3");

    println!("Engine:  {}", engine_id);
    println!("Text:    {text}");
    println!("Output:  {out_file}");

    print!("Validating credentials... ");
    match engine.check_credentials() {
        Ok(true) => println!("OK"),
        Ok(false) => {
            eprintln!("FAILED — credentials rejected");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }

    print!("Synthesizing... ");
    match engine.synth_to_bytes(&text, None, 1.0, 1.0, 1.0) {
        Ok(audio) => {
            std::fs::write(&out_file, &audio).unwrap_or_else(|e| eprintln!("Write error: {e}"));
            println!("{} bytes written", audio.len());
        }
        Err(e) => {
            eprintln!("Synthesis error: {e}");
            std::process::exit(1);
        }
    }
}

fn get_credentials(engine_id: &str) -> String {
    match engine_id {
        "polly" => {
            let key_id = std::env::var("POLLY_AWS_KEY_ID").unwrap_or_default();
            let secret = std::env::var("POLLY_AWS_ACCESS_KEY").unwrap_or_default();
            let region = std::env::var("POLLY_REGION").unwrap_or_else(|_| "us-east-1".into());
            format!(
                r#"{{"accessKeyId":"{key_id}","secretAccessKey":"{secret}","region":"{region}"}}"#
            )
        }
        "azure" => {
            let key = std::env::var("MICROSOFT_TOKEN").unwrap_or_default();
            let region = std::env::var("MICROSOFT_REGION").unwrap_or_else(|_| "eastus".into());
            format!(r#"{{"subscriptionKey":"{key}","region":"{region}"}}"#)
        }
        "elevenlabs" => {
            let key = std::env::var("ELEVENLABS_API_KEY").unwrap_or_default();
            format!(r#"{{"apiKey":"{key}"}}"#)
        }
        "witai" => {
            let token = std::env::var("WITAI_TOKEN").unwrap_or_default();
            format!(r#"{{"token":"{token}"}}"#)
        }
        "playht" => {
            let key = std::env::var("PLAYHT_API_KEY").unwrap_or_default();
            let user_id = std::env::var("PLAYHT_USER_ID").unwrap_or_default();
            format!(r#"{{"apiKey":"{key}","userId":"{user_id}"}}"#)
        }
        "upliftai" => {
            let key = std::env::var("UPLIFTAI_API_KEY").unwrap_or_default();
            format!(r#"{{"apiKey":"{key}"}}"#)
        }
        "openai" => {
            let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
            format!(r#"{{"apiKey":"{key}"}}"#)
        }
        _ => String::new(),
    }
}
