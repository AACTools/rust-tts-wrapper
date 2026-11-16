//! The omnivoice-local `ModelSource` implementation (#102 Phase 2): one catalogue row (the published
//! GGUF pair from `Serveurperso/OmniVoice-GGUF`), a TWO-FILE install —
//! the base GGUF (the entry's url/sha256) plus the codec GGUF (the
//! registry's new `extra_files`, every byte checksum-gated) — into
//! `models_dir/<id>/`, and voices for the installed set.

use crate::local_models::install::{
    atomic_swap, fetch_verified, preflight_disk, with_install_lock, InstallManifest, UreqFetcher,
    MANIFEST_NAME,
};
use crate::local_models::{
    CancelFn, CatalogueVoice, InstallError, ModelEntry, ModelSource, ProgressFn,
};
use std::path::PathBuf;

/// The singleton source.
pub(crate) static OMNIVOICE_SOURCE: OmnivoiceModelSource = OmnivoiceModelSource;

pub(crate) struct OmnivoiceModelSource;

/// The engine's models dir: `TTS_MODELS_DIR` override then
/// `~/.rust-tts-wrapper/omnivoice-local`.
fn models_dir() -> PathBuf {
    if let Some(base) = std::env::var_os("TTS_MODELS_DIR") {
        return PathBuf::from(base).join("omnivoice-local");
    }
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|k| std::env::var_os(k).map(|h| (!h.is_empty()).then(|| PathBuf::from(h))))
        .flatten()
        .map_or_else(
            || PathBuf::from(".omnivoice-local"),
            |h| h.join(".rust-tts-wrapper").join("omnivoice-local"),
        )
}

/// Installed when the model dir holds the base GGUF (the file the
/// entry's url names) — the codec is fetched alongside, so one
/// implies both.
fn installed_entry(id: &str, m: &sherpa_onnx_models::ModelInfo) -> bool {
    let dir = models_dir().join(id);
    let base_name = m.url.rsplit('/').next().unwrap_or_default();
    dir.join(base_name).is_file() || dir.join(MANIFEST_NAME).is_file()
}

/// The registry rows this engine serves: `engines` naming
/// omnivoice-local.
fn registry_rows() -> Vec<(&'static str, &'static sherpa_onnx_models::ModelInfo)> {
    sherpa_onnx_models::models()
        .iter()
        .filter(|(_, m)| m.engines.split(',').any(|e| e.trim() == "omnivoice-local"))
        .map(|(id, m)| (id.as_str(), m))
        .collect()
}

fn entry_for(id: &str, m: &sherpa_onnx_models::ModelInfo) -> ModelEntry {
    let installed = installed_entry(id, m);
    let has_sha = m.sha256.as_deref().is_some_and(|s| !s.is_empty());
    let update_available = InstallManifest::read_from(&models_dir().join(id)).is_some_and(|man| {
        m.sha256
            .as_deref()
            .is_some_and(|reg| !reg.eq_ignore_ascii_case(&man.sha256))
    });
    let availability = crate::local_models::simple_availability(crate::local_models::ModelFacts {
        installed,
        deprecated: m.deprecated,
        has_url: !m.url.is_empty(),
        has_sha256: has_sha,
    });
    ModelEntry {
        engine: "omnivoice-local",
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
        // download total = base + every extra file (SUM — matches the
        // install preflight; loop-2 review caught a max here)
        filesize_mb: m.filesize_mb + m.extra_files.iter().map(|f| f.size_mb).sum::<f64>(),
        license: m.license.clone(),
        installed,
        update_available,
        experimental: false,
        availability,
    }
}

