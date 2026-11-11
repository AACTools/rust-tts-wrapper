//! Stable error codes for the C ABI and .NET bindings (GH #69).
//!
//! Engines historically surface failures as free-form `TtsError(String)`
//! messages. A desktop settings screen needs to decide *what the user
//! should do next* — fill in a field, fix credentials, install a model,
//! or retry the connection — which message strings cannot express on
//! their own.
//!
//! This module adds:
//!
//! * [`TtsErrorCode`] — a stable, ordered, wire-compatible enum. The
//!   numeric values are part of the C ABI contract (see
//!   `tts_get_last_error_code`); never renumber, only append.
//! * [`classify_error_message`] — a best-effort classifier that maps
//!   engine error strings onto codes at the FFI boundary, for failures
//!   that originate inside engines (mid-synthesis HTTP failures etc.).
//! * [`redact_message`] — scrubs obvious secret material (API keys,
//!   bearer tokens) so display strings are safe to show and log.
//! * [`ErrorDetail`] — the full structured record (code, message,
//!   engine id, offending field key) with JSON serialization for
//!   `tts_get_last_error_detail_json`.
//!
//! Failures with known origin (engine setup, credential checks) carry
//! exact codes set explicitly at the source; classification is only the
//! fallback for opaque engine messages.

use std::fmt;
use std::os::raw::c_char;

/// Stable error codes crossing the C ABI (GH #69).
///
/// Values are `u32` and part of the wire contract: `.NET` mirrors them in
/// `TtsErrorCode`. Append new codes; never insert or renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TtsErrorCode {
    /// No error recorded yet (also the default for legacy paths).
    Unknown = 0,
    /// A required engine credential/setup field is missing. The detail
    /// carries the offending field key (e.g. `subscriptionKey`).
    MissingConfiguration = 1,
    /// A configuration value is present but malformed (bad JSON, out of
    /// range, unparsable path).
    InvalidConfiguration = 2,
    /// The provider rejected the credentials (401/403, invalid key).
    CredentialsRejected = 3,
    /// The provider could not be reached (DNS, timeout, TLS, refused).
    NetworkError = 4,
    /// A local model / voice file could not be found on disk.
    ModelNotFound = 5,
    /// A native dependency (engine DLL/so/dylib) failed to load.
    MissingNativeDependency = 6,
    /// The engine id is unknown, or compiled out of this build
    /// (feature/platform gated).
    EngineNotAvailable = 7,
    /// The engine rejected the requested language (e.g. an OmniVoice
    /// code outside its accepted table).
    LanguageNotSupported = 8,
    /// Any other engine failure.
    EngineFailure = 9,
}

