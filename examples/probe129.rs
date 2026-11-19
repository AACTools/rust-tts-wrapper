fn main() {
    match rust_tts_wrapper::factory::create_engine("openai", r#"{"apiKey":"test-key"}"#) {
        Ok(_) => println!("OK"),
        Err(e) => println!("ERR: {e:?}"),
    }
}