impl ModelSource for OmnivoiceModelSource {
    fn engine_id(&self) -> &'static str {
        "omnivoice-local"
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
        if !dir.exists() {
            return Err(InstallError::ModelNotFound);
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|e| InstallError::InUse(format!("cannot remove {}: {e}", dir.display())))?;
        let root = models_dir();
        std::fs::remove_dir_all(root.join(format!(".staging-{id}"))).ok();
        for name in part_names(id) {
            std::fs::remove_file(root.join(name)).ok();
        }
        Ok(())
    }

    /// Adopt an existing GGUF pair at `models_dir/<id>` (#106): both
    /// files must be present (the same pair `resolve_installed_files`
    /// requires), then the neutral manifest.
    fn adopt(&self, id: &str) -> Result<(), InstallError> {
        if registry_rows().into_iter().all(|(rid, _)| rid != id) {
            return Err(InstallError::ModelNotFound);
        }
        if resolve_installed_files(id).is_none() {
            return Err(InstallError::UnsupportedUrl(format!(
                "the base and codec GGUFs must both be present under {}",
                models_dir().join(id).display()
            )));
        }
        let (_, m) = registry_rows()
            .into_iter()
            .find(|(rid, _)| *rid == id)
            .expect("checked above");
        InstallManifest {
            id: id.to_string(),
            engine: "omnivoice-local".to_string(),
            sha256: m.sha256.clone().unwrap_or_default(),
            files: Vec::new(),
            installed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
        .write_to(&models_dir().join(id))?;
        Ok(())
    }

    fn models_dir(&self) -> PathBuf {
        models_dir()
    }

    fn voices(&self) -> Vec<CatalogueVoice> {
        let base = models_dir();
        registry_rows()
            .into_iter()
            .filter(|(id, m)| installed_entry(id, m) || base.join(id).join(MANIFEST_NAME).is_file())
            .map(|(id, m)| CatalogueVoice {
                engine: "omnivoice-local",
                // the engine addresses voices by voice-design string /
                // language per speak(); the catalogue voice id selects
                // the installed model (model id), leaving design to
                // the engine's own per-call selectors
                voice: "default".to_string(),
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

/// Resolve an installed model id to its (base, codec) file paths —
/// the factory's modelId fallback (#102). `None` when the registry
/// row or the installed files are absent (the caller falls back to
/// the explicit-path error).
#[must_use]
pub(crate) fn resolve_installed_files(id: &str) -> Option<(String, String)> {
    let (_, m) = registry_rows().into_iter().find(|(rid, _)| *rid == id)?;
    let dir = models_dir().join(id);
    let base_name = m.url.rsplit('/').next()?;
    let codec_name = m.extra_files.first()?.name.as_str();
    let base = dir.join(base_name);
    let codec = dir.join(codec_name);
    if !base.is_file() || !codec.is_file() {
        return None;
    }
    Some((
        base.to_string_lossy().into_owned(),
        codec.to_string_lossy().into_owned(),
    ))
}

/// Part-file names for an install attempt (base + every extra), at
/// the models-dir root so interrupted downloads resume.
fn part_names(id: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Some((_, m)) = registry_rows().into_iter().find(|(rid, _)| *rid == id) {
        // SAME derivation as fetch_one: the base part is named after
        // the URL's last segment (loop-1 review: a mismatched constant
        // here leaked the largest file uninstall could not clean)
        let base_name = m.url.rsplit('/').next().unwrap_or_default();
        if !base_name.is_empty() {
            names.push(format!(".{id}-{base_name}.part"));
        }
        for f in &m.extra_files {
            names.push(format!(".{id}-{}.part", f.name));
        }
    }
    names
}

/// The locked install: base GGUF + every `extra_files` entry — each
/// sha256-gated (multi-file GGUF models have no small sidecars; every
/// byte is verified, #102 rule) — then the neutral manifest and an
/// atomic swap.
#[allow(clippy::cast_precision_loss)] // progress fractions
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
    let Some(base_sha) = m.sha256.as_deref().filter(|s| !s.is_empty()) else {
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
    // every extra file must carry its own digest — no unverifiable bytes
    if m.extra_files.iter().any(|f| f.sha256.is_empty()) {
        return Err(InstallError::UnsupportedUrl(
            "registry entry has an extra file without sha256".into(),
        ));
    }

    let extras_mb: f64 = m.extra_files.iter().map(|f| f.size_mb).sum();
    let total_mb = m.filesize_mb + extras_mb;
    let dir = models_dir();
    preflight_disk(&dir, (total_mb * 1_000_000.0).round() as u64 * 2)?;

    let staging = dir.join(format!(".staging-{id}"));
    std::fs::remove_dir_all(&staging).ok();
    std::fs::create_dir_all(&staging).map_err(|e| InstallError::Io("staging mkdir".into(), e))?;

    let mut files: Vec<String> = Vec::new();
    // the base file, then every extra under the same URL directory
    let base_name = m.url.rsplit('/').next().unwrap_or("model.gguf");
    let url_dir = &m.url[..m.url.rsplit_once('/').map_or(0, |(d, _)| d.len())];
    let mut specs = vec![FetchSpec {
        url: m.url.clone(),
        name: base_name,
        sha256: base_sha,
        size_mb: m.filesize_mb,
    }];
    for f in &m.extra_files {
        specs.push(FetchSpec {
            url: format!("{url_dir}/{}", f.name),
            name: &f.name,
            sha256: &f.sha256,
            size_mb: f.size_mb,
        });
    }
    // size-weighted bands: fetch_verified emits its own absolute
    // 0→0.74 scale per file — map it into this file's share of the
    // whole download so the overall stream is monotonic (the loop-1
    // sherpa bug class)
    let total_bytes: f64 = specs.iter().map(|s| s.size_mb).sum::<f64>().max(1.0);
    let mut done_bytes = 0.0f64;
    for spec in &specs {
        let band_start = 0.02 + 0.93 * done_bytes / total_bytes;
        let band_end = 0.02 + 0.93 * (done_bytes + spec.size_mb) / total_bytes;
        let banded =
            |f: f64, stage: &str| progress(band_start + (band_end - band_start) * f, stage);
        fetch_one(id, spec, &dir, &staging, &banded, cancel)?;
        files.push(spec.name.to_string());
        done_bytes += spec.size_mb;
    }

    let manifest = InstallManifest {
        id: id.to_string(),
        engine: "omnivoice-local".to_string(),
        sha256: base_sha.to_string(),
        files,
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    manifest.write_to(&staging)?;
    atomic_swap(&staging, &dir.join(id))?;
    progress(1.0, "done");
    Ok(())
}

/// One file of a multi-file entry.
struct FetchSpec<'a> {
    url: String,
    name: &'a str,
    sha256: &'a str,
    size_mb: f64,
}

/// One checksum-gated fetch: `.part` at the models-dir root (resume),
/// rename into staging under its real name.
#[allow(clippy::too_many_arguments)]
fn fetch_one(
    id: &str,
    spec: &FetchSpec<'_>,
    dir: &std::path::Path,
    staging: &std::path::Path,
    progress: ProgressFn<'_>,
    cancel: CancelFn<'_>,
) -> Result<(), InstallError> {
    fetch_one_with(&UreqFetcher, id, spec, dir, staging, progress, cancel)
}

/// Injectable-fetcher core so tests pin the multi-file loop offline.
#[allow(clippy::too_many_arguments)]
fn fetch_one_with(
    fetcher: &dyn crate::local_models::install::HttpFetch,
    id: &str,
    spec: &FetchSpec<'_>,
    dir: &std::path::Path,
    staging: &std::path::Path,
    progress: ProgressFn<'_>,
    cancel: CancelFn<'_>,
) -> Result<(), InstallError> {
    let part = dir.join(format!(".{id}-{}.part", spec.name));
    fetch_verified(
        fetcher,
        &spec.url,
        spec.sha256,
        &part,
        (spec.size_mb * 1_000_000.0).round() as u64,
        progress,
        cancel,
    )?;
    std::fs::rename(&part, staging.join(spec.name))
        .map_err(|e| InstallError::Io("part rename".into(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sources() must list omnivoice in EVERY feature combination that
    /// enables it — the pairing-build drop was the loop-1 must-fix
    /// (proven by a probe, not caught by any compiler error). This
    /// compiles under whichever combo the suite runs.
    #[test]
    fn sources_lists_omnivoice_when_compiled_in() {
        let ids: Vec<&str> = crate::local_models::sources()
            .iter()
            .map(|s| s.engine_id())
            .collect();
        assert!(ids.contains(&"omnivoice-local"), "sources: {ids:?}");
        // no duplicates
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate sources: {ids:?}");
    }

    /// The factory's modelId fallback: staged GGUF pair resolves to
    /// (modelPath, codecPath); absent files resolve to None.
    #[test]
    fn resolve_installed_files_needs_both_files() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = crate::local_models::test_util::env_lock();
        let prior = std::env::var_os("TTS_MODELS_DIR");
        std::env::set_var("TTS_MODELS_DIR", tmp.path());
        let dir = models_dir().join("omnivoice-base-q8_0");
        std::fs::create_dir_all(&dir).unwrap();
        // neither file → None
        assert!(resolve_installed_files("omnivoice-base-q8_0").is_none());
        // base only → None (the codec gate holds)
        std::fs::write(dir.join("omnivoice-base-Q8_0.gguf"), b"x").unwrap();
        assert!(resolve_installed_files("omnivoice-base-q8_0").is_none());
        // both → Some with the exact names
        std::fs::write(dir.join("omnivoice-tokenizer-F32.gguf"), b"x").unwrap();
        let (base, codec) = resolve_installed_files("omnivoice-base-q8_0").expect("resolves");
        assert!(base.ends_with("omnivoice-base-Q8_0.gguf"), "{base}");
        assert!(codec.ends_with("omnivoice-tokenizer-F32.gguf"), "{codec}");
        match prior {
            Some(p) => std::env::set_var("TTS_MODELS_DIR", p),
            None => std::env::remove_var("TTS_MODELS_DIR"),
        }
    }

    /// part_names matches what fetch_one can actually leave behind (the
    /// loop-1 base-name mismatch leaked 656 MB).
    #[test]
    fn part_names_cover_every_fetched_file() {
        let names = part_names("omnivoice-base-q8_0");
        assert!(names.len() >= 2, "{names:?}");
        assert!(
            names.iter().any(|n| n.contains("omnivoice-base-Q8_0.gguf")),
            "the BASE part is named after the URL segment: {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|n| n.contains("omnivoice-tokenizer-F32.gguf")),
            "every extra file has a part name: {names:?}"
        );
    }
}
