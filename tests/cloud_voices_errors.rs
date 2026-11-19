//! #129 acceptance: cloud voice-list HTTP failures surface as real
//! errors (never a silent empty list), and a service-account JSON
//! keyFilePath fails engine creation with the specific reason.
//! Local `TcpListener` mocks — no network, no keys.

#![cfg(feature = "cloud")]

use std::io::{Read, Write};
use std::net::TcpListener;

use rust_tts_wrapper::factory::create_engine;

/// One-shot HTTP server answering `status` with an empty body.
fn status_server(status: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut s = stream;
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
        }
    });
    port
}

#[test]
fn voice_list_401_is_an_error_not_an_empty_list() {
    let port = status_server("401 Unauthorized");
    let engine = create_engine(
        "google",
        &format!(r#"{{"apiKey":"dummy","voicesUrl":"http://127.0.0.1:{port}/voices"}}"#),
    )
    .expect("engine");
    let err = engine
        .get_voices()
        .expect_err("401 must surface as an error");
    let msg = format!("{err:?}");
    assert!(msg.contains("401"), "message carries the status: {msg}");
}

#[test]
fn voice_list_503_is_an_error() {
    let port = status_server("503 Service Unavailable");
    let engine = create_engine(
        "google",
        &format!(r#"{{"apiKey":"dummy","voicesUrl":"http://127.0.0.1:{port}/voices"}}"#),
    )
    .expect("engine");
    assert!(engine.get_voices().is_err());
}

#[test]
fn service_account_json_keyfile_fails_creation_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("sa.json");
    std::fs::write(
        &key,
        "{\n  \"type\": \"service_account\",\n  \"project_id\": \"x\"\n}\n",
    )
    .unwrap();
    let creds = serde_json::json!({ "keyFilePath": key.display().to_string() }).to_string();
    let err = create_engine("google", &creds).expect_err("service-account JSON must fail creation");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("service-account"),
        "message names the cause: {msg}"
    );
    assert!(msg.contains("apiKey"), "message names the fix: {msg}");
}

#[test]
fn plain_text_keyfile_still_reads_first_line() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key.txt");
    std::fs::write(&key, "\u{feff}  \nAIza-example-plain-key\n").unwrap();
    let creds = serde_json::json!({ "keyFilePath": key.display().to_string() }).to_string();
    // creation succeeds (BOM + blank line skipped, first real line
    // is the key) — the config embeds it in the URLs
    let _engine = create_engine("google", &creds).expect("plain key file works");
}

#[test]
fn unknown_engine_message_keeps_the_hint() {
    let err = create_engine("not-a-cloud", "{}").expect_err("unknown engine");
    let msg = format!("{err:?}");
    assert!(msg.contains("Available engines"), "keeps the hint: {msg}");
}
