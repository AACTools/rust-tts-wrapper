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
        // ICL FIRST (#145-grade upgrade): a transcripted clip clones
        // in-context — accent and prosody are preserved, not just
        // timbre. The reference WAV must PERSIST (it is re-read every
        // utterance): it lands in a per-identity cache slot under the
        // user's cache dir, so handles stay valid across restarts.
        if let Some(transcript) = clip.transcript.as_ref().filter(|t| !t.is_empty()) {
            let dir = icl_cache_dir()?;
            std::fs::create_dir_all(&dir)
                .map_err(|e| TtsError(format!("create {}: {e}", dir.display())))?;
            let slot = dir.join(format!(
                "{}.wav",
                stable_identity_key(&identity.name, &clip.pcm)
            ));
            let wav = wav_bytes(&clip.pcm, clip.sample_rate);
            std::fs::write(&slot, &wav)
                .map_err(|e| TtsError(format!("write {}: {e}", slot.display())))?;
            return Ok(CloneOutcome::Ready(CloneHandle {
                engine: "qwen3-local".into(),
                // the engine's ICL convention: path + transcript
                voice_id: format!("icl:{}|{transcript}", slot.display()),
                model: None,
            }));
        }
        // No transcript: x-vector embedding (timbre-only) — the C API
        // extracts from a WAV path, so materialise the clip in the
        // temp dir (deleted after; pid + nanos avoids races).
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

/// The persistent ICL reference cache: `~/.cache/rust-tts-wrapper/
/// qwen3-icl` (HOME → USERPROFILE, the #123 resolution).
fn icl_cache_dir() -> TtsResult<std::path::PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .ok_or_else(|| TtsError("qwen3-local ICL cache: no home directory".into()))?;
    Ok(std::path::PathBuf::from(home)
        .join(".cache")
        .join("rust-tts-wrapper")
        .join("qwen3-icl"))
}

/// A stable, filesystem-safe cache key for one (identity, clip)
/// pair: identity name + a byte-hash of the PCM. Same clip → same
/// slot → the cache converges instead of accumulating.
fn stable_identity_key(name: &str, pcm: &[u8]) -> String {
    fn hash(bytes: &[u8]) -> u64 {
        // FNV-1a: deterministic, no dependency
        let mut h = 0xcbf2_9ce4_8422_2325_u64;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        h
    }
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    format!("{safe}-{:016x}", hash(pcm))
}
