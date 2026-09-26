//! Google Instant Custom Voice (Chirp 3) — `generateVoiceCloningKey`.
//!
//! One synchronous REST call: reference audio + a recorded consent
//! statement (verified against Google's fixed script) + language → an
//! opaque `voiceCloningKey` that the caller stores and replays per
//! synthesis (`voice.voice_clone.voice_cloning_key` in
//! `text:synthesize`). The server keeps no voice registry — the key IS
//! the handle.
//!
//! **Allow-list gated**: the API is restricted to allow-listed Google
//! Cloud projects (sales contact required). Everything works in code;
//! un-gated projects get a 403 at clone time.
//!
//! Request shapes verified against the official docs 2026-09-26 (not
//! exercised live in CI — no allow-listed key available).

use super::{
    select_clips, CloneHandle, CloneOutcome, CloningMode, ConsentRecording, ConsentSpec,
    VoiceCloning, VoiceIdentity,
};
use crate::types::{TtsError, TtsResult};
use base64::Engine as _;
use std::collections::HashMap;

/// Google requires (close to) 10 s of reference audio.
const GOOGLE_TARGET_SECS: u32 = 10;

/// The consent script is fixed by Google and verified against the
/// recording — the user must read this exact text.
pub(crate) const GOOGLE_CONSENT_SCRIPT: &str = "I am the owner of this voice and I consent to \
     Google using this voice to create a synthetic voice model.";

pub(crate) struct GoogleCloner {
    api_key: String,
    project: String,
    credentials: HashMap<String, String>,
    client: reqwest::blocking::Client,
}

impl GoogleCloner {
    pub(crate) fn new(credentials: &HashMap<String, String>) -> Self {
        Self {
            api_key: credentials.get("apiKey").cloned().unwrap_or_default(),
            project: credentials.get("projectId").cloned().unwrap_or_default(),
            credentials: credentials.clone(),
            client: reqwest::blocking::Client::new(),
        }
    }

    fn endpoint(&self) -> String {
        // The google engine's synth path uses the global TTS endpoint;
        // mirror it, honouring the same region credential.
        let base = match self.credentials.get("region").map(String::as_str) {
            Some(r) if !r.is_empty() => format!("https://{r}.texttospeech.googleapis.com"),
            _ => "https://texttospeech.googleapis.com".to_string(),
        };
        format!("{base}/v1beta1/voices:generateVoiceCloningKey")
    }

    fn consent_for(identity: &VoiceIdentity) -> Option<&ConsentRecording> {
        identity.consent.iter().find(|c| c.engine == "google")
    }
}

