//! Offline tests for the Gemini 3.8 TTS provider: the Interactions API
//! request shape, the no-audio error path (safety refusals / in-band
//! errors), and undecodable-audio handling.
//!
//! Uses a `std::net::TcpListener` mock so no network access or API key is
//! needed (same pattern as `elevenlabs_timestamps_fallback.rs`). The WAV
//! fixture is generated inline — 0.1 s of square-ish wave at 24 kHz.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use rust_tts_wrapper::factory::create_engine;

const GEMINI_JSON_OK: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {LEN}\r\nConnection: close\r\n\r\n{BODY}";

/// Minimal WAV builder: mono 16-bit PCM.
#[allow(clippy::cast_possible_truncation)]
fn tiny_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut wav = Vec::with_capacity(44 + data_len);
    wav.extend_from_slice(b"RIFF");
    #[allow(clippy::cast_possible_truncation)]
    wav.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    #[allow(clippy::cast_possible_truncation)]
    wav.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

fn interaction_json_with_audio(wav_b64: &str) -> String {
    format!(
        r#"{{"steps":[{{"type":"model_output","content":[{{"type":"audio","mime_type":"audio/wav","data":"{wav_b64}"}}]}}]}}"#
    )
}

/// Serve exactly one request from a background thread, responding with a
/// JSON body whose Content-Length the HTTP client can honor. Returns the
/// local port.
fn spawn_mock(body: String) -> (u16, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let request = drain_request(&mut stream);
        let response = GEMINI_JSON_OK
            .replace("{LEN}", &body.len().to_string())
            .replace("{BODY}", &body);
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        request
    });
    (port, handle)
}

