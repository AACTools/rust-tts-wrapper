//! The floravox `ModelSource` implementation (#102 Phase 1): catalogue from the shared embedded
//! registry (entries whose `engines` field names floravox — the
//! student tier plus the floravox-compatible patched piper voices),
//! single-file installs through the SHARED machinery, and an
//! `experimental: true` flag on every entry — the engine itself is
//! under active development; the model surface is stable.

use crate::local_models::install::{
    atomic_swap, fetch_verified, preflight_disk, with_install_lock, InstallManifest, UreqFetcher,
    MANIFEST_NAME,
};
use crate::local_models::{
    Availability, CancelFn, CatalogueVoice, InstallError, ModelEntry, ModelSource, ProgressFn,
};
use std::path::PathBuf;

/// The singleton source.
pub(crate) static FLORAVOX_SOURCE: FloravoxModelSource = FloravoxModelSource;

pub(crate) struct FloravoxModelSource;

/// The engine's models dir (one resolution: `TTS_MODELS_DIR` override
/// then the engine default — see floravox_engine::default_models_dir).
fn models_dir() -> PathBuf {
    crate::floravox_engine::default_models_dir()
}

/// A voice is installed when the engine can resolve it: a
/// `models_dir/<id>.onnx` file or a `models_dir/<id>/` directory
/// holding one (mirrors FloravoxEngine::resolve_model).
fn dir_has_voice(dir: &std::path::Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let n = e.file_name();
        let Some(n) = n.to_str() else { return false };
        n.to_ascii_lowercase().ends_with(".onnx") || n == MANIFEST_NAME
    })
}

/// One registry entry as a floravox catalogue row. Availability is
/// direct (no bundle tiers): installed → ready; registry-complete →
/// needs_download; otherwise blocked with the parameterized reason.
fn entry_for(id: &str, m: &sherpa_onnx_models::ModelInfo) -> ModelEntry {
    let dir = models_dir().join(id);
    // mirrors the engine's resolve_model: the directory layout, or a
    // flat `models_dir/<id>.onnx` file (a manual install)
    let installed = dir_has_voice(&dir) || models_dir().join(format!("{id}.onnx")).is_file();
    let update_available = InstallManifest::read_from(&models_dir().join(id)).is_some_and(|man| {
        m.sha256
            .as_deref()
            .is_some_and(|reg| !reg.eq_ignore_ascii_case(&man.sha256))
    });
    let has_sha = m.sha256.as_deref().is_some_and(|s| !s.is_empty());
    let availability = if installed {
        Availability::Ready
    } else if m.deprecated {
        Availability::Blocked {
            reason: "deprecated".into(),
        }
    } else if m.url.is_empty() {
        Availability::Blocked {
            reason: "no-url".into(),
        }
    } else if !has_sha {
        Availability::Blocked {
            reason: "no-sha256".into(),
        }
    } else {
        Availability::NeedsDownload
    };
    ModelEntry {
        engine: "floravox",
        id: id.to_string(),
        name: m.name.clone(),
        model_type: m.model_type.clone(),
        quality: m.quality.clone(),
        languages: m
            .language
            .iter()
            .map(|l| (l.lang_code.clone(), l.language_name.clone()))
            .collect(),
        num_speakers: m.num_speakers,
        sample_rate: m.sample_rate,
        filesize_mb: m.filesize_mb,
        license: m.license.clone(),
        installed,
        update_available,
        // the engine is under active development (#102): features
        // work, the model surface is stable, UIs may badge the tier
        experimental: true,
        availability,
    }
}

/// The registry rows this engine serves: every entry whose `engines`
/// field names floravox (students + the floravox-compatible patched
/// piper voices).
fn registry_rows() -> Vec<(&'static str, &'static sherpa_onnx_models::ModelInfo)> {
    sherpa_onnx_models::models()
        .iter()
        .filter(|(_, m)| m.engines.split(',').any(|e| e.trim() == "floravox"))
        .map(|(id, m)| (id.as_str(), m))
        .collect()
}

