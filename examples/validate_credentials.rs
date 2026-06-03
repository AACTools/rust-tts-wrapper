use rust_tts_wrapper::factory::create_engine;

struct Provider {
    id: &'static str,
    name: &'static str,
    creds_fn: fn() -> Option<String>,
}

fn main() {
    dotenv::dotenv().ok();

    let providers: Vec<Provider> = vec![
        Provider {
            id: "polly",
            name: "Amazon Polly",
            creds_fn: polly_creds,
        },
        Provider {
            id: "azure",
            name: "Azure TTS",
            creds_fn: azure_creds,
        },
        Provider {
            id: "elevenlabs",
            name: "ElevenLabs",
            creds_fn: elevenlabs_creds,
        },
        Provider {
            id: "witai",
            name: "Wit.ai",
            creds_fn: witai_creds,
        },
        Provider {
            id: "playht",
            name: "PlayHT",
            creds_fn: playht_creds,
        },
        Provider {
            id: "upliftai",
            name: "UpliftAI",
            creds_fn: upliftai_creds,
        },
        Provider {
            id: "openai",
            name: "OpenAI",
            creds_fn: openai_creds,
        },
    ];

    println!("=== TTS Credential Validation ===\n");

    let mut valid = 0;
    let mut invalid = 0;
    let mut missing = 0;

    for provider in &providers {
        let creds = (provider.creds_fn)();
        match creds {
            None => {
                println!(
                    "⏭  {} ({}) — no credentials in env",
                    provider.name, provider.id
                );
                missing += 1;
            }
            Some(creds_json) => {
                let engine = create_engine(provider.id, &creds_json);
                match engine {
                    None => {
                        println!(
                            "❌ {} ({}) — failed to create engine",
                            provider.name, provider.id
                        );
                        invalid += 1;
                    }
                    Some(eng) => match eng.check_credentials() {
                        Ok(true) => {
                            let voice_count = eng.get_voices().map(|v| v.len()).unwrap_or(0);
                            println!(
                                "✅ {} ({}) — credentials valid ({} voices available)",
                                provider.name, provider.id, voice_count
                            );
                            valid += 1;
                        }
                        Ok(false) => {
                            println!(
                                "❌ {} ({}) — credentials rejected by API",
                                provider.name, provider.id
                            );
                            invalid += 1;
                        }
                        Err(e) => {
                            println!(
                                "❌ {} ({}) — validation error: {}",
                                provider.name, provider.id, e
                            );
                            invalid += 1;
                        }
                    },
                }
            }
        }
    }

    println!(
        "\n=== Summary: {} valid, {} invalid, {} skipped ===",
        valid, invalid, missing
    );

    if invalid > 0 {
        std::process::exit(1);
    }
}

fn polly_creds() -> Option<String> {
    let key_id = std::env::var("POLLY_AWS_KEY_ID").ok()?;
    let secret = std::env::var("POLLY_AWS_ACCESS_KEY").ok()?;
    let region = std::env::var("POLLY_REGION").unwrap_or_else(|_| "us-east-1".into());
    Some(format!(
        r#"{{"accessKeyId":"{key_id}","secretAccessKey":"{secret}","region":"{region}"}}"#
    ))
}

fn azure_creds() -> Option<String> {
    let key = std::env::var("MICROSOFT_TOKEN").ok()?;
    let region = std::env::var("MICROSOFT_REGION").unwrap_or_else(|_| "eastus".into());
    Some(format!(
        r#"{{"subscriptionKey":"{key}","region":"{region}"}}"#
    ))
}

fn elevenlabs_creds() -> Option<String> {
    let key = std::env::var("ELEVENLABS_API_KEY").ok()?;
    Some(format!(r#"{{"apiKey":"{key}"}}"#))
}

fn witai_creds() -> Option<String> {
    let token = std::env::var("WITAI_TOKEN").ok()?;
    Some(format!(r#"{{"token":"{token}"}}"#))
}

fn playht_creds() -> Option<String> {
    let key = std::env::var("PLAYHT_API_KEY").ok()?;
    let user_id = std::env::var("PLAYHT_USER_ID").ok()?;
    Some(format!(r#"{{"apiKey":"{key}","userId":"{user_id}"}}"#))
}

fn upliftai_creds() -> Option<String> {
    let key = std::env::var("UPLIFTAI_API_KEY").ok()?;
    Some(format!(r#"{{"apiKey":"{key}"}}"#))
}

fn openai_creds() -> Option<String> {
    let key = std::env::var("OPENAI_API_KEY").ok()?;
    Some(format!(r#"{{"apiKey":"{key}"}}"#))
}