impl TtsErrorCode {
    /// Stable identifier matching the JSON `name` field.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::MissingConfiguration => "MissingConfiguration",
            Self::InvalidConfiguration => "InvalidConfiguration",
            Self::CredentialsRejected => "CredentialsRejected",
            Self::NetworkError => "NetworkError",
            Self::ModelNotFound => "ModelNotFound",
            Self::MissingNativeDependency => "MissingNativeDependency",
            Self::EngineNotAvailable => "EngineNotAvailable",
            Self::LanguageNotSupported => "LanguageNotSupported",
            Self::EngineFailure => "EngineFailure",
        }
    }

    /// Short, safe, user-facing hint for settings screens. Never embeds
    /// values from the original message (which may contain secrets).
    #[must_use]
    pub fn display_hint(self) -> &'static str {
        match self {
            Self::Unknown => "An unknown error occurred.",
            Self::MissingConfiguration => {
                "A required setting is missing. Fill in the highlighted field."
            }
            Self::InvalidConfiguration => "One of the settings has an invalid value.",
            Self::CredentialsRejected => {
                "The provider rejected these credentials. Check them and try again."
            }
            Self::NetworkError => "Could not reach the service. Check the connection and retry.",
            Self::ModelNotFound => {
                "A required model or voice file is missing. Install or select it."
            }
            Self::MissingNativeDependency => {
                "A required native library is missing. Reinstall the app package."
            }
            Self::EngineNotAvailable => "This engine is not available in the current build.",
            Self::LanguageNotSupported => "This voice's language is not supported by the engine.",
            Self::EngineFailure => "The speech engine failed. Try again.",
        }
    }

    /// Parse from the raw u32 that crossed the ABI. Unknown values map to
    /// [`TtsErrorCode::Unknown`] rather than failing, so newer native
    /// builds degrade gracefully on older bindings.
    #[must_use]
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::MissingConfiguration,
            2 => Self::InvalidConfiguration,
            3 => Self::CredentialsRejected,
            4 => Self::NetworkError,
            5 => Self::ModelNotFound,
            6 => Self::MissingNativeDependency,
            7 => Self::EngineNotAvailable,
            8 => Self::LanguageNotSupported,
            9 => Self::EngineFailure,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for TtsErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Best-effort classification of an engine error message (GH #69).
///
/// Exact codes are set at known origins (engine setup, credential
/// checks); this classifier covers opaque engine strings at the FFI
/// boundary. Order matters: authentication beats network (a 401 over
/// HTTPS is an auth problem, not a connectivity one), and file lookups
/// beat generic failures.
#[must_use]
pub fn classify_error_message(msg: &str) -> TtsErrorCode {
    let m = msg.to_lowercase();

    // Engine availability — the factory's own wording for compiled-out
    // or unknown engines.
    if m.contains("unknown engine")
        || m.contains("not enabled in this build")
        || m.contains("requires the '")
        || m.contains("rebuild with --features")
    {
        return TtsErrorCode::EngineNotAvailable;
    }

    // Language rejections (omnivoice "unknown language" and friends).
    if m.contains("unknown language") || m.contains("unsupported language") {
        return TtsErrorCode::LanguageNotSupported;
    }

    // Authentication: explicit rejections by the provider. Bare digit
    // substrings are NOT matched ("loaded chunk 4012" is not an auth
    // failure) — the digits must appear as standalone tokens AND in an
    // HTTP/auth context. 407 (proxy auth) is deliberately absent: the
    // provider never saw the credentials, so it is a retry/config
    // issue, not a rejection.
    if m.contains("unauthorized")
        || m.contains("forbidden")
        || m.contains("invalid api key")
        || m.contains("invalid_api_key")
        || m.contains("api key invalid")
        || m.contains("authentication")
        || m.contains("credentials rejected")
        || m.contains("incorrect api key")
        || http_status_in(&m, &["401", "403"])
    {
        return TtsErrorCode::CredentialsRejected;
    }

    // Missing configuration: factory/engine wording for absent fields.
    if m.contains("required")
        && (m.contains("credential")
            || m.contains("api key")
            || m.contains("key")
            || m.contains("path")
            || m.contains("model")
            || m.contains("field"))
    {
        return TtsErrorCode::MissingConfiguration;
    }

    // Native dependency load failures.
    if (m.contains("library") || m.contains("dll") || m.contains("dylib") || m.contains("dlopen"))
        && (m.contains("fail") || m.contains("not found") || m.contains("cannot open"))
    {
        return TtsErrorCode::MissingNativeDependency;
    }

    // Local model/voice files.
    if m.contains("model not found")
        || m.contains("no such file")
        || m.contains("file not found")
        || m.contains("failed to load reference wav")
        || m.contains("failed to load model")
        || m.contains("enoent")
        || (m.contains("missing") && (m.contains("file") || m.contains("model")))
    {
        return TtsErrorCode::ModelNotFound;
    }

    // Connectivity. "error sending request" is reqwest's transport
    // failure wording (as wrapped by the cloud engine's "HTTP error:"
    // prefix).
    if m.contains("error sending request")
        || m.contains("network")
        || m.contains("dns")
        || m.contains("timeout")
        || m.contains("timed out")
        || m.contains("connection refused")
        || m.contains("connection closed")
        || m.contains("connect error")
        || m.contains("unreachable")
        || m.contains("tls")
        || m.contains("ssl")
    {
        return TtsErrorCode::NetworkError;
    }

    TtsErrorCode::EngineFailure
}

/// Scrub obvious secret material from an error message so it is safe to
/// display or log (GH #69: "with secret values redacted").
///
/// Handles the shapes that realistically leak into error strings: OpenAI
///-style `sk-…` keys (at word boundaries), `Bearer` headers, and JSON
/// `"apiKey":"…"` fragments (camelCase, snake_case and kebab-case key
/// spellings).
#[must_use]
pub fn redact_message(msg: &str) -> String {
    let redacted = replace_prefixed_tokens(msg, &["sk-", "rk-", "gsk_"], "«redacted»");
    let redacted = replace_prefixed_tokens(&redacted, &["Bearer ", "bearer "], "Bearer «redacted»");
    redact_json_secret_values(&redacted)
}

/// Replace `<prefix><token>` runs with `replacement`, left to right.
/// A prefix with no following token run is left untouched (and skipped),
/// so the function always terminates even on bare prefixes. Prefixes
/// must start at a word boundary (not preceded by alnum/`-`/`_`), so
/// "task-risk" never triggers the `sk-` scanner.
fn replace_prefixed_tokens(s: &str, prefixes: &[&str], replacement: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        // Earliest boundary-valid prefix occurrence in what is left.
        let found = prefixes
            .iter()
            .filter_map(|p| rest.find(p).map(|i| (i, *p)))
            .filter(|(i, _)| {
                *i == 0
                    || !rest
                        .as_bytes()
                        .get(i.wrapping_sub(1))
                        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
            })
            .min_by_key(|(i, _)| *i);
        let Some((pos, prefix)) = found else { break };
        let start = pos + prefix.len();
        let tlen = token_len(rest, start);
        if tlen == 0 {
            // Bare prefix with no token after it — keep it and continue
            // scanning after the prefix.
            out.push_str(&rest[..start]);
            rest = &rest[start..];
            continue;
        }
        out.push_str(&rest[..pos]);
        out.push_str(replacement);
        rest = &rest[start + tlen..];
    }
    out.push_str(rest);
    out
}

