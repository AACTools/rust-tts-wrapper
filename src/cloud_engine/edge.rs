// The split modules resolve shared names through the parent glob.
#![allow(clippy::wildcard_imports)]

#[cfg(feature = "cloud")]
use super::*;

/// Microsoft Edge "Read Aloud" constants. The trusted client token is the
/// well-known value used by edge-tts / VoiceGarden-SAPI; `Sec-MS-GEC` is
/// derived from it and the current time (see `edge_sec_ms_gec`).
#[cfg(feature = "cloud-core")]
pub(crate) const EDGE_TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";

#[cfg(feature = "cloud-core")]
pub(crate) const EDGE_VOICE_LIST_URL: &str = "https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list?trustedclienttoken=6A5AA1D4EAFF4E9FB37E23D68491D6F4";

#[cfg(feature = "cloud-core")]
pub(crate) const EDGE_DEFAULT_VOICE: &str = "en-US-AriaNeural";

/// Edge's WS endpoint 403-rejects bare handshakes — it expects the Edge
/// browser's Read Aloud User-Agent (and the Read Aloud extension Origin).
#[cfg(feature = "cloud-core")]
pub(crate) const EDGE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0";

#[cfg(feature = "cloud-core")]
pub(crate) const EDGE_ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";

/// The `Sec-MS-GEC-Version` query value: `2-` + the Chromium full version
/// Microsoft's service now requires (the `1-` prefix 403s since their
/// 2026-10 handshake change; mirrors edge-tts PR #492).
#[cfg(feature = "cloud-core")]
pub(crate) const EDGE_SEC_MS_GEC_VERSION: &str = "2-143.0.3650.75";

// ─── Sec-MS-GEC v2 (#141): a SHA-256 variant with a data-dependent
// initial state plus a custom "CS64" checksum, "<16hex>-<64hex>".
// Ported from edge-tts PR #492 (drm.py); golden-vector pinned in the
// tests below. All math is wrapping u32 — the Python original's
// multiply-by-2^32+1 shifts are plain rotate_right, so the round
// function reduces to STANDARD SHA-256 rounds; what differs is the
// IV derivation (salt = the first 7 bytes), the padding (the salt is
// consumed into the IV, length offset by 57), one round constant
// swap in the IV pass, and the CS64 trailer.

