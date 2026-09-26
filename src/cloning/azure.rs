//! Azure AI Speech Personal Voices.
//!
//! Three-step REST under the Speech resource (`Ocp-Apim-Subscription-Key`):
//!
//! 1. **Consent** — `PUT {endpoint}/customvoice/consents/{id}` (multipart:
//!    projectId, voiceTalentName, companyName, locale, audiodata). The
//!    verbal statement must be Azure's fixed per-locale script, and the
//!    spoken names must match the fields — the service verifies the
//!    speaker.
//! 2. **Personal voice** — `POST {endpoint}/customvoice/personalvoices/{id}`
//!    (multipart: projectId, consentId, audiodata WAV 5–90 s). Returns
//!    `Operation-Location` for a long-running operation.
//! 3. **Poll** the operation until `Succeeded`; the created resource
//!    carries the `speakerProfileId` used for synthesis.
//!
//! Synthesis needs SSML `<mstts:ttsembedding speakerProfileId=…>` on a
//! base model voice (`DragonLatestNeural`…) — the azure engine accepts a
//! `"{base_model}/{speaker_profile_id}"` voice string for that.
//!
//! **Intake-gated**: Personal Voice API access requires Microsoft's
//! registration form (aka.ms/customneural). Code works; un-gated
//! subscriptions fail at consent creation. Request shapes verified
//! against the official REST docs 2026-09-26 (not exercised live — no
//! gated subscription available).

use super::{
    select_clips, wav_bytes, CloneHandle, CloneOutcome, CloningMode, ConsentRecording, ConsentSpec,
    VoiceCloning, VoiceIdentity,
};
use crate::types::{TtsError, TtsResult};
use std::collections::HashMap;

/// Azure prompt audio: 5–90 s.
const AZURE_TARGET_SECS: u32 = 60;

/// en-US consent script (verbatim — Azure verifies the statement).
pub(crate) const AZURE_CONSENT_SCRIPT_EN_US: &str = "I [state your first and last name] am aware \
     that recordings of my voice will be used by [state the name of the company] to create and \
     use a synthetic version of my voice.";

pub(crate) struct AzureCloner {
    api_key: String,
    region: String,
    credentials: HashMap<String, String>,
    client: reqwest::blocking::Client,
}

impl AzureCloner {
    pub(crate) fn new(credentials: &HashMap<String, String>) -> Self {
        Self {
            api_key: credentials
                .get("apiKey")
                .or_else(|| credentials.get("subscriptionKey"))
                .cloned()
                .unwrap_or_default(),
            region: credentials
                .get("region")
                .cloned()
                .unwrap_or_else(|| "eastus".into()),
            credentials: credentials.clone(),
            client: reqwest::blocking::Client::new(),
        }
    }

    fn base(&self) -> String {
        format!("https://{}.api.cognitive.microsoft.com", self.region)
    }

    fn api_version(&self) -> String {
        self.credentials
            .get("apiVersion")
            .cloned()
            .unwrap_or_else(|| "2026-01-01".into())
    }

    fn consent_for(identity: &VoiceIdentity) -> Option<&ConsentRecording> {
        identity.consent.iter().find(|c| c.engine == "azure")
    }

    fn auth(&self) -> (&'static str, String) {
        ("Ocp-Apim-Subscription-Key", self.api_key.clone())
    }

