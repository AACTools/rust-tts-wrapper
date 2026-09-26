//! Qwen voice cloning via the DashScope `voice-enrollment` customization
//! API — the flavor bound to Qwen-Audio-TTS / CosyVoice synthesis models
//! (the same models our `qwen` engine speaks).
//!
//! Endpoint: `POST {base}/api/v1/services/audio/tts/customization` with
//! an action-based body. Verified live 2026-09-26: creation is instant,
//! free, and accepts a base64 data-URI in the `url` field (the docs say
//! "public URL"; the data URI works and avoids hosting the caller's
//! voice anywhere).

use super::{
    concat_clips, select_clips, wav_bytes, CloneHandle, CloneOutcome, CloningMode, VoiceCloning,
    VoiceIdentity,
};
use crate::types::{TtsError, TtsResult};
use base64::Engine as _;
use std::collections::HashMap;

const DEFAULT_TARGET_MODEL: &str = "qwen-audio-3.0-tts-flash";

/// Enroll-window target: Qwen recommends 10–20 s (60 s max, ≤10 MB).
const QWEN_TARGET_SECS: u32 = 20;

pub(crate) struct QwenCloner {
    api_key: String,
    credentials: HashMap<String, String>,
    client: reqwest::blocking::Client,
}

impl QwenCloner {
    pub(crate) fn new(credentials: &HashMap<String, String>) -> Self {
        Self {
            api_key: credentials
                .get("apiKey")
                .or_else(|| credentials.get("subscriptionKey"))
                .cloned()
                .unwrap_or_default(),
            credentials: credentials.clone(),
            client: reqwest::blocking::Client::new(),
        }
    }

    /// REST customization base, mirroring the speak path's WS selection:
    /// `customizationUrl` override > `region` (qwencloud | intl |
    /// beijing).
    fn endpoint(&self) -> TtsResult<String> {
        if let Some(url) = self
            .credentials
            .get("customizationUrl")
            .filter(|u| !u.is_empty())
        {
            return Ok(url.clone());
        }
        match self.credentials.get("region").map(String::as_str) {
            None | Some("qwencloud" | "") => Ok("https://maas.qwencloudapi.com".into()),
            Some("intl") => Ok("https://dashscope-intl.aliyuncs.com".into()),
            Some("beijing") => Ok("https://dashscope.aliyuncs.com".into()),
            Some(other) => Err(TtsError(format!(
                "qwen: unknown region '{other}' (expected qwencloud, intl, or beijing; \
                 or set customizationUrl to override the endpoint)"
            ))),
        }
    }

    /// POST an action to the customization endpoint and return the JSON.
    fn post_action(&self, input: &serde_json::Value) -> TtsResult<serde_json::Value> {
        let url = format!(
            "{}/api/v1/services/audio/tts/customization",
            self.endpoint()?
        );
        let body = serde_json::json!({ "model": "voice-enrollment", "input": input });
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("bearer {}", self.api_key))
            .json(&body)
            .send()
            .map_err(|e| TtsError(format!("qwen voice API: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| TtsError(format!("qwen voice API read: {e}")))?;
        if !status.is_success() {
            return Err(TtsError(format!("qwen voice API {status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| TtsError(format!("qwen voice API parse: {e}")))
    }

    fn target_model(&self) -> String {
        self.credentials
            .get("modelId")
            .filter(|m| !m.is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_TARGET_MODEL.into())
    }
}

/// Qwen prefixes: ≤10 alphanumeric characters.
fn qwen_prefix(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(10)
        .collect::<String>()
        .to_lowercase();
    if cleaned.is_empty() {
        "voice".into()
    } else {
        cleaned
    }
}

impl VoiceCloning for QwenCloner {
    fn engine_id(&self) -> &'static str {
        "qwen"
    }

    fn cloning_mode(&self) -> CloningMode {
        CloningMode::Instant
    }

    fn clone_voice(&self, identity: &VoiceIdentity) -> TtsResult<CloneOutcome> {
        if identity.clips.is_empty() {
            return Err(TtsError("qwen cloning: identity has no clips".into()));
        }
        // One URL of 10–20 s: longest clips first, 0.4 s gaps.
        let picked = select_clips(&identity.clips, QWEN_TARGET_SECS);
        let (pcm, rate) = concat_clips(&picked, 400)?;
        if rate < 16_000 {
            return Err(TtsError(format!(
                "qwen cloning: clips must be ≥16 kHz (got {rate} Hz)"
            )));
        }
        let wav = wav_bytes(&pcm, rate);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
        if b64.len() > 10 * 1024 * 1024 {
            return Err(TtsError(format!(
                "qwen cloning: enrollment audio too large ({} MB base64; \
                 cap is 10 MB / 60 s)",
                b64.len() / (1024 * 1024)
            )));
        }
        let target_model = self.target_model();
        let input = serde_json::json!({
            "action": "create_voice",
            "target_model": target_model,
            "prefix": qwen_prefix(&identity.name),
            // Data URI (verified working): keeps the caller's voice off
            // any third-party file host.
            "url": format!("data:audio/wav;base64,{b64}"),
        });
        let json = self.post_action(&input)?;
        let voice_id = json
            .get("output")
            .and_then(|o| o.get("voice_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| TtsError(format!("qwen cloning: no voice_id in response: {json}")))?;
        Ok(CloneOutcome::Ready(CloneHandle {
            engine: "qwen".into(),
            voice_id: voice_id.to_string(),
            model: Some(target_model),
        }))
    }

    fn list_cloned(&self) -> TtsResult<Vec<CloneHandle>> {
        let mut handles = Vec::new();
        let mut page = 0u32;
        loop {
            let json = self.post_action(&serde_json::json!({
                "action": "list_voice",
                "page_index": page,
                "page_size": 100,
            }))?;
            let output = json.get("output").cloned().unwrap_or_default();
            let list = output
                .get("voice_list")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for v in &list {
                let Some(id) = v.get("voice_id").and_then(|x| x.as_str()) else {
                    continue;
                };
                handles.push(CloneHandle {
                    engine: "qwen".into(),
                    voice_id: id.to_string(),
                    model: v
                        .get("target_model")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                });
            }
            let total = output
                .get("total_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if u64::from(page) * 100 + list.len() as u64 >= total || list.is_empty() {
                break;
            }
            page += 1;
        }
        Ok(handles)
    }

    fn delete_cloned(&self, handle: &CloneHandle) -> TtsResult<()> {
        self.post_action(&serde_json::json!({
            "action": "delete_voice",
            "voice_id": handle.voice_id,
        }))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_sanitized_alnum() {
        assert_eq!(qwen_prefix("Will's Personal Voice 1"), "willsperso");
        assert_eq!(qwen_prefix("Ó"), "voice");
        assert_eq!(qwen_prefix("Ab12Cd34Ef45X"), "ab12cd34ef");
    }

    #[test]
    fn endpoint_regions() {
        let mut c = HashMap::new();
        let cloner = QwenCloner::new(&c);
        assert_eq!(cloner.endpoint().unwrap(), "https://maas.qwencloudapi.com");
        c.insert("region".into(), "intl".into());
        assert_eq!(
            QwenCloner::new(&c).endpoint().unwrap(),
            "https://dashscope-intl.aliyuncs.com"
        );
        c.insert("region".into(), "Beijing".into());
        assert!(QwenCloner::new(&c).endpoint().is_err());
        c.insert(
            "customizationUrl".into(),
            "https://proxy.example.com".into(),
        );
        assert_eq!(
            QwenCloner::new(&c).endpoint().unwrap(),
            "https://proxy.example.com"
        );
    }
}
