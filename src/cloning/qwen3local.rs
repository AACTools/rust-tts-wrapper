//! Zero-shot cloner for the local `qwen3-local` engine: extracts an
//! ECAPA speaker embedding from the identity's best clip once, and
//! returns it as the handle — the engine synthesizes with
//! `qwen3_tts_synthesize_with_embedding`, skipping re-encoding on every
//! utterance.
//!
//! "Cloning" here is pure local computation (no upload, no quota, no
//! consent gate) — the reference audio never leaves the machine.

use super::{
    select_clips, wav_bytes, CloneHandle, CloneOutcome, CloningMode, VoiceCloning, VoiceIdentity,
};
use crate::qwen3_local_engine::Qwen3LocalEngine;
use crate::qwen3_local_support::encode_embedding;
use crate::types::{TtsError, TtsResult};
use std::collections::HashMap;

pub(crate) struct Qwen3LocalCloner {
    engine: Qwen3LocalEngine,
}

impl Qwen3LocalCloner {
    pub(crate) fn new(credentials: &HashMap<String, String>) -> TtsResult<Self> {
        Ok(Self {
            engine: Qwen3LocalEngine::new(credentials)?,
        })
    }
}

impl VoiceCloning for Qwen3LocalCloner {
    fn engine_id(&self) -> &'static str {
        "qwen3-local"
    }

    fn cloning_mode(&self) -> CloningMode {
        CloningMode::ZeroShot
    }

    fn clone_voice(&self, identity: &VoiceIdentity) -> TtsResult<CloneOutcome> {
        let picked = select_clips(&identity.clips, 10);
        let Some(clip) = picked.first() else {
            return Err(TtsError(
                "qwen3-local cloning: identity has no clips".into(),
            ));
        };
        // The C API extracts embeddings from a WAV path: materialise the
        // clip in the system temp dir (deleted after). pid + nanos keeps
        // concurrent clone_voice calls from racing on the file name.
        let wav = wav_bytes(&clip.pcm, clip.sample_rate);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rust-tts-qwen3-emb-{}-{nanos}.wav",
            std::process::id()
        ));
        std::fs::write(&path, &wav).map_err(|e| TtsError(format!("write temp reference: {e}")))?;
        let result = self.engine.extract_embedding(&path.to_string_lossy());
        let _ = std::fs::remove_file(&path);
        let embedding = result?;
        Ok(CloneOutcome::Ready(CloneHandle {
            engine: "qwen3-local".into(),
            // The handle IS the embedding (opaque to everyone else;
            // only the qwen3-local engine interprets the emb: prefix).
            voice_id: encode_embedding(&embedding),
            model: None,
        }))
    }

    fn list_cloned(&self) -> TtsResult<Vec<CloneHandle>> {
        // Embeddings live in the caller's registry; nothing server-side.
        Ok(Vec::new())
    }

    fn delete_cloned(&self, _handle: &CloneHandle) -> TtsResult<()> {
        // Dropping the registry entry is the whole lifecycle.
        Ok(())
    }
}
