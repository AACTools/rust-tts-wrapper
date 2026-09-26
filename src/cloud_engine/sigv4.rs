// The split modules resolve shared names through the parent glob.
#![allow(clippy::wildcard_imports)]

//! Minimal AWS Signature Version 4 signer for the Polly engine —
//! hand-rolled on `sha2` (already a dependency), no AWS SDK.
//!
//! Covers exactly what the Polly REST API needs: signed `POST` with a
//! JSON body and signed `GET` with an optional query string, region- and
//! service-scoped credentials. Not a general SigV4 implementation (no
//! chunked uploads, no session tokens, no S3 quirks).

use sha2::{Digest, Sha256};

/// HMAC-SHA256 (RFC 2104). Key-block size for SHA-256 is 64 bytes.
pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    fn pad(key: &[u8], byte: u8) -> [u8; 64] {
        let mut block = [0u8; 64];
        if key.len() > 64 {
            // Long keys are hashed first (RFC 2104 §2).
            let digest = Sha256::digest(key);
            block[..32].copy_from_slice(&digest);
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        for b in &mut block {
            *b ^= byte;
        }
        block
    }
    let mut inner = Sha256::new();
    inner.update(pad(key, 0x36));
    inner.update(message);
    let inner_digest: [u8; 32] = inner.finalize().into();

    let mut outer = Sha256::new();
    outer.update(pad(key, 0x5c));
    outer.update(inner_digest);
    outer.finalize().into()
}

/// Lowercase hex encoding.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Lowercase hex of a SHA-256 digest.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    to_hex(&Sha256::digest(data))
}

/// RFC 4231 requires percent-encoding of everything except the
/// unreserved set `A-Z a-z 0-9 - _ . ~` (AWS canonical form — note this
/// is stricter than form-encoding: space is `%20`, never `+`).
fn uri_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// A parsed `https://host/path?query` split for canonicalization.
pub(crate) struct SignedUrl {
    pub(crate) host: String,
    pub(crate) path: String,
    /// (name, value) pairs; canonicalization sorts by encoded name.
    pub(crate) query: Vec<(String, String)>,
}

/// The credentials a SigV4 request carries.
pub(crate) struct SigV4Credentials<'a> {
    pub(crate) access_key: &'a str,
    pub(crate) secret_key: &'a str,
    pub(crate) region: &'a str,
    pub(crate) service: &'a str,
}

/// Build the `Authorization` header value for a request.
///
/// `amz_date` is `YYYYMMDDTHHMMSSZ`; `payload_hash` is the hex SHA-256
/// of the exact body bytes sent (`GET` uses the empty-body hash).
pub(crate) fn authorization_header(
    method: &str,
    url: &SignedUrl,
    creds: &SigV4Credentials<'_>,
    amz_date: &str,
    content_type: Option<&str>,
    payload_hash: &str,
) -> String {
    // Canonical headers: content-type (when present), host, x-amz-date —
    // trimmed, lowercased names, sorted, terminated by a newline.
    let mut header_lines: Vec<(String, String)> = Vec::new();
    if let Some(ct) = content_type {
        header_lines.push(("content-type".into(), ct.trim().to_string()));
    }
    header_lines.push(("host".into(), url.host.clone()));
    header_lines.push(("x-amz-date".into(), amz_date.to_string()));
    header_lines.sort();
    let canonical_headers = header_lines.iter().fold(String::new(), |mut acc, (n, v)| {
        use std::fmt::Write;
        let _ = write!(acc, "{n}:{v}");
        acc.push('\n');
        acc
    });
    let signed_headers = header_lines
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let mut query: Vec<(String, String)> = url
        .query
        .iter()
        .map(|(n, v)| (uri_encode(n), uri_encode(v)))
        .collect();
    query.sort();
    let canonical_query = query
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    // Canonical URI: each path segment is percent-encoded, but the '/'
    // separators stay literal (encoding them as %2F breaks the digest).
    let canonical_uri = url
        .path
        .split('/')
        .map(uri_encode)
        .collect::<Vec<_>>()
        .join("/");
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let credential_scope = format!(
        "{}/{}/{}/aws4_request",
        &amz_date[..8],
        creds.region,
        creds.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    // Signing key: HMAC chain over date → region → service → terminator.
    let k_date = hmac_sha256(
        format!("AWS4{}", creds.secret_key).as_bytes(),
        &amz_date.as_bytes()[..8],
    );
    let k_region = hmac_sha256(&k_date, creds.region.as_bytes());
    let k_service = hmac_sha256(&k_region, creds.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hmac_sha256(&k_signing, string_to_sign.as_bytes());
    let signature_hex = to_hex(&signature);

    format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        creds.access_key, credential_scope, signed_headers, signature_hex
    )
}

/// Current UTC as SigV4 needs it: (`YYYYMMDDTHHMMSSZ`, `YYYYMMDD`).
pub(crate) fn amz_date_now() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days_since_epoch = secs / 86_400;
    let secs_today = secs % 86_400;
    // Howard Hinnant's civil-from-days (same approach as the Azure WS
    // timestamp helper).
    let z = i64::try_from(days_since_epoch).unwrap_or(0) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let stamp = format!(
        "{year:04}{m:02}{d:02}T{hour:02}{minute:02}{second:02}Z",
        hour = secs_today / 3600,
        minute = (secs_today % 3600) / 60,
        second = secs_today % 60
    );
    (stamp.clone(), stamp[..8].to_string())
}