/// Redact the string values of secret-looking JSON members
/// (`"apiKey":"…"`, `"token": "…"`, …). Non-secret members pass through.
fn redact_json_secret_values(s: &str) -> String {
    const SECRET_KEYS: [&str; 13] = [
        "apikey",
        "api_key",
        "api-key",
        "subscriptionkey",
        "subscription-key",
        "secretaccesskey",
        "secret-key",
        "access-token",
        "authorization",
        "client-secret",
        "token",
        "password",
        "secret",
    ];
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        let Some(quote) = rest.find('"') else {
            out.push_str(rest);
            break;
        };
        // Copy up to and including the opening quote.
        out.push_str(&rest[..quote + 1]);
        let after_quote = &rest[quote + 1..];
        let Some(close_rel) = after_quote.find('"') else {
            out.push_str(after_quote);
            break;
        };
        let key = &after_quote[..close_rel];
        let after_key = &after_quote[close_rel + 1..];
        let trimmed = after_key.trim_start();
        if SECRET_KEYS.contains(&key.to_lowercase().as_str()) && trimmed.starts_with(':') {
            let after_colon = trimmed[1..].trim_start();
            if let Some(value) = after_colon.strip_prefix('"') {
                // Find the value's closing quote, honouring backslash
                // escapes ("a\"b" is one value, not two).
                let mut end_rel = None;
                let bytes = value.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2, // skip the escaped character
                        b'"' => {
                            end_rel = Some(i);
                            break;
                        }
                        _ => i += 1,
                    }
                }
                if let Some(end_rel) = end_rel {
                    if end_rel > 0 {
                        // `"key" : "value"` → keep the quoted key and
                        // the separator, redact the value. (The closing
                        // quote of the key MUST be re-emitted — it was
                        // consumed into after_key.)
                        out.push_str(key);
                        out.push('"');
                        out.push_str(&after_key[..after_key.len() - after_colon.len()]);
                        out.push_str("\"«redacted»\"");
                        rest = &value[end_rel + 1..];
                        continue 'outer;
                    }
                }
            }
        }
        // Not a secret member: copy the key and continue after it.
        out.push_str(key);
        out.push('"');
        rest = after_key;
    }
    out
}

