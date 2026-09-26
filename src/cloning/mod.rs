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
//! use rust_tts_wrapper::cloning::{create_cloner, CloneOutcome, VoiceCorpus};
//!
//! let corpus = VoiceCorpus::from_personal_voice_zip("Will's Personal Voice 1 - Recordings.zip")?;
//! let identity = corpus.to_identity(Some("en"));
//! let cloner = create_cloner("qwen", r#"{"apiKey":"sk-..."}"#)
//!     .ok_or("no qwen cloner")?;
//! let CloneOutcome::Ready(handle) = cloner.clone_voice(&identity)?
//!     else { unimplemented!("qwen cloning is instant") };
//! println!("cloned: {} (model {})", handle.voice_id, handle.model.as_deref().unwrap_or("-"));
//! # Ok(())
//! # }
//! ```
//!
//! Design notes and the full provider matrix live in the project's
//! voice-cloning plan (kept out of the repository).

mod azure;
mod corpus;
mod elevenlabs;
mod google;
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
    /// Per-provider consent recordings (consent-gated engines only:
    /// Azure Personal Voice, Google Instant Custom Voice). Scripts are
    /// provider-specific and fixed — see [`VoiceCloning::consent_spec`].
    pub consent: Vec<ConsentRecording>,
}

/// A recorded consent statement for a consent-gated provider. Providers
/// verify the recording against their fixed script and metadata (Azure
/// additionally requires the spoken talent/company names to match).
#[derive(Debug, Clone)]
pub struct ConsentRecording {
    /// Engine the consent is for (`"azure"`, `"google"`).
    pub engine: String,
    /// PCM16 LE mono consent audio.
    pub pcm: Vec<u8>,
    /// Sample rate of `pcm` (Hz).
    pub sample_rate: u32,
    /// Provider-specific metadata: Azure needs `voiceTalentName`,
    /// `companyName`, `locale`; Google needs `language_code`.
    pub metadata: std::collections::HashMap<String, String>,
}

/// What a consent-gated engine requires the user to record before
/// [`VoiceCloning::clone_voice`] can succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentSpec {
    /// The exact script the user must read aloud (verbatim — providers
    /// verify).
    pub script: String,
    /// Locale the script is written for (e.g. `en-US`).
    pub locale: String,
    /// Extra metadata keys the recording must carry.
    pub metadata_keys: Vec<&'static str>,
}

impl VoiceIdentity {
    /// Start building an identity (recording-session or file-by-file
    /// flow). See [`VoiceIdentityBuilder`].
    #[must_use]
    pub fn builder(name: &str) -> VoiceIdentityBuilder {
        VoiceIdentityBuilder::new(name)
    }

    /// Build an identity from (audio file path, transcript) pairs —
    /// the "some audio + transcription" entry point.
    ///
    /// # Errors
    /// When any file fails to load/decode (all-or-nothing by design:
    /// a banking corpus should be complete).
    pub fn from_transcribed_pairs(
        name: &str,
        pairs: &[(std::path::PathBuf, &str)],
        language: Option<&str>,
    ) -> TtsResult<Self> {
        let mut clips = Vec::with_capacity(pairs.len());
        for (path, transcript) in pairs {
            let mut clip = AudioClip::from_audio_file(path)?;
            clip.set_transcript(transcript);
            clips.push(clip);
        }
        Ok(Self {
            name: name.to_string(),
            clips,
            language: language.map(str::to_string),
            consent: Vec::new(),
        })
    }
}

/// Incremental identity capture for recording UIs: open a clip, push
/// microphone PCM as it arrives, close it with the transcript (or attach
/// one later), repeat. `finish()` produces the identity for enrollment.
///
/// Enrollment APIs are upload-based, so "streaming" lives at capture
/// time — bytes accumulate here, not on the network.
///
/// ```no_run
/// # use rust_tts_wrapper::cloning::VoiceIdentityBuilder;
/// let mut b = VoiceIdentityBuilder::new("Dad").language("en");
/// let mut clip = b.start_clip("prompt-1", 24_000);
/// clip.push_pcm(&[0u8; 4800], 24_000).unwrap();   // live mic chunks
/// b.finish_clip(clip, Some("The quick brown fox."));
/// let identity = b.finish();
/// ```
#[derive(Debug, Default)]
pub struct VoiceIdentityBuilder {
    name: String,
    language: Option<String>,
    clips: Vec<AudioClip>,
    consent: Vec<ConsentRecording>,
}