/// FIPS 180-4 initial state.
const SHA256_INITIAL: [u32; 8] = [
    0x6A_09E667,
    0xBB_67AE85,
    0x3C_6EF372,
    0xA5_4FF53A,
    0x51_0E527F,
    0x9B_05688C,
    0x1F_83D9AB,
    0x5B_E0CD19,
];
/// The service's custom IV pre-state.
const SHA256_STATE: [u32; 8] = [
    0xFD_9FBEA2,
    0x91_243CE7,
    0xBE_499094,
    0xCD_2E1E0A,
    0x6C_006F49,
    0x04_BD35AB,
    0xFE_B0A7E4,
    0x03_EFFBD2,
];
const SHA256_SCHEDULE: [u32; 14] = [
    0xEB_24BF9E,
    0x15_58691F,
    0xA0_C09EE2,
    0x0C_FA32BA,
    0xA6_22EE8C,
    0xD1_DD9491,
    0xC0_DD25A0,
    0xF9_E70AB6,
    0xF0_C31573,
    0x70_58CE4E,
    0x71_88E606,
    0x00_E5AB33,
    0xD2_58B060,
    0x72_766564,
];
const SHA256_K: [u32; 64] = [
    0x42_8A2F98,
    0x71_374491,
    0xB5_C0FBCF,
    0xE9_B5DBA5,
    0x39_56C25B,
    0x59_F111F1,
    0x92_3F82A4,
    0xAB_1C5ED5,
    0xD8_07AA98,
    0x12_835B01,
    0x24_3185BE,
    0x55_0C7DC3,
    0x72_BE5D74,
    0x80_DEB1FE,
    0x9B_DC06A7,
    0xC1_9BF174,
    0xE4_9B69C1,
    0xEF_BE4786,
    0x0F_C19DC6,
    0x24_0CA1CC,
    0x2D_E92C6F,
    0x4A_7484AA,
    0x5C_B0A9DC,
    0x76_F988DA,
    0x98_3E5152,
    0xA8_31C66D,
    0xB0_0327C8,
    0xBF_597FC7,
    0xC6_E00BF3,
    0xD5_A79147,
    0x06_CA6351,
    0x14_292967,
    0x27_B70A85,
    0x2E_1B2138,
    0x4D_2C6DFC,
    0x53_380D13,
    0x65_0A7354,
    0x76_6A0ABB,
    0x81_C2C92E,
    0x92_722C85,
    // ^ index 32 is 0x27_B70A85 here (the service's table reuses the
    // FIPS K[33] slot — differs from stock SHA-256)
    0xA2_BFE8A1,
    0xA8_1A664B,
    0xC2_4B8B70,
    0xC7_6C51A3,
    0xD1_92E819,
    0xD6_990624,
    0xF4_0E3585,
    0x10_6AA070,
    0x19_A4C116,
    0x1E_376C08,
    0x27_48774C,
    0x34_B0BCB5,
    0x39_1C0CB3,
    0x4E_D8AA4A,
    0x5B_9CCA4F,
    0x68_2E6FF3,
    0x74_8F82EE,
    0x78_A5636F,
    0x84_C87814,
    0x8C_C70208,
    0x90_BEFFFA,
    0xA4_506CEB,
    0xBE_F9A3F7,
    0xC6_7178F2,
];
const SHA256_ADJUSTMENT: u32 = 0xC9_DC06A7;
const SHA256_SIGMA0_ADJUSTMENT: u32 = 0x05_9C0B80;
const SHA256_SIGMA1_ADJUSTMENT: u32 = 0x00_0B92C0;

const CS64_COEFFICIENTS: [(u32, u32); 14] = [
    (0xE8_2E91B1, 0xB3_2700D0),
    (0x44_DE24C6, 0x88_DFF51D),
    (0x3B_EB65BC, 0x29_514038),
    (0xC8_9C5933, 0xB3_28CBDF),
    (0x8D_1EC211, 0xD4_05B4D5),
    (0x31_4EF5D5, 0x26_E55F3C),
    (0xE9_2491B1, 0x60_7801C6),
    (0xF0_A5B9D5, 0x82_B2C1FB),
    (0x06_064CF3, 0xFA_51A3DA),
    (0xDA_79FA60, 0x20_7CC464),
    (0xBE_D20560, 0x3C_3C74F3),
    (0x89_F9C9F8, 0xD2_D14B61),
    (0x35_FA38C6, 0xBB_04A2C1),
    (0xCB_49E3C6, 0xC5_626167),
];
const CS64_FACTOR: u32 = 0x8289_3933;
const CS64_ADJUSTMENT0: u32 = 0xF2F9_0B1A;
const CS64_ADJUSTMENT1: u32 = 0x0B1A_0000;

/// The small sigmas — NOT standard SHA-256: the reference combines
/// `(x>>17)^(x<<15)` (=ROTR17), `(x>>19)^(x<<13)` (=ROTR19), and a
/// plain `x>>10` — the left-half of the third rotation is absent.
fn sigma0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}

fn sigma1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

/// The round big-sigmas as WIDE values: the reference's multiply-by-
/// (2^32+1)-then-shift keeps high bits (up to bit 58), and those
/// carry into the unmasked t1/t2 sums. Low 32 bits equal the
/// standard Σ; the high bits are load-bearing.
fn sigma_round1(e: u32) -> u64 {
    let e64 = u64::from(e);
    ((e64 << 26) | (e64 >> 6)) ^ ((e64 << 21) | (e64 >> 11)) ^ ((e64 << 7) | (e64 >> 25))
}