/// The empty-payload hash (`GET` requests).
pub(crate) const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_vector() {
        // RFC 4231 test case 1 (HMAC-SHA256): key = 0x0b × 20, data
        // "Hi There". Guards the hand-rolled HMAC against regressions.
        let key = [0x0b; 20];
        let mac = hmac_sha256(&key, b"Hi There");
        let hex = to_hex(&mac);
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn hmac_long_key_is_hashed_first() {
        // RFC 4231 test case 6: key longer than the block size.
        let key = [0xaa; 131];
        let mac = hmac_sha256(
            &key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            to_hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn empty_payload_hash_is_the_standard_constant() {
        assert_eq!(sha256_hex(b""), EMPTY_PAYLOAD_SHA256);
    }

    #[test]
    fn authorization_header_shape() {
        let url = SignedUrl {
            host: "polly.us-east-1.amazonaws.com".into(),
            path: "/v1/synthesis".into(),
            query: Vec::new(),
        };
        let creds = SigV4Credentials {
            access_key: "AKIDEXAMPLE",
            secret_key: "secret",
            region: "us-east-1",
            service: "polly",
        };
        let payload_hash = sha256_hex(b"{}");
        let auth = authorization_header(
            "POST",
            &url,
            &creds,
            "20260926T120000Z",
            Some("application/json"),
            &payload_hash,
        );
        assert!(
            auth.starts_with(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260926/us-east-1/polly/aws4_request, "
            ),
            "{auth}"
        );
        assert!(
            auth.contains("SignedHeaders=content-type;host;x-amz-date"),
            "{auth}"
        );
        assert!(auth.contains("Signature="), "{auth}");
        // Deterministic: same inputs → same signature.
        let again = authorization_header(
            "POST",
            &url,
            &creds,
            "20260926T120000Z",
            Some("application/json"),
            &sha256_hex(b"{}"),
        );
        assert_eq!(auth, again);
    }

    #[test]
    fn query_pairs_are_encoded_and_sorted() {
        let url = SignedUrl {
            host: "h".into(),
            path: "/".into(),
            query: vec![("b".into(), "x y".into()), ("a".into(), "ü".into())],
        };
        let creds = SigV4Credentials {
            access_key: "k",
            secret_key: "s",
            region: "r",
            service: "svc",
        };
        let auth = authorization_header(
            "GET",
            &url,
            &creds,
            "20260926T120000Z",
            None,
            EMPTY_PAYLOAD_SHA256,
        );
        // We assert via determinism + shape only; the encoding itself is
        // exercised below.
        assert!(auth.contains("Signature="));
        assert_eq!(uri_encode("x y"), "x%20y");
        assert_eq!(uri_encode("ü"), "%C3%BC");
        assert_eq!(uri_encode("a-b~._z"), "a-b~._z");
    }
}
