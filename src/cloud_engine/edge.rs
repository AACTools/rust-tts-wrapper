#![allow(clippy::wildcard_imports)] // shared-import pattern for the split modules
use super::*;

/// Microsoft Edge "Read Aloud" constants. The trusted client token is the
/// well-known value used by edge-tts / VoiceGarden-SAPI; `Sec-MS-GEC` is
/// derived from it and the current time (see `edge_sec_ms_gec`).
#[cfg(feature = "cloud")]
pub(crate) const EDGE_TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
#[cfg(feature = "cloud")]
pub(crate) const EDGE_VOICE_LIST_URL: &str = "https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list?trustedclienttoken=6A5AA1D4EAFF4E9FB37E23D68491D6F4";
#[cfg(feature = "cloud")]
pub(crate) const EDGE_DEFAULT_VOICE: &str = "en-US-AriaNeural";
/// Edge's WS endpoint 403-rejects bare handshakes — it expects the Edge
/// browser's Read Aloud User-Agent (and the Read Aloud extension Origin).
#[cfg(feature = "cloud")]
pub(crate) const EDGE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.0.0";
#[cfg(feature = "cloud")]
pub(crate) const EDGE_ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";

/// Generate the `Sec-MS-GEC` token Microsoft Edge "Read Aloud" requires.
///
/// Algorithm (matching edge-tts / VoiceGarden `WSConnectionPool::GetGECToken`):
/// take the Windows FILETIME tick count (100-ns units since 1601-01-01), round
/// down to the nearest 5-minute boundary (3,000,000,000 ticks), concatenate
/// with the trusted client token, and SHA-256 → uppercase hex. Returns a
/// fresh token valid for up to 5 minutes.
#[cfg(feature = "cloud")]
pub(crate) fn edge_sec_ms_gec() -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let unix_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // FILETIME epoch (1601-01-01) is 116_444_736_000s before Unix (1970-01-01);
    // 100-ns ticks = nanos/100 + that offset in ticks.
    #[allow(clippy::cast_possible_truncation)]
    let filetime_ticks: u128 = unix_nanos / 100 + 116_444_736_000_000_000;
    let rounded = filetime_ticks - (filetime_ticks % 3_000_000_000);
    let input = format!("{rounded}{EDGE_TRUSTED_CLIENT_TOKEN}");
    let hash = Sha256::digest(input.as_bytes());
    let mut token = String::with_capacity(hash.len() * 2);
    for b in &hash {
        let _ = write!(token, "{b:02X}");
    }
    token
}

/// The concrete WebSocket stream type returned by tungstenite's `connect` for a
/// `wss://` URL. Stored in the connection pool between synthesis calls.
#[cfg(feature = "cloud")]
pub(crate) type WsStream =
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

#[cfg(feature = "cloud")]
pub(crate) struct PooledConn {
    socket: WsStream,
    born_at: std::time::Instant,
}

/// Warm WebSocket connection pool, keyed by synthesis URL. A fresh
/// `tungstenite::connect` is a full TLS+WS handshake (~300 ms); reusing a live
/// connection between utterances removes that latency. Azure keys are stable
/// (region+key); Edge keys include the Sec-MS-GEC token so they rotate every
/// 5-minute window (old conns age out instead of being reused stale).
#[cfg(feature = "cloud")]
static WS_POOL: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Vec<PooledConn>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Max age before a pooled connection is considered stale (Azure/Edge close
/// idle sessions after ~3-5 min). Capping at 3 min keeps us from handing out a
/// server-closed connection.
#[cfg(feature = "cloud")]
// clippy's duration_suboptimal_units suggests Duration::from_mins here, but the
// minute/hour constructors are const only behind the unstable
// `duration_constructors_lite` feature and don't build on the stable rustc used
// downstream (e.g. the GNOME SDK rust-stable in the Dasher-GTK Flatpak). Keep
// the portable, const-stable from_secs.
#[allow(clippy::duration_suboptimal_units)]
pub(crate) const WS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3 * 60);
/// Max connections cached per URL — bounds memory for busy callers.
#[cfg(feature = "cloud")]
pub(crate) const WS_POOL_MAX_PER_URL: usize = 4;

/// Take a live connection for `url`, dropping any that have aged out.
#[cfg(feature = "cloud")]
pub(crate) fn ws_checkout(url: &str) -> Option<WsStream> {
    let mut pool = WS_POOL.lock().ok()?;
    let conns = pool.get_mut(url)?;
    while let Some(conn) = conns.pop() {
        if conn.born_at.elapsed() < WS_MAX_AGE {
            return Some(conn.socket);
        }
    }
    None
}

/// Return a connection for reuse, respecting the per-URL cap.
#[cfg(feature = "cloud")]
pub(crate) fn ws_checkin(url: String, socket: WsStream) {
    let Ok(mut pool) = WS_POOL.lock() else {
        return;
    };
    let bucket = pool.entry(url).or_default();
    if bucket.len() < WS_POOL_MAX_PER_URL {
        bucket.push(PooledConn {
            socket,
            born_at: std::time::Instant::now(),
        });
    }
}