impl ModelSource for FloravoxModelSource {
    fn engine_id(&self) -> &'static str {
        "floravox"
    }

    fn list(&self) -> Vec<ModelEntry> {
        registry_rows()
            .into_iter()
            .map(|(id, m)| entry_for(id, m))
            .collect()
    }

    fn entry(&self, id: &str) -> Option<ModelEntry> {
        registry_rows()
            .into_iter()
            .find(|(rid, _)| *rid == id)
            .map(|(rid, m)| entry_for(rid, m))
    }

    fn install(
        &self,
        id: &str,
        progress: ProgressFn<'_>,
        cancel: CancelFn<'_>,
    ) -> Result<(), InstallError> {
        with_install_lock(|| install_locked(id, progress, cancel)).and_then(std::convert::identity)
    }

    fn uninstall(&self, id: &str) -> Result<(), InstallError> {
        let dir = models_dir().join(id);
        if !dir.exists() && !models_dir().join(format!("{id}.onnx")).exists() {
            return Err(InstallError::ModelNotFound);
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|e| InstallError::InUse(format!("cannot remove {}: {e}", dir.display())))?;
        let root = models_dir();
        std::fs::remove_file(root.join(format!("{id}.onnx"))).ok();
        std::fs::remove_dir_all(root.join(format!(".staging-{id}"))).ok();
        std::fs::remove_file(root.join(format!(".{id}.part"))).ok();
        Ok(())
    }

    fn models_dir(&self) -> PathBuf {
        models_dir()
    }

    fn voices(&self) -> Vec<CatalogueVoice> {
        let base = models_dir();
        registry_rows()
            .into_iter()
            .filter(|(id, _)| {
                dir_has_voice(&base.join(id)) || base.join(format!("{id}.onnx")).is_file()
            })
            .map(|(id, m)| CatalogueVoice {
                engine: "floravox",
                // a floravox voice selector IS the model id/stem
                voice: id.to_string(),
                model: id.to_string(),
                name: m.name.clone(),
                language_codes: m
                    .language
                    .iter()
                    .map(|l| {
                        (
                            l.lang_code.clone(),
                            crate::types::locale_display_name(&l.lang_code),
                        )
                    })
                    .collect(),
            })
            .collect()
    }
}

/// The locked single-file install: every floravox registry url is a
/// raw `.onnx` (no archives), installed as `models_dir/<id>/<url
/// filename>` — the engine's directory voice layout, leaving room for
/// sibling `.onnx.json` / `.student` sidecars later.
fn install_locked(
    id: &str,
    progress: ProgressFn<'_>,
    cancel: CancelFn<'_>,
) -> Result<(), InstallError> {
    let Some(m) = registry_rows()
        .into_iter()
        .find(|(rid, _)| *rid == id)
        .map(|(_, m)| m)
    else {
        return Err(InstallError::ModelNotFound);
    };
    let Some(sha256) = m.sha256.as_deref().filter(|s| !s.is_empty()) else {
        return Err(InstallError::UnsupportedUrl(
            "registry entry has no sha256 — unverifiable download".into(),
        ));
    };
    if m.url.is_empty() {
        return Err(InstallError::UnsupportedUrl(
            "registry entry has no url".into(),
        ));
    }
    if m.deprecated {
        return Err(InstallError::UnsupportedUrl(
            "registry entry is blocked (deprecated)".into(),
        ));
    }
    let dir = models_dir();
    preflight_disk(&dir, (m.filesize_mb * 1_000_000.0).round() as u64 * 2)?;
    let staging = dir.join(format!(".staging-{id}"));
    std::fs::remove_dir_all(&staging).ok();
    std::fs::create_dir_all(&staging).map_err(|e| InstallError::Io("staging mkdir".into(), e))?;

    let raw_name = m.url.rsplit('/').next().unwrap_or("voice.onnx");
    let part = dir.join(format!(".{id}.part"));
    progress(0.0, "download");
    fetch_verified(
        &UreqFetcher,
        &m.url,
        sha256,
        &part,
        (m.filesize_mb * 1_000_000.0).round() as u64,
        progress,
        cancel,
    )?;
    // the shared raw-file path — one implementation (loop-3 review B)
    let files = crate::local_models::install::extract_into(
        &part, None, raw_name, &staging, progress, cancel,
    )?;
    std::fs::remove_file(&part).ok();

    let manifest = InstallManifest {
        id: id.to_string(),
        engine: "floravox".to_string(),
        sha256: sha256.to_string(),
        files,
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    manifest.write_to(&staging)?;
    // flat legacy file (if a previous manual install dropped one)
    std::fs::remove_file(dir.join(format!("{id}.onnx"))).ok();
    atomic_swap(&staging, &dir.join(id))?;
    progress(1.0, "done");
    Ok(())
}