/// Length of a run of token-ish characters (alnum, `-`, `_`, `.`)
/// starting at `pos`, capped at 128.
fn token_len(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut end = pos;
    while end < bytes.len() && end - pos < 128 {
        let b = bytes[end];
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' {
            end += 1;
        } else {
            break;
        }
    }
    end - pos
}

/// True when `status` (e.g. "401") appears as a standalone token AND
/// the message carries an HTTP-status context word — so "loaded chunk
/// 4012 at offset 401" never classifies as an auth failure, while
/// "HTTP status client error (401 Unauthorized)" does. "request"/
/// "response" are deliberately NOT context words: reqwest transport
/// errors always contain them ("error sending request…"), and a stray
/// byte count there must not flip the classification.
fn http_status_in(lower_msg: &str, statuses: &[&str]) -> bool {
    let has_context = lower_msg.contains("http") || lower_msg.contains("status");
    if !has_context {
        return false;
    }
    for status in statuses {
        let mut search = 0;
        while let Some(rel) = lower_msg[search..].find(status) {
            let start = search + rel;
            let end = start + status.len();
            let boundary_before =
                start == 0 || !lower_msg[..start].ends_with(|c: char| c.is_ascii_digit());
            let boundary_after = end == lower_msg.len()
                || !lower_msg[end..].starts_with(|c: char| c.is_ascii_digit());
            if boundary_before && boundary_after {
                return true;
            }
            search = end;
        }
    }
    false
}

/// The structured error record surfaced by the ABI (GH #69).
#[derive(Debug, Clone)]
pub struct ErrorDetail {
    /// Stable classification.
    pub code: TtsErrorCode,
    /// Engine message (secret-redacted for the JSON/display forms).
    pub message: String,
    /// Engine id the failure belongs to, when known.
    pub engine_id: Option<String>,
    /// Offending credential/setup field key, when known
    /// (e.g. `subscriptionKey`).
    pub field_key: Option<String>,
}

impl ErrorDetail {
    /// Build a detail for an opaque engine failure: classifies the
    /// message and redacts it.
    #[must_use]
    pub fn from_engine_message(engine_id: Option<&str>, message: &str) -> Self {
        Self {
            code: classify_error_message(message),
            message: redact_message(message),
            engine_id: engine_id.map(std::string::ToString::to_string),
            field_key: None,
        }
    }

    /// Build a detail with an explicit code (known-origin failures).
    #[must_use]
    pub fn new(
        code: TtsErrorCode,
        engine_id: Option<&str>,
        field_key: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: redact_message(&message.into()),
            engine_id: engine_id.map(std::string::ToString::to_string),
            field_key: field_key.map(std::string::ToString::to_string),
        }
    }

    /// A safe, display-ready message: the code's hint plus the field or
    /// engine context when available. Never contains message content.
    #[must_use]
    pub fn display_message(&self) -> String {
        let mut s = self.code.display_hint().to_string();
        if let Some(field) = &self.field_key {
            s.push_str(&format!(" (field: {field})"));
        }
        if let Some(engine) = &self.engine_id {
            s.push_str(&format!(" [engine: {engine}]"));
        }
        s
    }

    /// JSON form for `tts_get_last_error_detail_json`.
    #[must_use]
    pub fn to_json(&self) -> String {
        let engine = self.engine_id.as_deref().unwrap_or_default();
        let field = self.field_key.as_deref().unwrap_or_default();
        let message = json_escape(&self.message);
        let display = json_escape(&self.display_message());
        format!(
            "{{\"code\":{},\"name\":\"{}\",\"message\":\"{}\",\"displayMessage\":\"{}\",\
             \"engineId\":\"{}\",\"fieldKey\":\"{}\"}}",
            self.code as u32,
            self.code.name(),
            message,
            display,
            json_escape(engine),
            json_escape(field)
        )
    }

    /// Owned C string for the ABI; null on allocation failure.
    pub(crate) fn into_c_string(self) -> Option<std::ffi::CString> {
        let json = self.to_json();
        // Interior nulls cannot occur after json_escape, but be safe.
        std::ffi::CString::new(json).ok()
    }
}

