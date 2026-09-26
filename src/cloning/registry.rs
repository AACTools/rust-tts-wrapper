//! Caller-side registry of cloned-voice handles, persisted as JSON.

use super::CloneHandle;
use crate::types::{TtsError, TtsResult};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Maps banked-identity names → per-engine handles. JSON on disk:
/// `{ "Will's Personal Voice 1": [ { "engine": "qwen", ... } ] }`.
#[derive(Debug, Default)]
pub struct CloneRegistry {
    path: Option<PathBuf>,
    map: BTreeMap<String, Vec<CloneHandle>>,
}

impl CloneRegistry {
    /// Load from `path` (missing file → empty registry bound to `path`).
    ///
    /// # Errors
    /// When `path` exists but is not valid registry JSON, or cannot be
    /// read.
    pub fn load(path: impl AsRef<Path>) -> TtsResult<Self> {
        let path = path.as_ref().to_path_buf();
        let map = match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
                .map_err(|e| TtsError(format!("parse {}: {e}", path.display())))?,
            _ => BTreeMap::new(),
        };
        Ok(Self {
            path: Some(path),
            map,
        })
    }

    /// An in-memory registry with no persistence.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Record a handle for `identity` (an identical re-registration is
    /// a no-op).
    pub fn add(&mut self, identity: &str, handle: CloneHandle) {
        let list = self.map.entry(identity.to_string()).or_default();
        if !list.contains(&handle) {
            list.push(handle);
        }
    }

    /// Handles registered for `identity`.
    #[must_use]
    pub fn handles(&self, identity: &str) -> &[CloneHandle] {
        self.map.get(identity).map_or(&[], Vec::as_slice)
    }

    /// Drop one handle; true when it was found and removed.
    pub fn remove(&mut self, identity: &str, handle: &CloneHandle) -> bool {
        if let Some(list) = self.map.get_mut(identity) {
            let before = list.len();
            list.retain(|h| h != handle);
            return list.len() != before;
        }
        false
    }

    /// All registered identities.
    #[must_use]
    pub fn identities(&self) -> Vec<&str> {
        self.map.keys().map(String::as_str).collect()
    }

    /// Persist to the bound path (no-op for in-memory registries).
    ///
    /// # Errors
    /// When the registry directory cannot be created or the file cannot
    /// be written/serialized.
    pub fn save(&self) -> TtsResult<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| TtsError(format!("mkdir {}: {e}", parent.display())))?;
        }
        let text = serde_json::to_string_pretty(&self.map)
            .map_err(|e| TtsError(format!("serialize registry: {e}")))?;
        // Atomic write (temp + rename): a crash mid-save must never
        // leave a truncated clones.json behind — load() would fail
        // forever until manually deleted. The tmp name carries the pid
        // so concurrent saves don't clobber each other's temp file.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            TtsError(format!("write {}: {e}", tmp.display()))
        })?;
        let renamed = std::fs::rename(&tmp, path);
        if renamed.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        renamed.map_err(|e| TtsError(format!("rename to {}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(engine: &str, id: &str) -> CloneHandle {
        CloneHandle {
            engine: engine.into(),
            voice_id: id.into(),
            model: None,
        }
    }

    #[test]
    fn registry_roundtrip_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clones.json");
        let mut reg = CloneRegistry::load(&path).unwrap();
        reg.add("Will", h("qwen", "q-1"));
        reg.add("Will", h("elevenlabs", "e-1"));
        reg.save().unwrap();

        let reloaded = CloneRegistry::load(&path).unwrap();
        assert_eq!(reloaded.handles("Will").len(), 2);
        assert_eq!(reloaded.identities(), vec!["Will"]);

        let mut reg2 = CloneRegistry::load(&path).unwrap();
        assert!(reg2.remove("Will", &h("qwen", "q-1")));
        assert!(!reg2.remove("Will", &h("qwen", "q-1")));
        assert_eq!(reg2.handles("Will").len(), 1);
    }

    #[test]
    fn in_memory_registry_save_is_noop() {
        let mut reg = CloneRegistry::in_memory();
        reg.add("x", h("qwen", "q"));
        assert!(reg.save().is_ok());
    }
}
