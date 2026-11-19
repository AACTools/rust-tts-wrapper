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
                let availability = if installed {
                    if crate::sherpaonnx_engine::is_english_model_id(&id.to_ascii_lowercase())
                        && readiness == "needs_bundle"
                    {
                        // English: the #66/#67 auto-lexicon still
                        // speaks and the bundle is an upgrade (the
                        // install gate's own degradation story,
                        // mirrored here so an app gating on Ready
                        // keeps showing a working voice)
                        Availability::Ready
                    } else if readiness == "needs_bundle" {
                        // #106: files-on-disk ≠ ready-offline. A
                        // needs_bundle voice with the bundle cached IS
                        // ready; without it the files exist but the
                        // voice cannot speak offline yet.
                        #[cfg(feature = "sherpa-g2p")]
                        {
                            crate::sherpa_lang::validated_bundle_for_model(&info).map_or(
                                Availability::from_readiness(&readiness),
                                |b| {
                                    if crate::sherpa_g2p::bundle_cached(b.archive, b.key) {
                                        Availability::Ready
                                    } else {
                                        Availability::from_readiness(&readiness)
                                    }
                                },
                            )
                        }
                        #[cfg(not(feature = "sherpa-g2p"))]
                        {
                            Availability::from_readiness(&readiness)
                        }
                    } else {
                        Availability::from_readiness(&readiness)
                    }
                } else if readiness.starts_with("blocked:") {
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

    /// Adopt an existing `models_dir/<id>` directory (#106): the
    /// family's loadable-file check, then the neutral manifest from
    /// the registry digest. For apps migrating their own layouts —
    /// the app moves its verified files into place first; its own
    /// markers stay its own.
    fn adopt(&self, id: &str) -> Result<(), InstallError> {
        let Some((_, model)) = sherpa_onnx_models::models()
            .iter()
            .filter(|(_, m)| m.engines.split(',').any(|e| e.trim() == "sherpa-onnx"))
            .find(|(mid, _)| mid.as_str() == id)
        else {
            return Err(InstallError::ModelNotFound);
        };
        let dir = models_dir().join(id);
        if !dir_has_model(&dir) {
            return Err(InstallError::UnsupportedUrl(format!(
                "no loadable model files in {} — move the verified files in first",
                dir.display()
            )));
        }
        InstallManifest {
            id: id.to_string(),
            engine: "sherpaonnx".to_string(),
            // the CURRENT registry digest — adoption trusts the app's
            // verification of the bytes and pins the digest for later
            // update_available comparison (a registry bump after
            // adoption therefore reports update_available even for
            // bytes that were fine at adoption time); digest-less
            // entries adopt with an empty one (never "update")
            sha256: model.sha256.clone().unwrap_or_default(),
            files: Vec::new(), // legacy layouts vary; the loadable-file
            // check above is the adoption gate
            installed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
        .write_to(&dir)?;
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
    install_locked_with(&UreqFetcher, id, None, progress, cancel)
}

/// Injectable-fetcher core (the test seam — #106's regression test
/// drives the bundle gate offline with a synthetic archive whose
/// digest obviously differs from the registry's; `sha256_override`
/// stands in for the registry digest in that setup).
fn install_locked_with(
    fetcher: &dyn crate::local_models::install::HttpFetch,
    id: &str,
    sha256_override: Option<&str>,
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
    let Some(registry_sha) = model
        .sha256
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return Err(InstallError::UnsupportedUrl(format!(
            "registry entry '{id}' has no sha256 — unverifiable download"
        )));
    };
    let sha256 = sha256_override.map_or(registry_sha, str::to_string);
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
    // RETAINED STAGING (#106): a previous attempt that downloaded and
    // verified the model but failed the bundle gate keeps its staging
    // dir (manifest inside, digest-matched) — a retry skips the
    // download and re-attempts only the bundle
    let manifest = InstallManifest::read_from(&staging);
    let staged_already = manifest
        .as_ref()
        .is_some_and(|m| m.id == id && m.sha256.eq_ignore_ascii_case(&sha256));
    if staged_already {
        progress(0.75, "verify"); // bytes already verified
    } else {
        std::fs::remove_dir_all(&staging).ok();
        std::fs::create_dir_all(&staging)
            .map_err(|e| InstallError::Io("staging mkdir".into(), e))?;
        let mut files = stage_files(
            fetcher,
            id,
            &url,
            &sha256,
            model.filesize_mb,
            &staging,
            progress,
            cancel,
        )?;
        // extra_files (#119): companion assets the archive doesn't
        // carry (matcha's shared vocoders) — matcha-en's
        // needs_bundle flip depends on this fetch
        files.extend(stage_extra_files(
            fetcher,
            &url,
            &model.extra_files,
            &staging,
            progress,
            cancel,
        )?);
        // the manifest goes in BEFORE the bundle gate: it is the
        // retained-staging marker, and its digest pins the bytes
        InstallManifest {
            id: id.to_string(),
            engine: "sherpaonnx".to_string(),
            sha256: sha256.clone(),
            files,
            installed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
        .write_to(&staging)?;
    }

    // The pronunciation-bundle gate (#106): for needs_bundle locales
    // the bundle IS required for offline speech — an install that
    // swaps in without it reports success and then fails its first
    // offline utterance. Provision BEFORE the swap using the SAME
    // provisioner the ABI uses (no replica fetch path); on failure
    // keep the previous working install and the retained staging.
    // English degrades to the #66/#67 auto-lexicon (the catalog's
    // own feature-off story): a bundle failure there is a warning,
    // not an install failure.
    #[cfg(feature = "sherpa-g2p")]
    if readiness == "needs_bundle" {
        progress(0.97, "post-install");
        if let Some(bundle) = crate::sherpa_lang::validated_bundle_for_model(&info) {
            if let Err(e) = crate::sherpa_g2p::provision_bundle(bundle.archive, bundle.key) {
                // English VITS degrades to the #66/#67 auto-lexicon
                // (the codepoints path re-provisions at speak with a
                // retry) — a bundle failure there is a warning, not
                // an install failure. The other English-orphaned
                // families (kitten/kokoro/matcha-en) have NO
                // speak-time provisioning, so a transient fetch
                // failure must fail the install (review: a
                // "successful" install with Ready availability that
                // can never speak is the worse outcome).
                let id_lower = id.to_ascii_lowercase();
                let english_vits = crate::sherpaonnx_engine::is_english_model_id(&id_lower)
                    && model.model_type.eq_ignore_ascii_case("vits");
                if !english_vits {
                    return Err(InstallError::PronunciationBundle(e));
                }
                eprintln!(
                    "sherpa: G2P bundle for '{id}' unavailable ({e}) — the \
                     auto-lexicon path will be used"
                );
            }
        }
    }

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
/// Fetch a registry entry's `extra_files` co-located into the model
/// staging dir (#119: matcha's shared vocoders, which the archive
/// doesn't carry). Every byte sha-gated; the per-file `url` override
/// reaches the separate vocoder-models release (absent = the entry
/// url's directory + name, the omnivoice convention).
fn stage_extra_files(
    fetcher: &dyn crate::local_models::install::HttpFetch,
    url: &str,
    extras: &[sherpa_onnx_models::ExtraFile],
    staging: &std::path::Path,
    progress: ProgressFn<'_>,
    cancel: CancelFn<'_>,
) -> Result<Vec<String>, InstallError> {
    let mut names = Vec::new();
    for f in extras {
        if f.sha256.is_empty() {
            return Err(InstallError::UnsupportedUrl(format!(
                "an extra file '{}' has no sha256 — unverifiable download",
                f.name
            )));
        }
        let dir = url
            .rsplit_once('/')
            .map_or(String::new(), |(d, _)| format!("{d}/"));
        let furl = f.url.clone().unwrap_or_else(|| format!("{dir}{}", f.name));
        let part = staging.join(&f.name);
        fetch_verified(
            fetcher,
            &furl,
            &f.sha256,
            &part,
            (f.size_mb * 1_000_000.0) as u64,
            progress,
            cancel,
        )?;
        names.push(f.name.clone());
    }
    Ok(names)
}

/// Fetch + extract the model's files into `staging` (archive, MMS
/// directory pair, or raw single file).
// fetcher threading + the seven call params of the layout branches
#[allow(clippy::too_many_arguments)]
fn stage_files(
    fetcher: &dyn crate::local_models::install::HttpFetch,
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
            fetcher,
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
        fetch_verified(fetcher, url, sha256, &part, expected, progress, cancel)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_models::install::{HttpFetch, InstallManifest};

    /// A fake source serving a small but VALID tar.gz archive (the
    /// digest matches), for offline install-path tests.
    struct FakeArchiveHttp;

    /// Drop-guard env restore: a failed assert mid-test must not leak
    /// TTS_MODELS_DIR / VOICEGARDEN_LEXICON_DIR into other tests
    /// (loop-1 review #3 — restore-at-end leaked on panic).
    struct EnvGuard {
        models: Option<std::ffi::OsString>,
        vg: Option<std::ffi::OsString>,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.models.take() {
                Some(m) => std::env::set_var("TTS_MODELS_DIR", m),
                None => std::env::remove_var("TTS_MODELS_DIR"),
            }
            match self.vg.take() {
                Some(v) => std::env::set_var("VOICEGARDEN_LEXICON_DIR", v),
                None => std::env::remove_var("VOICEGARDEN_LEXICON_DIR"),
            }
        }
    }

    /// A fetcher that FAILS if asked for bytes: retained staging must
    /// satisfy the model half of a retry without any download.
    struct NoNetwork;

    impl HttpFetch for NoNetwork {
        fn get_range(
            &self,
            _url: &str,
            _from: u64,
            _sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<Option<u64>, String> {
            Err("network must not be touched: staging is retained".into())
        }
    }

    /// The synthetic (valid-layout, incompressible) tar.gz body —
    /// ONE generator shared by the fetcher and the digest helper so
    /// they can never drift apart.
    fn fake_archive_body() -> Vec<u8> {
        let mut tar_buf = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_buf);
            let mut state = 0x9E37_79B9_7F4A_7C15_u64;
            let model: Vec<u8> = (0..32_768u32)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect();
            let mut header = tar::Header::new_gnu();
            header.set_size(model.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(
                &mut header,
                "voice/model.onnx",
                std::io::Cursor::new(&model),
            )
            .unwrap();
            let mut h2 = tar::Header::new_gnu();
            h2.set_size(4);
            h2.set_mode(0o644);
            h2.set_cksum();
            tar.append_data(&mut h2, "voice/tokens.txt", std::io::Cursor::new(b"tok\n"))
                .unwrap();
            tar.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar_buf).unwrap();
        gz.finish().unwrap()
    }

    impl HttpFetch for FakeArchiveHttp {
        fn get_range(
            &self,
            _url: &str,
            from: u64,
            sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<Option<u64>, String> {
            if from > 0 {
                return Err("no ranges".into());
            }
            let body = fake_archive_body();
            sink(&body)?;
            Ok(Some(body.len() as u64))
        }
    }

    /// #106 item 1: a needs_bundle (French) install whose bundle
    /// provisioning fails must REPORT the failure, keep the previous
    /// install, and RETAIN the verified staging — and the retry (with
    /// the bundle now available) succeeds without re-downloading.
    #[cfg(feature = "sherpa-g2p")]
    #[test]
    fn bundle_gate_fails_then_retry_uses_retained_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = crate::local_models::test_util::env_lock();
        let _env_guard = EnvGuard {
            models: std::env::var_os("TTS_MODELS_DIR"),
            vg: std::env::var_os("VOICEGARDEN_LEXICON_DIR"),
        };
        std::env::set_var("TTS_MODELS_DIR", tmp.path());
        // an unusable bundle cache forces provisioning failure:
        // offline the archive fetch fails, online the write under a
        // FILE path fails — deterministic either way (loop-1 review:
        // the archive-manifest fetch is not injectable yet; the
        // outcome never depends on which path kills it)
        let bad_cache = tmp.path().join("bad-bundle-cache");
        std::fs::write(&bad_cache, b"not a directory").unwrap();
        std::env::set_var("VOICEGARDEN_LEXICON_DIR", &bad_cache);

        let id = "piper-fr_FR-siwis-medium";
        let err = install_with_fetcher(id, &FakeArchiveHttp, &fake_sha(), &|_, _| {}, &|| false)
            .expect_err("bundle gate must fail");
        assert!(matches!(err, InstallError::PronunciationBundle(_)), "{err}");
        // nothing swapped in: the final dir is absent
        assert!(!models_dir().join(id).exists(), "no partial install");
        // the staging is RETAINED with its manifest (the digest marker)
        let staging = models_dir().join(format!(".staging-{id}"));
        let manifest = InstallManifest::read_from(&staging).expect("retained staging");
        assert_eq!(manifest.id, id);

        // now a WORKING bundle cache — stamp-verified by the LOCAL
        // manifest, so provisioning succeeds with ZERO network
        let good_cache = tmp.path().join("good-bundle-cache");
        std::fs::create_dir_all(good_cache.join("fra")).unwrap();
        std::fs::write(good_cache.join("fra/fra.fst"), b"x").unwrap();
        std::fs::write(good_cache.join("fra/fra.pho"), b"x").unwrap();
        std::fs::write(good_cache.join("fra/.sha256"), b"feedface").unwrap();
        std::fs::write(
            good_cache.join("expanded.json"),
            r#"{"version":"t","languages":[{"lang":"fra","sha256":"feedface"}]}"#,
        )
        .unwrap();
        std::env::set_var("VOICEGARDEN_LEXICON_DIR", &good_cache);

        install_with_fetcher(id, &NoNetwork, &fake_sha(), &|_, _| {}, &|| false)
            .expect("retry succeeds from retained staging");
        assert!(models_dir().join(id).join("model.onnx").is_file());
    }

    /// install_locked with an injectable fetcher (the test seam —
    /// production paths pass UreqFetcher).
    fn install_with_fetcher(
        id: &str,
        fetcher: &dyn HttpFetch,
        sha: &str,
        progress: &dyn Fn(f64, &str),
        cancel: &dyn Fn() -> bool,
    ) -> Result<(), InstallError> {
        install_locked_with(fetcher, id, Some(sha), progress, cancel)
    }

    /// The synthetic archive's sha256 (the override standing in for
    /// the registry digest).
    fn fake_sha() -> String {
        use sha2::Digest as _;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut h = sha2::Sha256::new();
        h.update(fake_archive_body());
        let d = h.finalize();
        let mut s = String::with_capacity(d.len() * 2);
        for b in d {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0xf) as usize] as char);
        }
        s
    }

    /// #106 item 2: adoption of a legacy install — files present, no
    /// manifest → adopt() writes it; the catalog then reports the
    /// model installed; an empty dir refuses adoption.
    #[test]
    fn adopt_legacy_install_and_refuse_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = crate::local_models::test_util::env_lock();
        let prior = std::env::var_os("TTS_MODELS_DIR");
        std::env::set_var("TTS_MODELS_DIR", tmp.path());

        let id = "piper-en_US-amy-low";
        let dir = models_dir().join(id);
        // legacy layout: model files, NO manifest, maybe an app marker
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("en_US-amy-low.onnx"), b"legacy").unwrap();
        std::fs::write(dir.join("tokens.txt"), b"a 0\nb 1\n").unwrap();
        std::fs::write(dir.join(".dasher-verified-model"), b"").unwrap();

        // catalog sees the files (installed) even before adoption
        let entry = SHERPA_SOURCE.entry(id).expect("catalogue row");
        assert!(entry.installed, "files on disk count as present");

        // adopt: writes the neutral manifest, preserves the app marker
        SHERPA_SOURCE.adopt(id).expect("adopt legacy");
        let manifest = InstallManifest::read_from(&dir).expect("manifest written");
        assert_eq!(manifest.id, id);
        assert!(!manifest.sha256.is_empty(), "registry digest pinned");
        assert!(
            dir.join(".dasher-verified-model").is_file(),
            "app marker kept"
        );

        // an empty directory refuses adoption with a clear error
        let empty_id = "piper-fr_FR-siwis-medium";
        std::fs::create_dir_all(models_dir().join(empty_id)).unwrap();
        let err = SHERPA_SOURCE.adopt(empty_id).expect_err("no files");
        assert!(matches!(err, InstallError::UnsupportedUrl(_)), "{err}");
        // unknown ids are ModelNotFound
        let err = SHERPA_SOURCE.adopt("zzz").expect_err("unknown");
        assert!(matches!(err, InstallError::ModelNotFound));

        match prior {
            Some(p) => std::env::set_var("TTS_MODELS_DIR", p),
            None => std::env::remove_var("TTS_MODELS_DIR"),
        }
    }

    /// #106 item 2: the catalog distinguishes files-on-disk from
    /// ready-offline — an installed needs_bundle voice without the
    /// bundle cached is NOT ready; with it cached it is.
    #[cfg(feature = "sherpa-g2p")]
    #[test]
    fn installed_needs_bundle_distinguishes_cached_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = crate::local_models::test_util::env_lock();
        let _env_guard = EnvGuard {
            models: std::env::var_os("TTS_MODELS_DIR"),
            vg: std::env::var_os("VOICEGARDEN_LEXICON_DIR"),
        };
        std::env::set_var("TTS_MODELS_DIR", tmp.path());
        // ISOLATED empty bundle cache — the developer machine's real
        // cache may already hold fra from live tests
        let empty_cache = tmp.path().join("empty-bundle-cache");
        std::fs::create_dir_all(&empty_cache).unwrap();
        std::env::set_var("VOICEGARDEN_LEXICON_DIR", &empty_cache);

        let id = "piper-fr_FR-siwis-medium";
        let dir = models_dir().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.onnx"), b"x").unwrap();
        std::fs::write(dir.join(MANIFEST_NAME), "{}").unwrap(); // unreadable → no manifest

        let entry = SHERPA_SOURCE.entry(id).expect("row");
        assert!(entry.installed, "files present");
        assert_eq!(
            entry.availability,
            Availability::NeedsBundle,
            "files on disk but the bundle is not cached — not ready offline"
        );

        // populate a stamp-verified bundle cache → the same files are
        // now READY (the bundle monolith satisfies the local manifest)
        let cache = crate::sherpa_lexicon::lexicon_cache_dir();
        std::fs::create_dir_all(cache.join("fra")).unwrap();
        std::fs::write(cache.join("fra/fra.fst"), b"x").unwrap();
        std::fs::write(cache.join("fra/fra.pho"), b"x").unwrap();
        let stamp = "feedface";
        std::fs::write(cache.join("fra/.sha256"), stamp).unwrap();
        let manifest_json = format!(
            concat!(
                r#"{{"version":"t","languages":[{{"lang":"fra","#,
                r#""name":"French","bcp47":"fr-FR","entries":1,"#,
                r#""license":"x","source":"x","phonetisaurus":null,"#,
                r#""file":"fra.tar.gz","sha256":"{}"}}]}}"#
            ),
            stamp
        );
        std::fs::write(cache.join("expanded.json"), manifest_json).unwrap();
        let entry = SHERPA_SOURCE.entry(id).expect("row");
        assert_eq!(
            entry.availability,
            Availability::Ready,
            "bundle cached → ready offline"
        );

        // cleanup: the cache lives OUTSIDE the tmp models dir
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[cfg(feature = "sherpa-g2p")]
    #[test]
    fn english_bundle_failure_degrades_not_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = crate::local_models::test_util::env_lock();
        let _env_guard = EnvGuard {
            models: std::env::var_os("TTS_MODELS_DIR"),
            vg: std::env::var_os("VOICEGARDEN_LEXICON_DIR"),
        };
        std::env::set_var("TTS_MODELS_DIR", tmp.path());
        // unusable bundle cache — same trick as the gate test
        let bad_cache = tmp.path().join("bad-bundle-cache");
        std::fs::write(&bad_cache, b"not a directory").unwrap();
        std::env::set_var("VOICEGARDEN_LEXICON_DIR", &bad_cache);

        // English: the bundle is the UPGRADE (auto-lexicon speaks);
        // its failure is a warning, the install still commits
        let id = "piper-en_US-amy-low";
        install_with_fetcher(id, &FakeArchiveHttp, &fake_sha(), &|_, _| {}, &|| false)
            .expect("English degrades instead of failing");
        assert!(
            models_dir().join(id).join("model.onnx").is_file(),
            "the install committed"
        );
    }
}