/// Minimal JSON string escaping (serde_json is available but this keeps
/// the module dependency-free and output stable).
#[must_use]
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Convenience alias used by the FFI layer.
pub type ErrorDetailPtr = *mut c_char;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_through_u32() {
        for code in [
            TtsErrorCode::Unknown,
            TtsErrorCode::MissingConfiguration,
            TtsErrorCode::InvalidConfiguration,
            TtsErrorCode::CredentialsRejected,
            TtsErrorCode::NetworkError,
            TtsErrorCode::ModelNotFound,
            TtsErrorCode::MissingNativeDependency,
            TtsErrorCode::EngineNotAvailable,
            TtsErrorCode::LanguageNotSupported,
            TtsErrorCode::EngineFailure,
        ] {
            assert_eq!(TtsErrorCode::from_u32(code as u32), code);
        }
        // Future/unknown values degrade to Unknown instead of panicking.
        assert_eq!(TtsErrorCode::from_u32(9999), TtsErrorCode::Unknown);
    }

    #[test]
    fn classification_auth_beats_network() {
        // "401" in an HTTPS failure message is an auth problem.
        assert_eq!(
            classify_error_message("HTTP 401 Unauthorized: https://api.example.com"),
            TtsErrorCode::CredentialsRejected
        );
        assert_eq!(
            classify_error_message("request failed with 403 Forbidden"),
            TtsErrorCode::CredentialsRejected
        );
        assert_eq!(
            classify_error_message("HTTP status client error (401 Unauthorized) for url (…)"),
            TtsErrorCode::CredentialsRejected
        );
    }

    #[test]
    fn classification_bare_status_digits_do_not_match() {
        // Digit substrings without HTTP/auth context are not auth
        // failures (GH #69 review: byte counts, offsets, chunk ids).
        assert_ne!(
            classify_error_message("failed to load model chunk 4012 at offset 401"),
            TtsErrorCode::CredentialsRejected
        );
        assert_ne!(
            classify_error_message("decoded 403 frames"),
            TtsErrorCode::CredentialsRejected
        );
    }

    #[test]
    fn classification_network() {
        assert_eq!(
            classify_error_message("error sending request for url (https://x): connection refused"),
            TtsErrorCode::NetworkError
        );
        assert_eq!(
            classify_error_message("dns error: resolve failed"),
            TtsErrorCode::NetworkError
        );
        assert_eq!(
            classify_error_message("operation timed out"),
            TtsErrorCode::NetworkError
        );
    }

    #[test]
    fn classification_engine_availability() {
        assert_eq!(
            classify_error_message("Engine 'omnivoice-local' is not enabled in this build. Rebuild with --features omnivoice-local."),
            TtsErrorCode::EngineNotAvailable
        );
        assert_eq!(
            classify_error_message("Unknown engine: foo"),
            TtsErrorCode::EngineNotAvailable
        );
    }

    #[test]
    fn classification_language() {
        assert_eq!(
            classify_error_message("ov_synthesize failed (status -1): unknown language"),
            TtsErrorCode::LanguageNotSupported
        );
    }

    #[test]
    fn classification_missing_config() {
        assert_eq!(
            classify_error_message("omnivoice-local: model_path and codec_path required"),
            TtsErrorCode::MissingConfiguration
        );
    }

    #[test]
    fn classification_model_files() {
        assert_eq!(
            classify_error_message("failed to load model /home/x/m.gguf"),
            TtsErrorCode::ModelNotFound
        );
        assert_eq!(
            classify_error_message("no such file or directory: /models/a.onnx"),
            TtsErrorCode::ModelNotFound
        );
    }

    #[test]
    fn redact_sk_keys() {
        let out = redact_message("request for key sk-abcdef1234567890abcdef rejected");
        assert!(!out.contains("abcdef1234567890"), "key leaked: {out}");
        assert!(out.contains("«redacted»"), "missing marker: {out}");
    }

    #[test]
    fn redact_bearer() {
        let out = redact_message("header Bearer abcdefghijklmnop was invalid");
        assert!(!out.contains("abcdefghijklmnop"), "token leaked: {out}");
        assert!(out.contains("Bearer «redacted»"), "got: {out}");
    }

    #[test]
    fn redact_json_key_values() {
        let out = redact_message(r#"sent {"apiKey":"supersecret123","model":"tts-1"}"#);
        assert!(!out.contains("supersecret123"), "secret leaked: {out}");
        // Non-secret fields survive.
        assert!(out.contains("tts-1"), "over-redacted: {out}");
    }

    #[test]
    fn redact_terminates_on_bare_prefixes() {
        // A "Bearer " with no token after it must not loop forever.
        let out = redact_message("header ended with 'Bearer ' and 'sk-' exactly");
        assert!(out.contains("Bearer "), "bare prefix kept: {out}");
        assert!(out.contains("sk-"), "bare prefix kept: {out}");
    }

    #[test]
    fn redact_preserves_plain_text() {
        let msg = "connection refused while reaching api.openai.com";
        assert_eq!(redact_message(msg), msg);
    }

    #[test]
    fn redact_does_not_trigger_mid_word() {
        // "task-risk" contains "sk-" but is not a key.
        let out = redact_message("task-risk assessment desk-lookup");
        assert_eq!(out, "task-risk assessment desk-lookup");
    }

    #[test]
    fn redact_kebab_case_secret_keys() {
        let out = redact_message(r#"sent {"api-key":"supersecret1","region":"us"}"#);
        assert!(!out.contains("supersecret1"), "secret leaked: {out}");
        assert!(out.contains("us"), "over-redacted: {out}");
    }

    #[test]
    fn redacted_output_stays_parseable_json() {
        // The redacted message is displayed and serialized verbatim
        // (ErrorDetail JSON, TtsException.Message) — it must remain
        // valid JSON when the input was.
        for input in [
            r#"sent {"apiKey":"supersecret123"}"#,
            r#"sent {"apiKey": "supersecret123", "model": "tts-1"}"#,
            r#"sent { "api-key" : "x" , "n": 5}"#,
            // Escaped quotes inside the secret value.
            r#"sent {"token":"a\"b"}"#,
        ] {
            let redacted = redact_message(input);
            let reparsed =
                serde_json::from_str::<serde_json::Value>(&redacted.replace("sent ", ""))
                    .unwrap_or_else(|e| panic!("redacted output not parseable: {redacted} ({e})"));
            assert!(reparsed.is_object(), "{redacted}");
        }
    }

    #[test]
    fn classifier_proxy_auth_407_is_not_a_rejection() {
        // The engine's 407 wording (chosen to avoid auth keywords) must
        // classify as a retryable failure, never CredentialsRejected.
        assert_ne!(
            classify_error_message(
                "credential check failed: proxy authorization required (HTTP 407)"
            ),
            TtsErrorCode::CredentialsRejected
        );
    }

    #[test]
    fn classifier_transport_errors_with_stray_status_digits_stay_network() {
        // reqwest transport errors always say "request"; a byte count
        // there must not read as an auth rejection.
        assert_eq!(
            classify_error_message(
                "error sending request: connection closed with 401 bytes remaining"
            ),
            TtsErrorCode::NetworkError
        );
    }

    #[test]
    fn detail_json_shape() {
        let d = ErrorDetail::new(
            TtsErrorCode::MissingConfiguration,
            Some("azure"),
            Some("subscriptionKey"),
            "azure: subscriptionKey required",
        );
        let json = d.to_json();
        assert!(json.contains(r#""code":1"#), "{json}");
        assert!(json.contains(r#""name":"MissingConfiguration""#), "{json}");
        assert!(json.contains(r#""fieldKey":"subscriptionKey""#), "{json}");
        assert!(json.contains(r#""engineId":"azure""#), "{json}");
        // displayMessage carries the safe hint, not the raw message.
        assert!(d.display_message().contains("required setting is missing"));
        assert!(d.display_message().contains("subscriptionKey"));
    }

    #[test]
    fn detail_json_escapes_and_redacts() {
        let d = ErrorDetail::from_engine_message(
            Some("openai"),
            "key sk-abcdef12345678 not valid \"quoted\"",
        );
        let json = d.to_json();
        assert!(!json.contains("abcdef12345678"), "leaked in: {json}");
        assert!(json.contains("\\\"quoted\\\""), "unescaped quotes: {json}");
    }
}