    /// PUT the consent resource (multipart per Consents_Post).
    fn put_consent(
        &self,
        consent: &ConsentRecording,
        resource_id: &str,
        project_id: &str,
    ) -> TtsResult<()> {
        let talent = consent
            .metadata
            .get("voiceTalentName")
            .ok_or_else(|| TtsError("azure cloning: consent needs voiceTalentName".into()))?;
        let company = consent
            .metadata
            .get("companyName")
            .ok_or_else(|| TtsError("azure cloning: consent needs companyName".into()))?;
        let locale = consent
            .metadata
            .get("locale")
            .ok_or_else(|| TtsError("azure cloning: consent needs locale".into()))?;
        let form = reqwest::blocking::multipart::Form::new()
            .text("projectId", project_id.to_string())
            .text("voiceTalentName", talent.clone())
            .text("companyName", company.clone())
            .text("locale", locale.clone())
            .part(
                "audiodata",
                reqwest::blocking::multipart::Part::bytes(wav_bytes(
                    &consent.pcm,
                    consent.sample_rate,
                ))
                .file_name("consent.wav")
                .mime_str("audio/wav")
                .map_err(|e| TtsError(format!("mime: {e}")))?,
            );
        let (header, value) = self.auth();
        let url = format!(
            "{}/customvoice/consents/{}?api-version={}",
            self.base(),
            resource_id,
            self.api_version()
        );
        let resp = self
            .client
            .post(&url)
            .header(header, value)
            .multipart(form)
            .send()
            .map_err(|e| TtsError(format!("azure consent: {e}")))?;
        let status = resp.status();
        // 201 created / 200 existing-and-identical are both fine.
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(TtsError(format!("azure consent {status}: {body}")));
        }
        Ok(())
    }

    /// POST the personal voice (multipart per PersonalVoices_Post);
    /// returns the Operation-Location URL.
    fn post_personal_voice(
        &self,
        resource_id: &str,
        project_id: &str,
        consent_id: &str,
        identity: &VoiceIdentity,
    ) -> TtsResult<String> {
        let picked = select_clips(&identity.clips, AZURE_TARGET_SECS);
        let (pcm, rate) = super::concat_clips(&picked, 300)?;
        // 5–90 s window.
        #[allow(clippy::cast_possible_truncation)]
        let max_pcm = (rate as usize).saturating_mul(2 * 90);
        let pcm = if pcm.len() > max_pcm {
            pcm[..max_pcm].to_vec()
        } else {
            pcm
        };
        if (pcm.len() as u64) < u64::from(rate) * 2 * 5 {
            return Err(TtsError("azure cloning: prompt audio must be ≥5 s".into()));
        }
        let form = reqwest::blocking::multipart::Form::new()
            .text("projectId", project_id.to_string())
            .text("consentId", consent_id.to_string())
            .part(
                "audiodata",
                reqwest::blocking::multipart::Part::bytes(wav_bytes(&pcm, rate))
                    .file_name("prompt.wav")
                    .mime_str("audio/wav")
                    .map_err(|e| TtsError(format!("mime: {e}")))?,
            );
        let (header, value) = self.auth();
        let url = format!(
            "{}/customvoice/personalvoices/{}?api-version={}",
            self.base(),
            resource_id,
            self.api_version()
        );
        let resp = self
            .client
            .post(&url)
            .header(header, value)
            .multipart(form)
            .send()
            .map_err(|e| TtsError(format!("azure personal voice: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(TtsError(format!("azure personal voice {status}: {body}")));
        }
        let op = resp
            .headers()
            .get("Operation-Location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| TtsError("azure personal voice: no Operation-Location header".into()))?;
        Ok(op)
    }

    /// Poll the LRO once; Ok(Some(handle)) on success, Ok(None) while
    /// running, Err on failure.
    fn poll_once(&self, operation_url: &str) -> TtsResult<Option<CloneHandle>> {
        let (header, value) = self.auth();
        let resp = self
            .client
            .get(operation_url)
            .header(header, value)
            .send()
            .map_err(|e| TtsError(format!("azure poll: {e}")))?;
        let status = resp.status();
        let json: serde_json::Value = resp
            .json()
            .map_err(|e| TtsError(format!("azure poll parse ({status}): {e}")))?;
        let op_status = json
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match op_status {
            "Succeeded" => {
                let profile = json
                    .pointer("/properties/result/speakerProfileId")
                    .or_else(|| json.pointer("/result/speakerProfileId"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        TtsError(format!(
                            "azure poll: succeeded but no speakerProfileId: {json}"
                        ))
                    })?;
                Ok(Some(CloneHandle {
                    engine: "azure".into(),
                    // Synthesis voice string: "{base_model}/{profile}" —
                    // the azure engine splits this for ttsembedding.
                    voice_id: format!("DragonLatestNeural/{profile}"),
                    model: Some("DragonLatestNeural".into()),
                }))
            }
            "Failed" => {
                let err = json
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                Err(TtsError(format!("azure personal voice failed: {err}")))
            }
            _ => Ok(None),
        }
    }
}

impl VoiceCloning for AzureCloner {
    fn engine_id(&self) -> &'static str {
        "azure"
    }

    fn cloning_mode(&self) -> CloningMode {
        CloningMode::Job
    }

    fn consent_spec(&self) -> Option<ConsentSpec> {
        Some(ConsentSpec {
            script: AZURE_CONSENT_SCRIPT_EN_US.into(),
            locale: "en-US".into(),
            metadata_keys: vec!["voiceTalentName", "companyName", "locale"],
        })
    }

    fn clone_voice(&self, identity: &VoiceIdentity) -> TtsResult<CloneOutcome> {
        let project_id = self
            .credentials
            .get("projectId")
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                TtsError(
                    "azure cloning: projectId credential required (a Custom Voice \
                     project from Speech Studio)"
                        .into(),
                )
            })?;
        let Some(consent) = Self::consent_for(identity) else {
            return Err(TtsError(format!(
                "azure cloning: consent recording required — the user must read \
                 the {AZURE_CONSENT_SCRIPT_EN_US:?} script aloud, with metadata \
                 voiceTalentName/companyName/locale"
            )));
        };
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let consent_id = format!("rust-tts-consent-{suffix}");
        let voice_id = format!("rust-tts-pv-{suffix}");
        self.put_consent(consent, &consent_id, project_id)?;
        let operation = self.post_personal_voice(&voice_id, project_id, &consent_id, identity)?;
        // First poll immediately — training is documented as <5 s.
        if let Some(handle) = self.poll_once(&operation)? {
            return Ok(CloneOutcome::Ready(handle));
        }
        Ok(CloneOutcome::Pending {
            engine: "azure".into(),
            job_id: operation,
        })
    }

    fn poll_clone(&self, job_id: &str) -> TtsResult<CloneOutcome> {
        match self.poll_once(job_id)? {
            Some(handle) => Ok(CloneOutcome::Ready(handle)),
            None => Ok(CloneOutcome::Pending {
                engine: "azure".into(),
                job_id: job_id.to_string(),
            }),
        }
    }

    fn list_cloned(&self) -> TtsResult<Vec<CloneHandle>> {
        let (header, value) = self.auth();
        let url = format!(
            "{}/customvoice/personalvoices?api-version={}",
            self.base(),
            self.api_version()
        );
        let resp = self
            .client
            .get(&url)
            .header(header, value)
            .send()
            .map_err(|e| TtsError(format!("azure list: {e}")))?;
        let status = resp.status();
        let json: serde_json::Value = resp
            .json()
            .map_err(|e| TtsError(format!("azure list parse ({status}): {e}")))?;
        let Some(values) = json.get("value").and_then(|v| v.as_array()) else {
            return Ok(Vec::new());
        };
        Ok(values
            .iter()
            .filter_map(|v| {
                let profile = v.get("speakerProfileId").and_then(|x| x.as_str())?;
                Some(CloneHandle {
                    engine: "azure".into(),
                    voice_id: format!("DragonLatestNeural/{profile}"),
                    model: Some("DragonLatestNeural".into()),
                })
            })
            .collect())
    }

    fn delete_cloned(&self, handle: &CloneHandle) -> TtsResult<()> {
        // The personal voice resource id is needed for DELETE; handles
        // carry speakerProfileId instead. Resolve via list.
        let (header, value) = self.auth();
        let list_url = format!(
            "{}/customvoice/personalvoices?api-version={}",
            self.base(),
            self.api_version()
        );
        let resp = self
            .client
            .get(&list_url)
            .header(header, value.clone())
            .send()
            .map_err(|e| TtsError(format!("azure delete lookup: {e}")))?;
        let json: serde_json::Value = resp
            .json()
            .map_err(|e| TtsError(format!("azure delete lookup parse: {e}")))?;
        let wanted = handle
            .voice_id
            .rsplit('/')
            .next()
            .unwrap_or(&handle.voice_id);
        let Some(values) = json.get("value").and_then(|v| v.as_array()) else {
            return Err(TtsError("azure delete: voice not found".into()));
        };
        let Some(entry) = values
            .iter()
            .find(|v| v.get("speakerProfileId").and_then(|x| x.as_str()) == Some(wanted))
        else {
            return Err(TtsError("azure delete: voice not found".into()));
        };
        let Some(id) = entry.get("id").and_then(|x| x.as_str()) else {
            return Err(TtsError("azure delete: resource has no id".into()));
        };
        let url = format!(
            "{}/customvoice/personalvoices/{}?api-version={}",
            self.base(),
            id,
            self.api_version()
        );
        let resp = self
            .client
            .delete(&url)
            .header(header, value)
            .send()
            .map_err(|e| TtsError(format!("azure delete: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(TtsError(format!("azure delete {status}: {body}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_script_is_pinned() {
        assert!(AZURE_CONSENT_SCRIPT_EN_US.contains("synthetic version of my voice"));
    }

    #[test]
    fn consent_gate_and_project_required() {
        let cloner = AzureCloner::new(&HashMap::new());
        let identity = VoiceIdentity::default();
        let err = cloner.clone_voice(&identity).unwrap_err().to_string();
        assert!(err.contains("projectId"), "{err}");

        let mut creds = HashMap::new();
        creds.insert("projectId".to_string(), "p1".to_string());
        let cloner = AzureCloner::new(&creds);
        let err = cloner.clone_voice(&identity).unwrap_err().to_string();
        assert!(err.contains("consent recording required"), "{err}");
    }

    #[test]
    fn job_mode_with_consent_spec() {
        let cloner = AzureCloner::new(&HashMap::new());
        assert_eq!(cloner.cloning_mode(), CloningMode::Job);
        let spec = cloner.consent_spec().expect("gated");
        assert!(spec.metadata_keys.contains(&"voiceTalentName"));
    }
}