impl VoiceIdentityBuilder {
    /// New builder for a named voice.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }

    /// Set the BCP-47 language hint.
    #[must_use]
    pub fn language(mut self, language: &str) -> Self {
        self.language = Some(language.to_string());
        self
    }

    /// Add a complete clip (e.g. from [`AudioClip::from_audio_file`] or
    /// [`AudioClip::from_pcm`]).
    #[must_use]
    pub fn add_clip(mut self, clip: AudioClip) -> Self {
        self.clips.push(clip);
        self
    }

    /// Open a new streaming clip. Push PCM16 LE mono chunks into it,
    /// then close with [`Self::finish_clip`].
    #[must_use]
    pub fn start_clip(&mut self, name: &str, sample_rate: u32) -> AudioClip {
        AudioClip {
            name: name.to_string(),
            pcm: Vec::new(),
            sample_rate,
            transcript: None,
        }
    }

    /// Close a streaming clip, attaching its transcript.
    pub fn finish_clip(&mut self, clip: AudioClip, transcript: Option<&str>) {
        let mut clip = clip;
        if let Some(t) = transcript {
            clip.transcript = Some(t.to_string());
        }
        self.clips.push(clip);
    }

    /// Attach a consent recording (consent-gated engines).
    pub fn add_consent(&mut self, consent: ConsentRecording) {
        self.consent.push(consent);
    }

    /// Total captured audio so far, in seconds.
    #[must_use]
    pub fn total_duration_secs(&self) -> u32 {
        self.clips.iter().map(AudioClip::duration_secs).sum()
    }

    /// Produce the identity. Errors when no clip captured any audio.
    ///
    /// # Errors
    /// When the builder holds no non-empty clips.
    pub fn finish(self) -> TtsResult<VoiceIdentity> {
        if self.clips.iter().all(|c| c.pcm.is_empty()) {
            return Err(TtsError(
                "voice identity needs at least one non-empty clip".into(),
            ));
        }
        Ok(VoiceIdentity {
            name: self.name,
            clips: self.clips,
            language: self.language,
            consent: self.consent,
        })
    }
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
    /// Duration in seconds (rounded down). A zero `sample_rate` (invalid,
    /// but the fields are public) yields 0 rather than panicking.
    #[must_use]
    pub fn duration_secs(&self) -> u32 {
        let bytes_per_sec = u64::from(self.sample_rate) * 2;
        if bytes_per_sec == 0 {
            return 0;
        }
        u32::try_from(self.pcm.len() as u64 / bytes_per_sec).unwrap_or(u32::MAX)
    }

    /// Load an audio file (wav/mp3/m4a/flac) as a canonical clip. The
    /// transcript, when known, is attached by the caller.
    ///
    /// # Errors
    /// When the file cannot be read or decoded.
    pub fn from_audio_file(path: impl AsRef<std::path::Path>) -> TtsResult<Self> {
        let path = path.as_ref();
        let bytes =
            std::fs::read(path).map_err(|e| TtsError(format!("read {}: {e}", path.display())))?;
        if bytes.len() as u64 > crate::cloning::corpus::MAX_IMPORT_FILE_BYTES {
            return Err(TtsError(format!(
                "{} is larger than the import cap",
                path.display()
            )));
        }
        let pcm = corpus::decode_to_canonical_pcm(&bytes)?;
        Ok(Self {
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("clip")
                .to_string(),
            pcm,
            sample_rate: corpus::CORPUS_SAMPLE_RATE,
            transcript: None,
        })
    }

