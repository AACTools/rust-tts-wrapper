//! The sherpa-onnx `ModelSource` implementation (#102 Phase 1): catalogue from the embedded registry,
//! install state from the models dir, installs through the shared
//! machinery (resume, preflight, checksum gate, atomic swap, neutral
//! `.installed` manifest), post-install G2P bundle provisioning via
//! the SAME provisioner the FFI ABI uses — no replica download paths.

use crate::local_models::install::{
    archive_kind, atomic_swap, fetch_verified, preflight_disk, with_install_lock, InstallManifest,
    UreqFetcher, MANIFEST_NAME,
};
use crate::local_models::{
    Availability, CancelFn, CatalogueVoice, InstallError, ModelEntry, ModelSource, ProgressFn,
};
use std::path::PathBuf;

/// The singleton source (referenced as a `&'static dyn` by
/// [`crate::local_models::sources`]).
pub(crate) static SHERPA_SOURCE: SherpaModelSource = SherpaModelSource;

pub(crate) struct SherpaModelSource;

/// The engine's model directory: `TTS_MODELS_DIR` overrides the base
/// (sandbox/Flatpak), then the engine's own default (which honors the
/// same env — one resolution, no replica).
fn models_dir() -> PathBuf {
    crate::sherpaonnx_engine::default_model_dir()
}

/// Does a candidate model dir contain a model? Any `.onnx` file or an
/// install manifest counts — the engine's own setup does the full
/// file-layout validation when it actually opens the model.
fn dir_has_model(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut has_onnx = false;
    let mut has_manifest = false;
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == MANIFEST_NAME {
            has_manifest = true;
        }
        if name.to_ascii_lowercase().ends_with(".onnx") {
            has_onnx = true;
        }
    }
    has_onnx || has_manifest
}

#[allow(clippy::unused_self)] // trait shape: sources are stateless singletons
impl ModelSource for SherpaModelSource {
    fn engine_id(&self) -> &'static str {
        "sherpaonnx"
    }

    fn list(&self) -> Vec<ModelEntry> {
        sherpa_onnx_models::models()
            .iter()
            // ONLY rows this engine can drive: the shared registry also
            // carries floravox-student rows (engines: "floravox") that
            // the sherpa vits config cannot load — listing them here
            // would install unusable voices (routing hazard, loop-3
            // review C). Shared rows ("sherpa-onnx,floravox") stay.
            .filter(|(_, m)| m.engines.split(',').any(|e| e.trim() == "sherpa-onnx"))
            .map(|(id, m)| {
                let info = crate::sherpa_registry_info(m);
                let installed = dir_has_model(&models_dir().join(id.as_str()));
                let has_sha = m.sha256.as_deref().is_some_and(|s| !s.is_empty());
                let readiness = crate::sherpa_catalog::readiness_for(&info, has_sha, m.deprecated);
                let manifest = if installed {
                    InstallManifest::read_from(&models_dir().join(id.as_str()))
                } else {
                    None
                };
                let update_available = manifest.is_some_and(|man| {
                    m.sha256
                        .as_deref()
                        .is_some_and(|reg| !reg.eq_ignore_ascii_case(&man.sha256))
                });
                let availability = if installed || readiness.starts_with("blocked:") {
                    Availability::from_readiness(&readiness)
                } else if has_sha {
                    // files absent, registry complete → installable now
                    Availability::NeedsDownload
                } else {
                    // MMS-style entries short-circuit to `unverified`
                    // before the sha check in readiness; an entry
                    // without a digest is not verifiably installable
                    Availability::Blocked {
                        reason: "no-sha256".into(),
                    }
                };
                ModelEntry {
                    engine: "sherpaonnx",
                    id: id.clone(),
                    name: m.name.clone(),
                    model_type: m.model_type.clone(),
                    quality: m.quality.clone(),
                    languages: info
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
                    experimental: false,
                    availability,
                }
            })
            .collect()
    }

    fn entry(&self, id: &str) -> Option<ModelEntry> {
        self.list().into_iter().find(|e| e.id == id)
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
        std::fs::remove_dir_all(&dir).map_err(|e| {
            // Windows: a live engine holds the files open
            InstallError::InUse(format!("cannot remove {}: {e}", dir.display()))
        })?;
        // staging/part siblings of a failed earlier attempt (archive
        // and MMS layouts each have their own part names)
        let root = models_dir();
        std::fs::remove_dir_all(root.join(format!(".staging-{id}"))).ok();
        std::fs::remove_file(root.join(format!(".{id}.part"))).ok();
        std::fs::remove_file(root.join(format!(".{id}-model.onnx.part"))).ok();
        std::fs::remove_file(root.join(format!(".{id}-tokens.txt.part"))).ok();
        Ok(())
    }

    fn models_dir(&self) -> PathBuf {
        models_dir()
    }

    fn voices(&self) -> Vec<CatalogueVoice> {
        let base = models_dir();
        sherpa_onnx_models::models()
            .iter()
            .filter(|(_, m)| m.engines.split(',').any(|e| e.trim() == "sherpa-onnx"))
            .filter(|(id, _)| dir_has_model(&base.join(id.as_str())))
            .flat_map(|(id, m)| {
                let info = crate::sherpa_registry_info(m);
                crate::sherpaonnx_engine::voices_for_info(&info)
                    .into_iter()
                    .map(move |v| CatalogueVoice {
                        engine: "sherpaonnx",
                        model: id.clone(),
                        voice: v.id,
                        name: v.name,
                        language_codes: v
                            .language_codes
                            .into_iter()
                            .map(|lc| (lc.bcp47, lc.display))
                            .collect(),
                    })
            })
            .collect()
    }
}