impl VoiceCloning for GoogleCloner {
    fn engine_id(&self) -> &'static str {
        // Registered under the google engine family; the cloner id stays
        // distinct so hosts can distinguish TTS vs cloning capability.
        "google"
    }

    fn cloning_mode(&self) -> CloningMode {
        CloningMode::Instant
    }

    fn consent_spec(&self) -> Option<ConsentSpec> {
        Some(ConsentSpec {
            script: GOOGLE_CONSENT_SCRIPT.into(),
            locale: "en-US".into(),
            metadata_keys: vec!["language_code"],
        })
    }

    fn clone_voice(&self, identity: &VoiceIdentity) -> TtsResult<CloneOutcome> {
        if identity.clips.is_empty() {
            return Err(TtsError("google cloning: identity has no clips".into()));
        }
        let Some(consent) = Self::consent_for(identity) else {
            return Err(TtsError(format!(
                "google cloning: consent recording required — the user must read \
                 this exact script aloud: \"{GOOGLE_CONSENT_SCRIPT}\""
            )));
        };
        let language_code = consent
            .metadata
            .get("language_code")
            .cloned()
            .or_else(|| identity.language.clone())
            .ok_or_else(|| {
                TtsError(
                    "google cloning: consent metadata needs language_code \
                     (e.g. en-US)"
                        .into(),
                )
            })?;

        // ~10 s of reference audio. LINEAR16 means headerless PCM16 LE —
        // no RIFF container.
        let picked = select_clips(&identity.clips, GOOGLE_TARGET_SECS);
        let (mut pcm, rate) = super::concat_clips(&picked, 300)?;
        #[allow(clippy::cast_possible_truncation)]
        let max_pcm = (rate as usize).saturating_mul(2 * GOOGLE_TARGET_SECS as usize);
        if pcm.len() > max_pcm {
            pcm.truncate(max_pcm);
        }
        let reference = pcm;
        let consent_pcm = consent.pcm.clone();

        let b64 = base64::engine::general_purpose::STANDARD;
        let body = serde_json::json!({
            "reference_audio": {
                "audio_config": { "audio_encoding": "LINEAR16" },
                "content": b64.encode(&reference),
            },
            "voice_talent_consent": {
                "audio_config": { "audio_encoding": "LINEAR16" },
                "content": b64.encode(&consent_pcm),
            },
            "consent_script": GOOGLE_CONSENT_SCRIPT,
            "language_code": language_code,
        });

        // Google API keys are not bearer tokens — the crate's google
        // synth engine uses x-goog-api-key; mirror it here.
        let req = self
            .client
            .post(self.endpoint())
            .header("x-goog-api-key", &self.api_key)
            .header("x-goog-user-project", &self.project)
            .json(&body);
        let resp = req
            .send()
            .map_err(|e| TtsError(format!("google cloning: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| TtsError(format!("google cloning read: {e}")))?;
        if !status.is_success() {
            if status.as_u16() == 403 {
                return Err(TtsError(format!(
                    "google cloning {status}: Instant Custom Voice is allow-list \
                     gated — this project is not on it ({text})"
                )));
            }
            return Err(TtsError(format!("google cloning {status}: {text}")));
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| TtsError(format!("google cloning parse: {e}")))?;
        let key = json
            .get("voiceCloningKey")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TtsError("google cloning: no voiceCloningKey in response".into()))?;
        Ok(CloneOutcome::Ready(CloneHandle {
            engine: "google".into(),
            // The key is the handle; there is no server-side registry —
            // the caller (registry) owns persistence.
            voice_id: key.to_string(),
            model: None,
        }))
    }

    fn list_cloned(&self) -> TtsResult<Vec<CloneHandle>> {
        // No server-side registry exists by design — keys are
        // client-stored (the CloneRegistry is that store).
        Ok(Vec::new())
    }

    fn delete_cloned(&self, _handle: &CloneHandle) -> TtsResult<()> {
        // Nothing to delete server-side; dropping the registry entry is
        // the whole lifecycle.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_script_is_pinned() {
        // Providers verify this verbatim — drift here breaks every
        // future clone attempt.
        assert_eq!(
            GOOGLE_CONSENT_SCRIPT,
            "I am the owner of this voice and I consent to Google using \
             this voice to create a synthetic voice model."
        );
    }

    #[test]
    fn consent_gate_errors_without_recording() {
        let cloner = GoogleCloner::new(&HashMap::new());
        let identity = VoiceIdentity {
            name: "x".into(),
            clips: vec![super::super::AudioClip::from_pcm(
                "c",
                vec![0u8; 48_000],
                24_000,
                None,
            )],
            language: Some("en-US".into()),
            consent: Vec::new(),
        };
        let err = cloner.clone_voice(&identity).unwrap_err().to_string();
        assert!(err.contains("consent recording required"), "{err}");
        assert!(err.contains("synthetic voice model"), "{err}");
    }

    #[test]
    fn spec_advertises_the_script() {
        let cloner = GoogleCloner::new(&HashMap::new());
        let spec = cloner.consent_spec().expect("gated");
        assert_eq!(spec.script, GOOGLE_CONSENT_SCRIPT);
        assert!(spec.metadata_keys.contains(&"language_code"));
    }
}
