//! ElevenLabs Instant Voice Cloning (IVC).
//!
//! `POST /v1/voices/add` (multipart `name` + `files[]`), synchronous.
//! Professional Voice Cloning (captcha-verified multi-step pipeline) is
//! deliberately out of scope. Consent is a dashboard checkbox on
//! ElevenLabs' side — there is no API consent field; the caller owns
//! having permission. Request shapes verified against the API docs
//! 2026-09-26 (not exercised live in CI).

use super::{
    select_clips, wav_bytes, CloneHandle, CloneOutcome, CloningMode, VoiceCloning, VoiceIdentity,
};
use crate::types::{TtsError, TtsResult};
use std::collections::HashMap;

/// ElevenLabs guidance: 1–2 min recommended, >3 min detrimental. 16-bit
/// WAV is accepted; mono is fine.
const ELEVENLABS_TARGET_SECS: u32 = 120;

pub(crate) struct ElevenLabsCloner {
    api_key: String,
    client: reqwest::blocking::Client,
}

impl ElevenLabsCloner {
    pub(crate) fn new(credentials: &HashMap<String, String>) -> Self {
        Self {
            api_key: credentials.get("apiKey").cloned().unwrap_or_default(),
            client: reqwest::blocking::Client::new(),
        }
    }

    fn url(suffix: &str) -> String {
        format!("https://api.elevenlabs.io/v1/voices{suffix}")
    }
}

impl VoiceCloning for ElevenLabsCloner {
    fn engine_id(&self) -> &'static str {
        "elevenlabs"
    }

    fn cloning_mode(&self) -> CloningMode {
        CloningMode::Instant
    }

    fn clone_voice(&self, identity: &VoiceIdentity) -> TtsResult<CloneOutcome> {
        if identity.clips.is_empty() {
            return Err(TtsError("elevenlabs cloning: identity has no clips".into()));
        }
        let picked = select_clips(&identity.clips, ELEVENLABS_TARGET_SECS);
        let mut form =
            reqwest::blocking::multipart::Form::new().text("name", identity.name.clone());
        for clip in picked {
            // Per-clip cap: >3 min of audio is detrimental per the docs;
            // a single long caller-built clip goes out truncated.
            #[allow(clippy::cast_possible_truncation)]
            let max_pcm =
                (clip.sample_rate as usize).saturating_mul(2 * ELEVENLABS_TARGET_SECS as usize);
            let pcm = if clip.pcm.len() > max_pcm {
                let mut trimmed = clip.pcm.clone();
                trimmed.truncate(max_pcm);
                trimmed
            } else {
                clip.pcm.clone()
            };
            let part = reqwest::blocking::multipart::Part::bytes(wav_bytes(&pcm, clip.sample_rate))
                .file_name(format!("{}.wav", clip.name))
                .mime_str("audio/wav")
                .map_err(|e| TtsError(format!("mime header: {e}")))?;
            form = form.part("files", part);
        }
        let resp = self
            .client
            .post(Self::url("/add"))
            .header("xi-api-key", &self.api_key)
            .multipart(form)
            .send()
            .map_err(|e| TtsError(format!("elevenlabs voice API: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| TtsError(format!("elevenlabs voice API read: {e}")))?;
        if !status.is_success() {
            return Err(TtsError(format!("elevenlabs voice API {status}: {text}")));
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| TtsError(format!("elevenlabs voice API parse: {e}")))?;
        let voice_id = json
            .get("voice_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TtsError("elevenlabs cloning: no voice_id in response".into()))?;
        Ok(CloneOutcome::Ready(CloneHandle {
            engine: "elevenlabs".into(),
            voice_id: voice_id.to_string(),
            // ElevenLabs voices work across its model range; no binding.
            model: None,
        }))
    }

    fn list_cloned(&self) -> TtsResult<Vec<CloneHandle>> {
        let resp = self
            .client
            .get(Self::url(""))
            .header("xi-api-key", &self.api_key)
            .send()
            .map_err(|e| TtsError(format!("elevenlabs voice API: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(TtsError(format!("elevenlabs voice API {status}: {body}")));
        }
        let json: serde_json::Value = resp
            .json()
            .map_err(|e| TtsError(format!("elevenlabs voice API parse: {e}")))?;
        let voices = json
            .get("voices")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        // /v1/voices includes library and workspace voices; only
        // category=="cloned" entries are deletable enrollment voices.
        Ok(voices
            .iter()
            .filter(|v| v.get("category").and_then(|x| x.as_str()) == Some("cloned"))
            .filter_map(|v| {
                let id = v.get("voice_id").and_then(|x| x.as_str())?;
                Some(CloneHandle {
                    engine: "elevenlabs".into(),
                    voice_id: id.to_string(),
                    model: None,
                })
            })
            .collect())
    }

    fn delete_cloned(&self, handle: &CloneHandle) -> TtsResult<()> {
        let resp = self
            .client
            .delete(Self::url(&format!("/{}", handle.voice_id)))
            .header("xi-api-key", &self.api_key)
            .send()
            .map_err(|e| TtsError(format!("elevenlabs voice API: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(TtsError(format!("elevenlabs voice API {status}: {body}")));
        }
        Ok(())
    }
}
