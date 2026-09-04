//! Offline test for the `ElevenLabs` `/with-timestamps` degrade path: when
//! the endpoint variant is rejected (a model that doesn't support it),
//! `speak()` must retry the plain synthesis endpoint and deliver estimated
//! boundaries instead of failing the call.
//!
//! Uses a `std::net::TcpListener` mock so no network access or API key is
//! needed. The MP3 fixture is 0.4s of silence, regenerated with:
//! `ffmpeg -f lavfi -i anullsrc=r=44100:cl=mono -t 0.4 -q:a 9 silence.mp3`

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use rust_tts_wrapper::factory::create_engine;

const SILENCE_MP3: &[u8] = include_bytes!("fixtures/silence.mp3");

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Minimal HTTP/1.1 responder: drains exactly one request (headers plus
/// Content-Length body) and writes `status` with `body`. Returns the
/// request line so tests can assert on the path.
fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) -> String {
    let mut buf = vec![0u8; 16_384];
    let mut read = 0usize;
    let header_end = loop {
        let n = stream.read(&mut buf[read..]).expect("read request");
        assert!(n > 0, "client closed connection before sending a request");
        read += n;
        if let Some(pos) = find_subsequence(&buf[..read], b"\r\n\r\n") {
            break pos;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length: usize = headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length"))
        .and_then(|l| l.split(':').nth(1))
        .map_or(0, |v| v.trim().parse().expect("content-length"));
    let mut received = read - header_end - 4;
    while received < content_length {
        let n = stream.read(&mut buf[read..]).expect("read body");
        read += n;
        received += n;
    }

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write headers");
    stream.write_all(body).expect("write body");
    stream.flush().expect("flush");
    headers.lines().next().unwrap_or_default().to_string()
}

#[test]
fn elevenlabs_timestamps_rejection_degrades_to_estimated_boundaries() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = std::thread::spawn(move || {
        let mut paths = Vec::new();
        // First attempt: the /with-timestamps variant is rejected.
        let (mut stream, _) = listener.accept().expect("accept 1");
        paths.push(respond(
            &mut stream,
            "404 Not Found",
            "application/json",
            b"{\"detail\":{\"message\":\"not found\"}}",
        ));
        // Retry: plain synthesis endpoint with an MP3 body.
        let (mut stream, _) = listener.accept().expect("accept 2");
        paths.push(respond(&mut stream, "200 OK", "audio/mpeg", SILENCE_MP3));
        paths
    });

    let creds = format!(r#"{{"apiKey":"test-key","synthUrl":"http://{addr}"}}"#);
    let engine = create_engine("elevenlabs", &creds).expect("elevenlabs engine");

    let mut words: Vec<String> = Vec::new();
    let mut on_boundary =
        |word: &str, _start: f32, _end: f32, offset: i32, _len: i32, _final: bool| {
            assert!(
                offset >= 0,
                "estimated word {word:?} must resolve in caller text"
            );
            words.push(word.to_string());
        };
    // Boundaries requested, no on_audio: exercises the degraded streaming
    // entry that previously would have delivered no boundaries at all.
    engine
        .speak(
            "Hello boundary fallback",
            None,
            1.0,
            1.0,
            1.0,
            None,
            Some(&mut on_boundary),
            None,
        )
        .expect("degraded speak must succeed");

    let paths = server.join().expect("server thread");
    assert!(
        paths[0].contains("/with-timestamps"),
        "first attempt must use the variant: {}",
        paths[0]
    );
    assert!(
        !paths[1].contains("/with-timestamps"),
        "retry must drop the variant: {}",
        paths[1]
    );
    assert_eq!(words, ["Hello", "boundary", "fallback"]);
}