/// The locked install body (free fn — the source is a stateless
/// singleton).
fn install_locked(
    id: &str,
    progress: ProgressFn<'_>,
    cancel: CancelFn<'_>,
) -> Result<(), InstallError> {
    let Some((_, model)) = sherpa_onnx_models::models()
        .iter()
        .filter(|(_, m)| m.engines.split(',').any(|e| e.trim() == "sherpa-onnx"))
        .find(|(mid, _)| mid.as_str() == id)
    else {
        return Err(InstallError::ModelNotFound);
    };
    let Some(sha256) = model
        .sha256
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return Err(InstallError::UnsupportedUrl(format!(
            "registry entry '{id}' has no sha256 — unverifiable download"
        )));
    };
    let url = model.url.clone();
    if url.is_empty() {
        return Err(InstallError::UnsupportedUrl(
            "registry entry has no url".into(),
        ));
    }
    // respect the catalogue's own verdict: deprecated / no-bundle /
    // g2p-unavailable entries are not installable through this API
    let info = crate::sherpa_registry_info(model);
    let readiness = crate::sherpa_catalog::readiness_for(&info, true, model.deprecated);
    if readiness.starts_with("blocked:") {
        return Err(InstallError::UnsupportedUrl(format!(
            "registry entry is blocked ({readiness})"
        )));
    }
    let dir = models_dir();
    preflight_disk(&dir, preflight_wanted(&url, model.filesize_mb))?;

    let staging = dir.join(format!(".staging-{id}"));
    std::fs::remove_dir_all(&staging).ok();
    std::fs::create_dir_all(&staging).map_err(|e| InstallError::Io("staging mkdir".into(), e))?;

    let files = stage_files(
        id,
        &url,
        &sha256,
        model.filesize_mb,
        &staging,
        progress,
        cancel,
    )?;

    // post-install: the G2P bundle for validated locales — the
    // SAME provisioner the tts_provision_sherpa_g2p_bundle ABI
    // uses (no replica fetch path)
    #[cfg(feature = "sherpa-g2p")]
    if readiness == "needs_bundle" {
        progress(0.97, "post-install");
        if let Some(bundle) = crate::sherpa_lang::validated_bundle_for_model(&info) {
            // bundle provisioning is best-effort here: a failure
            // degrades availability to needs_bundle (the install
            // itself succeeded) and speak() retries the fetch on a
            // later utterance
            let _ = crate::sherpa_g2p::provision_bundle(bundle.archive, bundle.key);
        }
    }

    let manifest = InstallManifest {
        id: id.to_string(),
        engine: "sherpaonnx".to_string(),
        sha256,
        files,
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    manifest.write_to(&staging)?;
    let final_dir = dir.join(id);
    // atomic_swap alone handles an existing final_dir: it moves the
    // old copy aside and RESTORES it if the swap fails — deleting
    // here would defeat that back-out
    atomic_swap(&staging, &final_dir)?;
    progress(1.0, "done");
    Ok(())
}

/// HF `resolve/.../<lang>` directory urls (MMS): no file extension,
/// the files inside are fetched by convention.
fn is_hf_dir_url(url: &str) -> bool {
    url.contains("/resolve/")
        && !has_archive_ext(url)
        && !url.to_ascii_lowercase().ends_with(".onnx")
}