/// Read one HTTP request (headers + Content-Length body), return the
/// request line plus body text. The buffer grows on demand — a large
/// request body must not panic the mock.
fn drain_request(stream: &mut TcpStream) -> String {
    let mut buf: Vec<u8> = vec![0u8; 65_536];
    let mut read = 0usize;
    let header_end = loop {
        if read == buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
        let n = stream
            .read(&mut buf[read..])
            .expect("read request");
        assert!(n > 0, "client closed early");
        read += n;
        if let Some(pos) = buf[..read]
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
        {
            break pos;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length: usize = headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length"))
        .and_then(|l| l.split(':').nth(1))
        .map_or(0, |v| v.trim().parse().unwrap_or(0));
    let mut received = read - header_end - 4;
    while received < content_length {
        if read == buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
        let n = stream.read(&mut buf[read..]).expect("read body");
        read += n;
        received += n;
    }
    let request_line = headers.lines().next().unwrap_or("").to_string();
    let body = String::from_utf8_lossy(&buf[header_end + 4..header_end + 4 + content_length]);
    format!("{request_line}\n{body}")
}

fn gemini_engine(synth_url: &str) -> std::sync::Arc<dyn rust_tts_wrapper::engine::TtsEngine> {
    let creds: std::collections::HashMap<String, String> = [
        ("apiKey".to_string(), "test-key".to_string()),
        ("synthUrl".to_string(), synth_url.to_string()),
    ]
    .into_iter()
    .collect();
    create_engine(
        "gemini",
        &serde_json::to_string(&creds).unwrap(),
    )
    .expect("gemini engine")
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn gemini_happy_path_delivers_pcm_and_boundaries() {
    use base64::Engine;
    let samples: Vec<i16> = (0..2400).map(|i| ((i % 100) * 200) as i16).collect();
    let wav = tiny_wav(&samples, 24_000);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
    let (port, handle) = spawn_mock(interaction_json_with_audio(&b64));

    let engine = gemini_engine(&format!("http://127.0.0.1:{port}/interactions"));
    let mut audio_bytes = 0usize;
    let mut boundaries = 0usize;
    engine
        .speak(
            "Plain text, no markdown.",
            Some("Kore"),
            0.0,
            0.0,
            0.0,
            Some(&mut |chunk: &[u8]| audio_bytes += chunk.len()),
            Some(&mut |_w: &str, _s: f32, _e: f32, _o: i32, _l: i32, _est: bool| {
                boundaries += 1;
            }),
            None,
        )
        .expect("speak must succeed");
    let request = handle.join().expect("mock thread");

    assert_eq!(audio_bytes, samples.len() * 2, "PCM16 mono delivered");
    assert!(boundaries > 0, "estimated boundaries fired");

    // Request contract: model, turn structure, speech_config voice,
    // response_format audio.
    let body_line = request.lines().nth(1).unwrap_or("");
    let json: serde_json::Value = serde_json::from_str(body_line).expect("valid JSON body");
    assert_eq!(json["model"], "gemini-3.8-flash-tts");
    assert_eq!(json["response_format"]["type"], "audio");
    assert_eq!(json["generation_config"]["speech_config"][0]["voice"], "Kore");
    assert_eq!(json["input"][0]["type"], "user_input");
    assert_eq!(
        json["input"][0]["content"][0]["text"],
        "Plain text, no markdown."
    );
    // Auth header present.
    assert!(request.lines().next().unwrap_or("").starts_with("POST"));
}

#[test]
fn gemini_no_audio_block_is_an_error_with_detail() {
    // In-band error payload: the API's message must reach the error text.
    let body = r#"{"error":{"code":400,"message":"The prompt was filtered"},"steps":[]}"#
        .to_string();
    let (port, handle) = spawn_mock(body);
    let engine = gemini_engine(&format!("http://127.0.0.1:{port}/interactions"));
    let result = engine.speak("Hello", None, 0.0, 0.0, 0.0, None, None, None);
    handle.join().expect("mock thread");
    let err = result.expect_err("2xx without audio must be an error");
    assert!(
        err.0.contains("no audio") && err.0.contains("filtered"),
        "error includes API detail: {}",
        err.0
    );
}

#[test]
fn gemini_undecodable_audio_is_an_error() {
    // Valid base64, but not a WAV — symphonia decode fails, which must
    // surface as an error rather than a silent zero-byte success.
    use base64::Engine;
    let junk = base64::engine::general_purpose::STANDARD.encode(b"certainly not audio");
    let (port, handle) = spawn_mock(interaction_json_with_audio(&junk));
    let engine = gemini_engine(&format!("http://127.0.0.1:{port}/interactions"));
    let result = engine.speak("Hello", None, 0.0, 0.0, 0.0, None, None, None);
    handle.join().expect("mock thread");
    let err = result.expect_err("undecodable audio must be an error");
    assert!(
        err.0.contains("failed to decode"),
        "error mentions decode failure: {}",
        err.0
    );
}

#[test]
fn gemini_style_credential_reaches_the_wire() {
    use base64::Engine;
    let samples = [0i16; 2400];
    let wav = tiny_wav(&samples, 24_000);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let request = drain_request(&mut stream);
        let body = interaction_json_with_audio(&b64);
        let response = GEMINI_JSON_OK
            .replace("{LEN}", &body.len().to_string())
            .replace("{BODY}", &body);
        let _ = stream.write_all(response.as_bytes());
        request
    });

    let creds: std::collections::HashMap<String, String> = [
        ("apiKey".to_string(), "test-key".to_string()),
        ("synthUrl".to_string(), format!("http://127.0.0.1:{port}/interactions")),
        ("style".to_string(), "whispered urgently".to_string()),
    ]
    .into_iter()
    .collect();
    let engine = create_engine("gemini", &serde_json::to_string(&creds).unwrap()).unwrap();
    engine
        .speak("Hello", None, 2.0, 0.0, 0.0, None, None, None)
        .expect("speak");
    let request = handle.join().expect("mock thread");
    let json: serde_json::Value =
        serde_json::from_str(request.lines().nth(1).unwrap_or("{}")).expect("valid JSON");
    // Credential style wins over the rate-derived style.
    let style = json["input"][0]["content"][0]["annotations"][0]["style"]
        .as_str()
        .unwrap();
    assert_eq!(style, "whispered urgently");
}

#[test]
fn gemini_modelid_credential_reaches_the_wire() {
    use base64::Engine;
    let samples = [0i16; 2400];
    let wav = tiny_wav(&samples, 24_000);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let request = drain_request(&mut stream);
        let body = interaction_json_with_audio(&b64);
        let response = GEMINI_JSON_OK
            .replace("{LEN}", &body.len().to_string())
            .replace("{BODY}", &body);
        let _ = stream.write_all(response.as_bytes());
        request
    });

    // The modelId credential is the documented way to pick
    // flash-lite (or any other model) — it must reach the wire.
    let creds: std::collections::HashMap<String, String> = [
        ("apiKey".to_string(), "test-key".to_string()),
        ("synthUrl".to_string(), format!("http://127.0.0.1:{port}/interactions")),
        ("modelId".to_string(), "gemini-3.8-flash-lite-tts".to_string()),
    ]
    .into_iter()
    .collect();
    let engine = create_engine("gemini", &serde_json::to_string(&creds).unwrap()).unwrap();
    engine
        .speak("Hello", None, 0.0, 0.0, 0.0, None, None, None)
        .expect("speak");
    let request = handle.join().expect("mock thread");
    let json: serde_json::Value =
        serde_json::from_str(request.lines().nth(1).unwrap_or("{}")).expect("valid JSON");
    assert_eq!(json["model"], "gemini-3.8-flash-lite-tts");
}

#[test]
fn gemini_speechmarkdown_routed_through_dialect() {
    // End-to-end through the mock: SpeechMarkdown input must reach the
    // wire already converted to the gemini dialect (angle-bracket tags),
    // and the request must carry the requested voice.
    use base64::Engine;
    let samples = [0i16; 2400];
    let wav = tiny_wav(&samples, 24_000);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
    let (port, handle) = spawn_mock(interaction_json_with_audio(&b64));
    let engine = gemini_engine(&format!("http://127.0.0.1:{port}/interactions"));
    engine
        .speak(
            "Wait [500ms] then [laugh]",
            Some("Puck"),
            0.0,
            0.0,
            0.0,
            None,
            None,
            None,
        )
        .expect("speak");
    let request = handle.join().expect("mock thread");
    let body_line = request.lines().nth(1).unwrap_or("");
    let json: serde_json::Value = serde_json::from_str(body_line).expect("valid JSON body");
    let text = json["input"][0]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("<short pause>"), "dialect on the wire: {text}");
    assert!(text.contains("<laugh>"), "dialect on the wire: {text}");
    assert!(!text.contains("[500ms]"), "no SMD brackets: {text}");
    assert_eq!(json["generation_config"]["speech_config"][0]["voice"], "Puck");
}