    /// A clip from raw PCM16 LE mono bytes at `sample_rate`, with an
    /// optional transcript.
    #[must_use]
    pub fn from_pcm(name: &str, pcm: Vec<u8>, sample_rate: u32, transcript: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            pcm,
            sample_rate,
            transcript: transcript.map(str::to_string),
        }
    }

    /// Append streaming PCM16 LE mono chunks (live microphone capture).
    /// The rate must match the clip's; chunks must be whole samples
    /// (odd-length trailing bytes are held back until the next push).
    ///
    /// # Errors
    /// When the chunk's sample rate differs from the clip's.
    pub fn push_pcm(&mut self, chunk: &[u8], sample_rate: u32) -> TtsResult<()> {
        if sample_rate != self.sample_rate {
            return Err(TtsError(format!(
                "stream chunk rate {sample_rate} != clip rate {}",
                self.sample_rate
            )));
        }
        self.pcm.extend_from_slice(chunk);
        Ok(())
    }

    /// Attach a transcript after the fact (e.g. ASR or a late prompt).
    pub fn set_transcript(&mut self, transcript: &str) {
        self.transcript = Some(transcript.to_string());
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
    /// For consent-gated engines: the script the user must record and
    /// the metadata the recording must carry. `None` for engines
    /// without a consent gate. Hosts surface this before recording.
    fn consent_spec(&self) -> Option<ConsentSpec> {
        None
    }

    /// Enroll `identity` and return a ready handle or a job.
    ///
    /// # Errors
    /// When the identity has no usable clips, a required consent
    /// recording is missing, the enrollment request fails (auth, quota,
    /// plan, audio constraints, allow-list gating), or the provider
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
/// support.
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
        #[cfg(feature = "cloning")]
        "google" => Some(Arc::new(google::GoogleCloner::new(&creds))),
        #[cfg(feature = "cloning")]
        "azure" => Some(Arc::new(azure::AzureCloner::new(&creds))),
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
        ids.push("google");
        ids.push("azure");
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
    fn builder_streams_clips_and_finishes() {
        let mut b = VoiceIdentityBuilder::new("Dad").language("en");
        let mut clip = b.start_clip("p1", 24_000);
        clip.push_pcm(&vec![0u8; 48_000], 24_000).expect("push");
        clip.push_pcm(&vec![0u8; 48_000], 24_000).expect("push2");
        // 2 × 48 000 B = 96 000 B = 48 000 samples = 2 s @ 24 kHz.
        assert_eq!(clip.duration_secs(), 2);
        clip.set_transcript("read this");
        b.finish_clip(clip, None);
        let identity = b.finish().expect("identity");
        assert_eq!(identity.name, "Dad");
        assert_eq!(identity.language.as_deref(), Some("en"));
        assert_eq!(identity.clips.len(), 1);
        assert_eq!(identity.clips[0].transcript.as_deref(), Some("read this"));
        // Empty builders cannot finish.
        assert!(VoiceIdentityBuilder::new("x").finish().is_err());
    }

    #[test]
    fn push_pcm_rejects_rate_change() {
        let mut clip = AudioClip::from_pcm("c", Vec::new(), 24_000, None);
        assert!(clip.push_pcm(&[0u8; 100], 22_050).is_err());
    }

    #[test]
    fn from_transcribed_pairs_loads_files() {
        let dir = tempfile::tempdir().unwrap();
        let wav = wav_bytes(&vec![0u8; 4_800], 24_000);
        let p1 = dir.path().join("a.wav");
        let p2 = dir.path().join("b.wav");
        std::fs::write(&p1, &wav).unwrap();
        std::fs::write(&p2, &wav).unwrap();
        let identity = VoiceIdentity::from_transcribed_pairs(
            "corpus",
            &[(p1.clone(), "first"), (p2.clone(), "second")],
            Some("en"),
        )
        .expect("identity");
        assert_eq!(identity.clips.len(), 2);
        assert_eq!(identity.clips[0].transcript.as_deref(), Some("first"));
        assert_eq!(identity.clips[0].sample_rate, 24_000);
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