/// Fetch + extract the model's files into `staging` (archive, MMS
/// directory pair, or raw single file). Returns the staged file list
/// for the manifest.
fn stage_files(
    id: &str,
    url: &str,
    sha256: &str,
    filesize_mb: f64,
    staging: &std::path::Path,
    progress: ProgressFn<'_>,
    cancel: CancelFn<'_>,
) -> Result<Vec<String>, InstallError> {
    let mut files: Vec<String> = Vec::new();
    if is_hf_dir_url(url) {
        // MMS layout: the url addresses a DIRECTORY on HF — the
        // files are known by convention. The entry's sha256 IS
        // the model.onnx digest (that is what the checksum
        // campaign pinned); tokens.txt has no registry digest, so
        // it gets the size floor + content-shape validation
        // instead (documented deviation: the alternative — an
        // unverifiable gate — buys nothing for a ~300 byte text file).
        // parts live at the models-dir ROOT (outside staging, which
        // every attempt wipes) so interrupted MMS downloads resume
        let dir = models_dir();
        let part = dir.join(format!(".{id}-model.onnx.part"));
        progress(0.0, "download");
        fetch_verified(
            &UreqFetcher,
            &format!("{url}/model.onnx"),
            sha256,
            &part,
            0,
            progress,
            cancel,
        )?;
        std::fs::rename(&part, staging.join("model.onnx"))
            .map_err(|e| InstallError::Io("part rename".into(), e))?;
        files.push("model.onnx".to_string());

        progress(0.65, "download");
        let tokens_part = dir.join(format!(".{id}-tokens.txt.part"));
        fetch_small_text(&format!("{url}/tokens.txt"), &tokens_part, cancel)?;
        let tokens = std::fs::read_to_string(&tokens_part)
            .map_err(|e| InstallError::Io("tokens read".into(), e))?;
        if tokens.lines().count() < 10 {
            return Err(InstallError::Network(
                "tokens.txt does not look like a token table".into(),
            ));
        }
        std::fs::rename(&tokens_part, staging.join("tokens.txt"))
            .map_err(|e| InstallError::Io("part rename".into(), e))?;
        files.push("tokens.txt".to_string());
    } else {
        let part = models_dir().join(format!(".{id}.part"));
        let expected = (filesize_mb * 1_000_000.0).round() as u64;
        progress(0.0, "download");
        fetch_verified(&UreqFetcher, url, sha256, &part, expected, progress, cancel)?;
        let kind = archive_kind(&part)?;
        let raw_name = url.rsplit('/').next().unwrap_or("model.bin");
        files = crate::local_models::install::extract_into(
            &part, kind, raw_name, staging, progress, cancel,
        )?;
        std::fs::remove_file(&part).ok();
    }
    Ok(files)
}

// clippy suggests Path::extension here, but these are URL strings
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn has_archive_ext(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.ends_with(".tar.bz2") || lower.ends_with(".tar.gz") || lower.ends_with(".tgz")
}

/// Floor for the registry-digest-less tokens.txt fetch: real MMS
/// token tables are 290–606 bytes, so this only needs to clear the
/// 9-byte "Not Found" body class with margin.
const MIN_TOKENS_BYTES: u64 = 32;

/// Small text fetch without a registry digest (tokens.txt for MMS):
/// the size floor applies plus a content-shape check by the caller.
fn fetch_small_text(
    url: &str,
    dest: &std::path::Path,
    cancel: CancelFn<'_>,
) -> Result<(), InstallError> {
    use crate::local_models::install::{HttpFetch, UreqFetcher};
    let mut out = std::fs::File::create(dest)
        .map_err(|e| InstallError::Io("tokens part create".into(), e))?;
    let res = UreqFetcher.get_range(url, 0, &mut |chunk| {
        if cancel() {
            return Err("cancelled".into());
        }
        std::io::Write::write_all(&mut out, chunk).map_err(|e| format!("{e}"))
    });
    match res {
        Ok(_) => {}
        Err(e) if e == "cancelled" => return Err(InstallError::Cancelled),
        Err(e) => return Err(InstallError::Network(e)),
    }
    let len = dest.metadata().map_or(0, |m| m.len());
    if len < MIN_TOKENS_BYTES {
        std::fs::remove_file(dest).ok();
        return Err(InstallError::Network(format!(
            "tokens.txt implausibly small ({len} bytes)"
        )));
    }
    Ok(())
}

/// Disk needed before the first byte: the download plus expansion
/// headroom (bz2 archives roughly double on extraction).
fn preflight_wanted(url: &str, filesize_mb: f64) -> u64 {
    let dl = (filesize_mb * 1_000_000.0).round() as u64;
    if has_archive_ext(url) {
        dl.saturating_mul(3)
    } else if is_hf_dir_url(url) {
        // MMS pairs are ~109 MB/model with a 0 filesize in the
        // registry — a constant, documented floor
        260_000_000
    } else {
        dl.saturating_mul(2)
    }
}