fn sigma_round0(a: u32) -> u64 {
    let a64 = u64::from(a);
    ((a64 << 30) | (a64 >> 2)) ^ ((a64 << 19) | (a64 >> 13)) ^ ((a64 << 10) | (a64 >> 22))
}

/// Standard SHA-256 rounds over `words` starting at `from`, with a
/// caller-chosen constant table (the IV pass swaps K[14]).
#[allow(clippy::many_single_char_names)] // SHA working registers
fn sha256_rounds(
    state: [u32; 8],
    words: &[u32; 64],
    constants: &[u32; 64],
    from: usize,
) -> [u32; 8] {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
    for (i, &word) in words.iter().enumerate().skip(from) {
        // the reference accumulates UNMASKED and masks only where it
        // assigns: carries from above bit 32 reach the result
        let t1 = u64::from(h)
            + sigma_round1(e)
            + u64::from(g ^ (e & (f ^ g)))
            + u64::from(constants[i])
            + u64::from(word);
        let t2 = sigma_round0(a) + u64::from((a & b) | (c & (a | b)));
        let new_a = (t1.wrapping_add(t2) & 0xFFFF_FFFF) as u32;
        let new_e = (u64::from(d).wrapping_add(t1) & 0xFFFF_FFFF) as u32;
        h = g;
        g = f;
        f = e;
        e = new_e;
        d = c;
        c = b;
        b = a;
        a = new_a;
    }
    [a, b, c, d, e, f, g, h]
}

/// The wide (unmasked) small sigma — shifts left of 32 bits survive so
/// their addition carries can reach the low word.
fn sigma0_wide(x: u32) -> u64 {
    let x = u64::from(x);
    (x >> 7) ^ (x << 25) ^ (x >> 18) ^ (x << 14) ^ (x >> 3)
}

fn sigma1_wide(x: u32) -> u64 {
    let x = u64::from(x);
    (x >> 17) ^ (x << 15) ^ (x >> 19) ^ (x << 13) ^ (x >> 10)
}

/// Extend the message schedule from `from`: the reference adds the
/// UNMASKED sigmas and masks once at the end.
fn expand_schedule(words: &mut Vec<u32>, from: usize) {
    for index in from..64 {
        let sum = sigma1_wide(words[index - 2])
            .wrapping_add(u64::from(words[index - 7]))
            .wrapping_add(sigma0_wide(words[index - 15]))
            .wrapping_add(u64::from(words[index - 16]));
        words.push((sum & 0xFFFF_FFFF) as u32);
    }
}

/// The service's custom IV: 14 zero words, the 7-byte salt as two big-
/// endian words, 15 derived words (schedule + adjustments at exactly
/// indices 16 and 29), then rounds 14.. from the custom state with
/// K[14] swapped for the adjustment constant.
fn sha256_init_words(salt: [u8; 7]) -> Vec<u32> {
    let mut words: Vec<u32> = vec![0; 14];
    words.push(u32::from_be_bytes([0, salt[0], salt[1], salt[2]]));
    words.push(u32::from_be_bytes([salt[3], salt[4], salt[5], salt[6]]));
    for index in 16..30 {
        let mut value = SHA256_SCHEDULE[index - 16];
        // index - 2 >= 14 always holds here: sigma1 at every step,
        // with the adjustment XORed in at exactly index 16
        if index == 16 {
            value = value.wrapping_add(sigma1(words[index - 2]) ^ SHA256_SIGMA1_ADJUSTMENT);
        } else {
            value = value.wrapping_add(sigma1(words[index - 2]));
        }
        if index >= 21 {
            value = value.wrapping_add(words[index - 7]);
        }
        if index == 29 {
            value = value.wrapping_add(sigma0(words[index - 15]) ^ SHA256_SIGMA0_ADJUSTMENT);
        }
        words.push(value);
    }
    words.push(
        sigma1(words[28])
            .wrapping_add(words[23])
            .wrapping_add(sigma0(words[15]))
            .wrapping_add(words[14])
            .wrapping_add(SHA256_ADJUSTMENT)
            .wrapping_sub(SHA256_K[14]),
    );
    words
}

