//! Voice cloning: bank a voice once, enroll it with every cloning-capable
//! engine, speak it everywhere. Experimental, Rust-only (no FFI yet).
//!
//! The model is voice *banking*: a `VoiceIdentity` (reference clips +
//! optional transcripts) is captured or imported once — from an Apple
//! Personal Voice export ZIP, an LJSpeech-format corpus, or any recorder
//! — then [`trait VoiceCloning`]::`clone_voice` enrolls it per engine
//! and returns a `CloneHandle` the synthesis engines already understand
//! (cloned voice ids are just voice strings; `tts_set_voice` needs no
//! change).
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use rust_tts_wrapper::cloning::{create_cloner, VoiceCorpus};
//!
//! let corpus = VoiceCorpus::from_personal_voice_zip("Will's Personal Voice 1 - Recordings.zip")?;
//! let identity = corpus.to_identity(Some("en"));
//! let cloner = create_cloner("qwen", r#"{"apiKey":"sk-..."}"#)
//!     .ok_or("no qwen cloner")?;
//! let handle = cloner.clone_voice(&identity)?;
//! println!("cloned: {} (model {})", handle.voice_id, handle.model.as_deref().unwrap_or("-"));
//! # Ok(())
//! # }
//! ```
//!
//! Design notes and the full provider matrix live in `VOICE_CLONING_PLAN.md`.

mod corpus;
mod elevenlabs;
mod qwen;
mod registry;

pub use corpus::VoiceCorpus;
pub use registry::CloneRegistry;

use crate::types::{TtsError, TtsResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// A banked voice: everything an enrollment API might need. Captured or
/// imported once, fanned out to every engine.
#[derive(Debug, Clone, Default)]
pub struct VoiceIdentity {
    /// Display name for the voice (used to derive provider voice-name
    /// prefixes where the provider restricts them, e.g. Qwen ≤10 alnum).
    pub name: String,
    /// Reference clips, canonical form: PCM16 little-endian, mono, at the
    /// clip's `sample_rate`. 10–20 s total is enough for every instant
    /// cloner; more is fine — engines select what they send.
    pub clips: Vec<AudioClip>,
    /// BCP-47 hint (e.g. `en`, `zh-CN`). Some providers require a
    /// language (Cartesia); others only use it for quality.
    pub language: Option<String>,
}

/// One reference recording.
#[derive(Debug, Clone)]
pub struct AudioClip {
    /// Clip label (e.g. the source filename).
    pub name: String,
    /// PCM16 LE mono bytes.
    pub pcm: Vec<u8>,
    /// Sample rate of `pcm` (Hz).
    pub sample_rate: u32,
    /// Transcript, when known. Always collect when you can: zipvoice
    /// needs exact ones, Resemble requires one per clip, Fish auto-ASRs
    /// them, the rest ignore them.
    pub transcript: Option<String>,
}

impl AudioClip {
    /// Duration in seconds (rounded down).
    #[must_use]
    pub fn duration_secs(&self) -> u32 {
        let bytes_per_sec = u64::from(self.sample_rate) * 2;
        u32::try_from(self.pcm.len() as u64 / bytes_per_sec).unwrap_or(u32::MAX)
    }
}

/// A provider-side handle to an enrolled voice. The `voice_id` is opaque
/// and must be replayed verbatim in synthesis (PlayHT's is an `s3://`
/// URL; Murf's is `cln_…`; never validate or parse it). `model` records
/// the provider-side model binding where voices are locked to a model
/// (Qwen `target_model`, Cartesia PVC snapshots, Murf falcon-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneHandle {
    /// Engine that owns this voice (e.g. `"qwen"`).
    pub engine: String,
    /// Opaque provider voice id — pass to `tts_set_voice` as-is.
    pub voice_id: String,
    /// Model the voice is bound to, when the provider binds voices to
    /// models. Synthesis must use this model or the provider rejects it.
    pub model: Option<String>,
}

/// How an engine produces cloned voices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloningMode {
    /// Enrollment is synchronous: `clone_voice` returns a ready handle.
    Instant,
    /// Enrollment is a job: `clone_voice` returns
    /// [`CloneOutcome::Pending`] and `poll_clone` resolves it.
    Job,
    /// No enrollment at all: the engine synthesizes zero-shot from the
    /// reference clips on every call (sherpa-onnx zipvoice/pocket style).
    ZeroShot,
}

/// Result of an enrollment call.
#[derive(Debug, Clone)]
pub enum CloneOutcome {
    /// The voice is ready to use now.
    Ready(CloneHandle),
    /// The provider accepted the request; poll with `poll_clone`.
    Pending { engine: String, job_id: String },
}

/// Optional per-engine voice cloning, mirroring [`crate::engine::TtsEngine`].
/// Engines that cannot clone simply don't implement it —
/// [`create_cloner`] returns `None` for them.
pub trait VoiceCloning: Send + Sync {
    /// The engine id (matches `create_engine` ids).
    fn engine_id(&self) -> &'static str;
    /// Whether this engine enrolls instantly, by job, or zero-shot.
    fn cloning_mode(&self) -> CloningMode;
    /// Enroll `identity` and return a ready handle or a job. Consent
    /// gates (Azure, Google ICV) are Phase 3 — not implemented yet.
    ///
    /// # Errors
    /// When the identity has no usable clips, the enrollment request
    /// fails (auth, quota, plan, audio constraints), or the provider
    /// response cannot be parsed.
    fn clone_voice(&self, identity: &VoiceIdentity) -> TtsResult<CloneOutcome>;
    /// Resolve a [`CloneOutcome::Pending`] job. Engines that never
    /// return `Pending` implement the default (an error).
    ///
    /// # Errors
    /// When the engine has no job workflow, or polling fails.
    fn poll_clone(&self, _job_id: &str) -> TtsResult<CloneOutcome> {
        Err(TtsError(format!(
            "{} cloning never returns pending jobs",
            self.engine_id()
        )))
    }
    /// List voices enrolled under the caller's account.
    ///
    /// # Errors
    /// When the listing request fails or the response cannot be parsed.
    fn list_cloned(&self) -> TtsResult<Vec<CloneHandle>>;
    /// Delete an enrolled voice (providers count them against quotas).
    ///
    /// # Errors
    /// When the delete request fails.
    fn delete_cloned(&self, handle: &CloneHandle) -> TtsResult<()>;
}