fn sha256_init_v2(salt: [u8; 7]) -> [u32; 8] {
    let mut block_k = SHA256_K;
    block_k[14] = SHA256_ADJUSTMENT;
    let mut words: Vec<u32> = sha256_init_words(salt);
    expand_schedule(&mut words, 31);
    let w: [u32; 64] = std::array::from_fn(|i| words[i]);
    let working = sha256_rounds(SHA256_STATE, &w, &block_k, 14);
    std::array::from_fn(|i| SHA256_INITIAL[i].wrapping_add(working[i]))
}

/// The custom digest: salt consumed into the IV, padding starts at
/// data[7], and the length is offset by 57.
fn sha256_digest_v2(data: &[u8]) -> [u32; 8] {
    let salt: [u8; 7] = data[..7].try_into().expect("token strings exceed 7 bytes");
    let state0 = sha256_init_v2(salt);
    let length = 57 + data.len();
    let mut padded: Vec<u8> = data[7..].to_vec();
    padded.push(0x80);
    let zeros = ((55isize - length as isize).rem_euclid(64)) as usize;
    padded.extend(std::iter::repeat_n(0u8, zeros));
    padded.extend_from_slice(&u64::try_from(length * 8).unwrap_or(u64::MAX).to_be_bytes());
    let mut state = state0;
    for block in padded.chunks(64) {
        let mut words: Vec<u32> = block
            .chunks(4)
            .map(|c| u32::from_be_bytes(c.try_into().expect("4-byte chunk")))
            .collect();
        expand_schedule(&mut words, 16);
        let w: [u32; 64] = words.try_into().expect("64 words");
        let working = sha256_rounds(state, &w, &SHA256_K, 0);
        state = std::array::from_fn(|i| state[i].wrapping_add(working[i]));
    }
    state
}

/// Little-endian 32-bit words of `data`, zero-padded to 4 bytes and an
/// even word count.
fn words_le4_even(data: &[u8]) -> Vec<u32> {
    let pad = data.len().wrapping_neg() % 4;
    let mut buf = data.to_vec();
    buf.extend(std::iter::repeat_n(0u8, pad));
    let mut words: Vec<u32> = buf
        .chunks(4)
        .map(|c| u32::from_le_bytes(c.try_into().expect("4-byte chunk")))
        .collect();
    if words.len() % 2 == 1 {
        words.push(0);
    }
    words
}