/// Create a voice-cloning client for a cloning-capable engine.
///
/// `credentials_json` uses the same credential keys as `create_engine`
/// for the same engine id. Returns `None` for engines without cloning
/// support (or when the `cloning` feature is off).
#[must_use]
pub fn create_cloner(engine_id: &str, credentials_json: &str) -> Option<Arc<dyn VoiceCloning>> {
    let creds: std::collections::HashMap<String, String> = if credentials_json.is_empty() {
        std::collections::HashMap::new()
    } else {
        serde_json::from_str(credentials_json).unwrap_or_default()
    };
    match engine_id {
        #[cfg(feature = "cloning")]
        "qwen" => Some(Arc::new(qwen::QwenCloner::new(&creds))),
        #[cfg(feature = "cloning")]
        "elevenlabs" => Some(Arc::new(elevenlabs::ElevenLabsCloner::new(&creds))),
        _ => None,
    }
}

/// Which engines in this build support cloning.
#[must_use]
pub fn cloning_engines() -> Vec<&'static str> {
    let mut ids = Vec::new();
    #[cfg(feature = "cloning")]
    {
        ids.push("qwen");
        ids.push("elevenlabs");
    }
    ids
}

/// Select the clips to send an enrollment API: longest-first until
/// `target_secs` of audio is reached (always at least one clip). Engines'
/// windows differ (Qwen 10–20 s, ElevenLabs 1–2 min); sending everything
/// is wasteful and some providers degrade past their window.
#[must_use]
pub(crate) fn select_clips(clips: &[AudioClip], target_secs: u32) -> Vec<&AudioClip> {
    let mut sorted: Vec<&AudioClip> = clips.iter().collect();
    sorted.sort_by_key(|c| std::cmp::Reverse(c.duration_secs()));
    let mut out = Vec::new();
    let mut total = 0u32;
    for clip in sorted {
        if !out.is_empty() && total >= target_secs {
            break;
        }
        total += clip.duration_secs().max(1);
        out.push(clip);
    }
    out
}

/// Concatenate clips into one continuous PCM stream with short silence
/// gaps between them (providers allow ≤2 s pauses; 0.4 s reads as natural
/// sentence spacing). All clips must share a sample rate.
pub(crate) fn concat_clips(clips: &[&AudioClip], gap_ms: u32) -> TtsResult<(Vec<u8>, u32)> {
    let Some(first) = clips.first() else {
        return Err(TtsError("no clips to concatenate".into()));
    };
    let rate = first.sample_rate;
    if clips.iter().any(|c| c.sample_rate != rate) {
        return Err(TtsError(
            "clips must share a sample rate to concatenate".into(),
        ));
    }
    let gap_bytes = (u64::from(rate) * 2 * u64::from(gap_ms) / 1000) as usize;
    let mut pcm = Vec::new();
    for (i, clip) in clips.iter().enumerate() {
        if i > 0 {
            pcm.resize(pcm.len() + gap_bytes, 0);
        }
        pcm.extend_from_slice(&clip.pcm);
    }
    Ok((pcm, rate))
}

/// Mux PCM16 LE mono into a WAV container (44-byte RIFF header).
#[must_use]
pub(crate) fn wav_bytes(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let mut wav = Vec::with_capacity(44 + pcm.len());
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * 2;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// Default registry location: `~/.rust-tts-wrapper/clones.json`.
#[must_use]
pub fn default_registry_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(
        PathBuf::from(home)
            .join(".rust-tts-wrapper")
            .join("clones.json"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(secs: u32) -> AudioClip {
        AudioClip {
            name: format!("c{secs}"),
            pcm: vec![0; secs as usize * 48_000],
            sample_rate: 24_000,
            transcript: None,
        }
    }

    #[test]
    fn select_clips_prefers_longest_and_caps() {
        let clips = vec![clip(4), clip(9), clip(6)];
        let picked = select_clips(&clips, 10);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].name, "c9");
        assert_eq!(picked[1].name, "c6");
        // Always at least one, even past target.
        let one = select_clips(&clips, 0);
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn concat_clips_inserts_gap_bytes() {
        let a = clip(1);
        let b = clip(1);
        let (pcm, rate) = concat_clips(&[&a, &b], 400).unwrap();
        assert_eq!(rate, 24_000);
        // 2×1 s clips (48 000 B/s) + 0.4 s gap (19 200 B).
        assert_eq!(pcm.len(), 48_000 * 2 + 19_200);
    }

    #[test]
    fn concat_clips_rejects_mixed_rates() {
        let mut a = clip(1);
        a.sample_rate = 22_050;
        let b = clip(1);
        assert!(concat_clips(&[&a, &b], 400).is_err());
    }

    #[test]
    fn wav_bytes_has_well_formed_header() {
        let wav = wav_bytes(&[0xAB, 0xCD], 24_000);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 2);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
    }
}