/// The CS64 checksum over the token string, keyed by the custom digest.
fn cs64_digest(data: &[u8], digest: &[u32; 8]) -> (u32, u32) {
    // keys are the digest's FIRST BYTES unpacked little-endian
    // (struct.unpack_from("<II", digest)) — not the BE word values
    let key_a = digest[0].swap_bytes() | 1;
    let key_b = digest[1].swap_bytes() | 1;
    let factors = [
        key_a.wrapping_mul(CS64_FACTOR),
        key_b.wrapping_mul(CS64_FACTOR),
    ];
    let mut terms: Vec<u32> = CS64_COEFFICIENTS
        .iter()
        .enumerate()
        .map(|(i, &(coef, _))| coef.wrapping_mul(factors[i % 2]))
        .collect();
    let mut offsets: Vec<u32> = CS64_COEFFICIENTS.iter().map(|&(_, off)| off).collect();
    let mut prefixed = vec![0u8];
    prefixed.extend_from_slice(data);
    for (index, &word) in words_le4_even(&prefixed).iter().enumerate() {
        let mut term = if index % 2 == 0 {
            word.wrapping_mul(key_a)
        } else {
            word.wrapping_mul(key_b)
        };
        let mut offset = if index % 2 == 0 {
            word.rotate_left(16).wrapping_mul(0xEF05_69FB)
        } else {
            word.rotate_left(16).wrapping_mul(0xC317_13DB)
        };
        if index == 0 {
            term = term.wrapping_add(CS64_ADJUSTMENT0.wrapping_mul(factors[0]));
            offset = offset.wrapping_add(CS64_ADJUSTMENT1);
        }
        terms.push(term);
        offsets.push(offset);
    }
    // the Python original accumulates some of these unmasked within an
    // iteration before masking — u64 mirrors that faithfully
    let mut swap_acc: u64 = 0;
    let mut swap_last: u32 = 0;
    let mut state: u32 = 0;
    let mut acc: u64 = 0;
    for index in (0..terms.len()).step_by(2) {
        let x = terms[index];
        let y = terms[index + 1];
        let t = offsets[index].wrapping_add(x).rotate_left(16);
        let q = t.wrapping_mul(0x76F5_0F34);
        swap_acc = swap_acc.wrapping_add(u64::from(
            q.rotate_left(16).wrapping_mul(0xC3EF_EA97).wrapping_add(q),
        ));
        let t = offsets[index + 1].wrapping_add(y).rotate_left(16);
        let q = t.wrapping_mul(0x1881_CE3C);
        swap_last = q.rotate_left(16).wrapping_mul(0x35BD_1EC9).wrapping_add(q);
        swap_acc = (swap_acc.wrapping_add(u64::from(swap_last))) & 0xFFFF_FFFF;

        state = state.wrapping_mul(key_a).wrapping_add(x);
        state = state.rotate_left(16).wrapping_mul(0xCF98_B111);
        state = state.rotate_left(16).wrapping_mul(0x8708_5B9F);
        state = state.rotate_left(16).wrapping_mul(0x12CE_B96D);
        state = state.rotate_left(16).wrapping_mul(0x257E_1D83);
        acc = acc.wrapping_add(u64::from(state));
        let mut t = state.wrapping_mul(key_b).wrapping_add(y);
        t = t.rotate_left(16).wrapping_mul(0xA274_16F5);
        t = t.rotate_left(16).wrapping_mul(0xD383_96FF);
        state = t
            .rotate_left(16)
            .wrapping_mul(0x7C93_2B89)
            .rotate_left(16)
            .wrapping_mul(0xBFA4_9F69);
        acc = (acc.wrapping_add(u64::from(state))) & 0xFFFF_FFFF;
    }
    (swap_last ^ state, (swap_acc ^ acc) as u32)
}

/// Generate the v2 `Sec-MS-GEC` token Microsoft Edge "Read Aloud"
/// requires since its 2026-10 handshake change (#141): the same
/// FILETIME-rounded-to-5-minutes input as v1, but digested with the
/// custom SHA-256 variant + CS64 checksum, formatted
/// `<16hex>-<64hex>`. Pure: caller-supplied Unix nanos (wasm callers
/// pass the JS clock).
#[must_use]
#[cfg(feature = "cloud-core")]
pub fn edge_sec_ms_gec_v2_at(unix_nanos: u128) -> String {
    // integer math is exact here where the reference uses f64
    // formatting: (secs + WIN_EPOCH) rounded to 300 × 1e7 always has
    // ≥7 trailing zero bits, so the f64 rounding never bites
    let secs = unix_nanos / 1_000_000_000;
    let rounded = secs + 11_644_473_600;
    let rounded = rounded - rounded % 300;
    let ticks: u128 = rounded * 10_000_000;
    let input = format!("{ticks}{EDGE_TRUSTED_CLIENT_TOKEN}");
    let digest = sha256_digest_v2(input.as_bytes());
    let (cs_a, cs_b) = cs64_digest(input.as_bytes(), &digest);
    let mut token = String::with_capacity(16 + 1 + 64);
    // struct.pack("<II", cs_a, cs_b): a's LE bytes first
    for b in cs_a.to_le_bytes().iter().chain(cs_b.to_le_bytes().iter()) {
        use std::fmt::Write;
        let _ = write!(token, "{b:02X}");
    }
    token.push('-');
    for w in &digest {
        for b in w.to_be_bytes() {
            use std::fmt::Write;
            let _ = write!(token, "{b:02X}");
        }
    }
    token
}

/// A fresh Edge ConnectionId (uuid v4, dashed — per connection,
/// mirroring edge-tts).
#[cfg(feature = "cloud")]
pub(crate) fn edge_fresh_connection_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A fresh random MUID (16 bytes, uppercase hex) for the handshake's
/// `Cookie: muid=…` header. The uuid crate is already a dependency —
/// a v4's bytes are exactly the 16 random bytes needed.
#[cfg(feature = "cloud")]
pub(crate) fn edge_fresh_muid() -> String {
    // fold instead of format!-per-byte (clippy) — same uppercase hex
    uuid::Uuid::new_v4()
        .as_bytes()
        .iter()
        .fold(String::with_capacity(32), |mut acc, x| {
            use std::fmt::Write;
            let _ = write!(acc, "{x:02X}");
            acc
        })
}

/// v2 `Sec-MS-GEC` from the system clock (native).
#[cfg(feature = "cloud")]
pub fn edge_sec_ms_gec() -> String {
    let unix_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    edge_sec_ms_gec_v2_at(unix_nanos)
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
    if let Ok(mut pool) = WS_POOL.lock() {
        let bucket = pool.entry(url).or_default();
        if bucket.len() < WS_POOL_MAX_PER_URL {
            bucket.push(PooledConn {
                socket,
                born_at: std::time::Instant::now(),
            });
        }
    }
}
#[cfg(feature = "cloud-core")]
pub fn azure_ws_parse_word_boundary(
    item: &serde_json::Value,
) -> Option<(&str, u64, u64, i32, i32)> {
    let data = item.get("Data")?;
    let offset_ticks = data
        .get("Offset")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let duration_ticks = data
        .get("Duration")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);

    // Azure has shipped three different shapes for the boundary text:
    //   1. {"Data": {"text": {"Text": "Hello"}}}   (current)
    //   2. {"Data": {"Text": {"Text": "Hello"}}}   (legacy capital-T)
    //   3. {"Data": {"text": "Hello"}}             (flat string)
    // Resolve in that order.
    let word = data
        .get("text")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("Text")?.as_str())
        .or_else(|| {
            data.get("Text")
                .and_then(|v| v.as_object())
                .and_then(|o| o.get("Text")?.as_str())
        })
        .or_else(|| data.get("text").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())?;

    // Extract character offset and length from the nested text object (when
    // present). Azure WS sends: {"text": {"Text": "word", "Offset": 4, "Length": 5}}
    let text_obj = data.get("text").and_then(|v| v.as_object());
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let char_offset = text_obj
        .and_then(|o| o.get("Offset"))
        .and_then(serde_json::Value::as_i64)
        .map_or(-1, |v| v as i32);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let char_len = text_obj
        .and_then(|o| o.get("Length"))
        .and_then(serde_json::Value::as_i64)
        .map_or(-1, |v| v as i32);

    // Ticks → ms: 1 ms = 10,000 ticks.
    let offset_ms = (offset_ticks.max(0) / 10_000) as u64;
    let duration_ms = (duration_ticks.max(0) / 10_000) as u64;
    Some((word, offset_ms, duration_ms, char_offset, char_len))
}

/// Parse one `Viseme` metadata item into `(viseme_id, offset_sec)`.
#[must_use]
#[cfg(feature = "cloud-core")]
pub fn azure_ws_parse_viseme(item: &serde_json::Value) -> Option<(i32, f32)> {
    let data = item.get("Data")?;
    let viseme_id = data
        .get("VisemeId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0) as i32;
    let offset_ticks = data
        .get("Offset")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    // Ticks → seconds: 1 s = 10,000,000 ticks.
    #[allow(clippy::cast_precision_loss)]
    let offset_sec = (offset_ticks as f64 / 10_000_000.0) as f32;
    Some((viseme_id, offset_sec))
}
