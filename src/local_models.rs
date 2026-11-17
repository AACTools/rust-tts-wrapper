//! Engine-agnostic local-model management (#102 Phase 1): ONE
//! catalogue, ONE availability vocabulary, ONE install path for every
//! local engine compiled into the build.
//!
//! The unified `Availability` enum is a superset RENAME of the sherpa
//! `readiness` vocabulary (v1.10.0), not a parallel taxonomy —
//! `needs_bundle` keeps its name, `blocked` keeps its parameterized
//! reasons as data (`reason`), `needs_assets` generalizes
//! `needs_assets:vocoder` (`missing`), and `needs_download` is the one
//! genuinely new member (files absent, registry complete → installable
//! now). One mapping site derives it; the legacy sherpa field stays
//! derivable from it byte-for-byte (`needs_assets` +
//! `missing: ["vocoder"]` → `"needs_assets:vocoder"`).
//!
//! Install reliability rules (the Dasher-GTK integration lessons):
//! a custom User-Agent (GitHub serves HTTP 200 + a 9-byte "Not Found"
//! body to curl's default UA), magic-byte + minimum-size validation
//! on every completed fetch, a checksum gate on every fetched byte,
//! `.part` files that survive cancellation and resume via HTTP
//! Range, disk preflight with expansion headroom before the first
//! byte, extraction to a temp dir with an atomic swap, and a neutral
//! `.installed` manifest (model id, digest, file list) — never a
//! consumer-branded marker. ONE install per process at a time;
//! installs run synchronously on the caller's thread with
//! progress/cancel callbacks firing on that same thread.

use std::path::PathBuf;

/// The unified vocabulary (#102): a superset rename of the sherpa
/// `readiness` taxonomy. Serialize as the `availability` string field
/// plus data companions (`missing`, `reason`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Files present, speakable offline now
    Ready,
    /// Files absent, registry complete (url + sha256) → installable
    NeedsDownload,
    /// Model present; its language's G2P bundle is missing (sherpa)
    NeedsBundle,
    /// Model present; shared assets missing (`["vocoder"]` for matcha)
    NeedsAssets(Vec<&'static str>),
    /// Native front-end works but offline validation is pending (MMS)
    Unverified,
    /// Not installable/usable in this build — `reason` carries the
    /// registry's parameterized cause (`no-url`, `no-sha256`,
    /// `deprecated`, `g2p-unavailable`, `no-bundle`,
    /// `bundle-not-validated`)
    Blocked {
        /// the registry's parameterized cause (`no-url`, `no-sha256`,
        /// `deprecated`, `g2p-unavailable`, `no-bundle`,
        /// `bundle-not-validated`)
        reason: String,
    },
    /// The id belongs to another engine's catalogue
    WrongEngine,
}

impl Availability {
    /// Derive from a sherpa `readiness` string — the ONE mapping site
    /// between the vocabularies (#102 F1).
    #[must_use]
    pub fn from_readiness(readiness: &str) -> Self {
        match readiness {
            "ready" => Self::Ready,
            "needs_bundle" => Self::NeedsBundle,
            "unverified" => Self::Unverified,
            r if r.starts_with("needs_assets:") => {
                Self::NeedsAssets(vec![match r.strip_prefix("needs_assets:") {
                    Some(asset) => match asset {
                        "vocoder" => "vocoder",
                        "vocoder-reference" => "vocoder-reference",
                        _ => "assets",
                    },
                    None => "assets",
                }])
            }
            r if r.starts_with("blocked:") => Self::Blocked {
                reason: r.strip_prefix("blocked:").unwrap_or("unknown").to_string(),
            },
            _ => Self::Blocked {
                reason: "unknown".to_string(),
            },
        }
    }

    /// The primary string field of the catalogue JSON.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NeedsDownload => "needs_download",
            Self::NeedsBundle => "needs_bundle",
            Self::NeedsAssets(_) => "needs_assets",
            Self::Unverified => "unverified",
            Self::Blocked { .. } => "blocked",
            Self::WrongEngine => "wrong_engine",
        }
    }

    /// The legacy sherpa `readiness` value, derivable byte-for-byte
    /// from the unified model (the #102 F2 shim contract).
    #[must_use]
    pub fn to_readiness(&self) -> String {
        match self {
            Self::Ready => "ready".into(),
            Self::NeedsDownload => "needs_download".into(),
            Self::NeedsBundle => "needs_bundle".into(),
            Self::Unverified => "unverified".into(),
            Self::Blocked { reason } => format!("blocked:{reason}"),
            Self::NeedsAssets(assets) => match assets.first() {
                Some(a) => format!("needs_assets:{a}"),
                None => "needs_assets".into(),
            },
            Self::WrongEngine => "wrong_engine".into(),
        }
    }
}

/// One catalogue entry (engine-agnostic). Serialized with snake_case
/// keys matching the sherpa registry's conventions.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    /// the owning local engine id ("sherpaonnx")
    pub engine: &'static str,
    /// the public registry id (used by install/uninstall/create)
    pub id: String,
    /// human-readable name
    pub name: String,
    /// engine family ("vits", "kokoro", "matcha", …)
    pub model_type: String,
    /// quality tier from the registry ("low", "medium", …)
    pub quality: String,
    /// (code, display name) pairs
    pub languages: Vec<(String, String)>,
    /// speaker count
    pub num_speakers: u32,
    /// synthesis sample rate in Hz
    pub sample_rate: u32,
    /// approximate download size
    pub filesize_mb: f64,
    /// license label
    pub license: String,
    /// files present in the engine's model dir
    pub installed: bool,
    /// installed digest ≠ registry digest (manifest-verified installs)
    pub update_available: bool,
    /// the engine itself is experimental (floravox today): features
    /// work but the engine is under active development — UIs may
    /// badge or section these entries
    pub experimental: bool,
    /// the unified availability value
    pub availability: Availability,
}

impl ModelEntry {
    /// The catalogue JSON object for this entry.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut o = serde_json::json!({
            "engine": self.engine,
            "id": self.id,
            "name": self.name,
            "model_type": self.model_type,
            "quality": self.quality,
            "languages": self.languages.iter().map(|(c, n)| serde_json::json!({
                "code": c, "name": n,
            })).collect::<Vec<_>>(),
            "num_speakers": self.num_speakers,
            "sample_rate": self.sample_rate,
            "filesize_mb": self.filesize_mb,
            "license": self.license,
            "installed": self.installed,
            "update_available": self.update_available,
            "experimental": self.experimental,
            "availability": self.availability.as_str(),
        });
        match &self.availability {
            Availability::NeedsAssets(assets) => {
                o["missing"] = serde_json::json!(assets);
            }
            Availability::Blocked { reason } => {
                o["reason"] = serde_json::json!(reason);
            }
            _ => {}
        }
        o
    }
}

/// One installed voice in the ctx-free catalogue: everything a UI
/// needs to speak without engine-specific knowledge.
#[derive(Debug, Clone)]
pub struct CatalogueVoice {
    /// the engine to create ("sherpaonnx")
    pub engine: &'static str,
    /// the model to create it for
    pub model: String,
    /// the voice id to select
    pub voice: String,
    /// display name
    pub name: String,
    /// (bcp47, display) pairs
    pub language_codes: Vec<(String, String)>,
}

impl CatalogueVoice {
    /// The catalogue JSON object for this voice.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "engine": self.engine,
            "model": self.model,
            "voice": self.voice,
            "name": self.name,
            "languages": self.language_codes.iter().map(|(c, d)| serde_json::json!({
                "bcp47": c, "display": d,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Progress callback: overall fraction 0.0..=1.0 plus a stage name
/// (`download`, `verify`, `extract`, `post-install`).
pub type ProgressFn<'a> = &'a dyn Fn(f64, &str);
/// Cancellation probe — checked between chunks/entries; true aborts.
pub type CancelFn<'a> = &'a dyn Fn() -> bool;

/// The engine-facing install contract. Phase 1 ships the sherpa
/// implementation; omnivoice/qwen3 arrive with their asset registries
/// (#102 F3).
pub trait ModelSource: Sync {
    /// The engine id this source serves ("sherpaonnx").
    fn engine_id(&self) -> &'static str;
    /// The full catalogue (installed state + availability included).
    fn list(&self) -> Vec<ModelEntry>;
    /// One entry by id (`None` when the id is not in this engine's
    /// catalogue — a caller-facing `WrongEngine` distinction the FFI
    /// layer resolves across engines).
    fn entry(&self, id: &str) -> Option<ModelEntry>;
    /// Install (verify → fetch → extract → post-install → atomic
    /// swap → `.installed` manifest). Synchronous on the caller's
    /// thread; ONE install per process.
    ///
    /// # Errors
    /// [`InstallError`] — see its variants; `ModelNotFound` when the
    /// id is not in this engine's catalogue.
    fn install(
        &self,
        id: &str,
        progress: ProgressFn<'_>,
        cancel: CancelFn<'_>,
    ) -> Result<(), InstallError>;
    /// Remove an installed model (manifest + files).
    ///
    /// # Errors
    /// `ModelNotFound` when nothing is installed under that id;
    /// `InUse` when the platform cannot remove open files (Windows).
    fn uninstall(&self, id: &str) -> Result<(), InstallError>;

    /// Adopt an EXISTING directory at `models_dir/<id>` as an
    /// installed model (#106): validate the family's expected files
    /// and write the neutral `.installed` manifest from the registry
    /// digest. For apps migrating their own model layouts — the app
    /// moves/copies its verified files into place first; its own
    /// verification markers stay the app's (Dasher keeps
    /// `.dasher-verified-model` in Dasher).
    ///
    /// # Errors
    /// `ModelNotFound` when the id is not in this engine's catalogue;
    /// `UnsupportedUrl`-class message when the directory lacks the
    /// files the engine needs to load the model.
    fn adopt(&self, id: &str) -> Result<(), InstallError>;
    /// The engine's model directory (`TTS_MODELS_DIR` override honored).
    fn models_dir(&self) -> PathBuf;
    /// Installed models flattened into speakable voices.
    fn voices(&self) -> Vec<CatalogueVoice>;
}

/// Install failures, mapped onto the FFI error-detail surface.
#[derive(Debug)]
pub enum InstallError {
    /// id not in ANY compiled-in catalogue
    ModelNotFound,
    /// another install is running (one per process)
    InstallInProgress,
    /// not enough disk: (bytes wanted, bytes free)
    InsufficientDisk(u64, u64),
    /// digest mismatch after a completed download
    ChecksumMismatch {
        /// the registry digest
        expected: String,
        /// what actually arrived
        got: String,
    },
    /// transport/HTTP failure (message included)
    Network(String),
    /// url layout this build cannot install from
    UnsupportedUrl(String),
    /// the cancelled callback fired
    Cancelled,
    /// filesystem failure (extraction, swap, manifest): (context, io error)
    Io(String, std::io::Error),
    /// a model file a live engine holds open on Windows, etc.
    InUse(String),
    /// the model's REQUIRED pronunciation bundle could not be
    /// provisioned (its fetch failed). The verified model staging is
    /// RETAINED — a retry skips the download and re-attempts only the
    /// bundle (#106). Retryable.
    PronunciationBundle(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModelNotFound => write!(f, "model id not found in any compiled-in catalogue"),
            Self::InstallInProgress => {
                write!(f, "another model install is in progress (one per process)")
            }
            Self::InsufficientDisk(want, free) => write!(
                f,
                "insufficient disk: {want} bytes needed (with expansion headroom), {free} free",
            ),
            Self::ChecksumMismatch { expected, got } => {
                write!(f, "checksum mismatch: expected {expected}, got {got}")
            }
            Self::Network(m) => write!(f, "network: {m}"),
            Self::UnsupportedUrl(u) => write!(f, "unsupported url layout: {u}"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Io(ctx, e) => write!(f, "{ctx}: {e}"),
            Self::InUse(m) => write!(f, "model in use: {m}"),
            Self::PronunciationBundle(e) => write!(
                f,
                "required pronunciation bundle failed to provision ({e}); \
                 the model download is retained — retry re-attempts only the bundle"
            ),
        }
    }
}

/// Every compiled-in local engine's model source, in stable order.
#[must_use]
// the push form is CORRECT here: the combinatorial-arm form silently
// dropped engines in pairing builds (loop-1 review, proven by probe)
#[allow(clippy::vec_init_then_push, unused_mut)]
pub fn sources() -> Vec<&'static dyn ModelSource> {
    let mut v: Vec<&'static dyn ModelSource> = Vec::new();
    #[cfg(feature = "sherpaonnx")]
    v.push(&crate::sherpa_model_source::SHERPA_SOURCE);
    #[cfg(feature = "floravox")]
    v.push(&crate::floravox_model_source::FLORAVOX_SOURCE);
    #[cfg(feature = "omnivoice-local")]
    v.push(&crate::omnivoice_model_source::OMNIVOICE_SOURCE);
    v
}

/// The direct availability ladder for engines without bundle tiers
/// (floravox, omnivoice): installed → ready; registry-gated states →
/// blocked with the parameterized reason; else installable now.
/// (Sherpa's readiness ladder stays in sherpa_catalog — it has bundle
/// tiers this one does not.)
#[must_use]
pub(crate) fn simple_availability(facts: ModelFacts) -> Availability {
    if facts.installed {
        Availability::Ready
    } else if facts.deprecated {
        Availability::Blocked {
            reason: "deprecated".into(),
        }
    } else if !facts.has_url {
        Availability::Blocked {
            reason: "no-url".into(),
        }
    } else if !facts.has_sha256 {
        Availability::Blocked {
            reason: "no-sha256".into(),
        }
    } else {
        Availability::NeedsDownload
    }
}

/// The registry facts the [`simple_availability`] ladder reads.
#[derive(Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ModelFacts {
    /// files present in the engine's model dir
    pub installed: bool,
    /// the registry row is deprecated
    pub deprecated: bool,
    /// the row carries a download url
    pub has_url: bool,
    /// the row carries a main-file sha256
    pub has_sha256: bool,
}

/// Look an id up across every compiled-in engine (or in the one named
/// by `engine_id`).
///
/// # Errors
/// `ModelNotFound` when no compiled-in source matches.
pub fn find_source(
    engine_id: Option<&str>,
    model_id: &str,
) -> Result<&'static dyn ModelSource, InstallError> {
    let all = sources();
    if let Some(engine) = engine_id {
        all.into_iter()
            .find(|s| s.engine_id() == engine)
            .ok_or(InstallError::ModelNotFound)
    } else {
        all.into_iter()
            .find(|s| s.entry(model_id).is_some())
            .ok_or(InstallError::ModelNotFound)
    }
}

// ──────────────────────────────────────────────────────────────────
// Install machinery (shared by every ModelSource). Gated on the
// engines that pull the tooling deps (Phase 1: sherpaonnx +
// floravox); when omnivoice/qwen3 sources arrive they join this gate
// via a shared internal feature.
// ──────────────────────────────────────────────────────────────────

#[cfg(any(
    feature = "sherpaonnx",
    feature = "floravox",
    feature = "omnivoice-local"
))]
pub(crate) mod install {
    use super::{CancelFn, InstallError, ProgressFn};
    use std::io::{Read, Write as _};

    /// lowercase hex digits (digest formatting without per-byte format!)
    const HEX: &[u8; 16] = b"0123456789abcdef";
    use std::path::{Path, PathBuf};
    /// The minimum plausibly-valid download: GitHub release assets can
    /// return HTTP 200 with a 9-byte "Not Found" text body depending on
    /// User-Agent (live-verified in the Dasher-GTK integration) — status
    /// alone proves nothing.
    pub(crate) const MIN_DOWNLOAD_BYTES: u64 = 1024;

    /// The User-Agent that gets real bytes from GitHub release assets.
    pub(crate) const HTTP_USER_AGENT: &str =
        concat!("rust-tts-wrapper/", env!("CARGO_PKG_VERSION"));

    /// The neutral install marker (never consumer-branded — #102 F8):
    /// `{ id, engine, sha256, files, installed_at }` inside the model dir.
    pub(crate) const MANIFEST_NAME: &str = ".installed";

    #[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
    pub(crate) struct InstallManifest {
        pub id: String,
        pub engine: String,
        pub sha256: String,
        pub files: Vec<String>,
        pub installed_at: u64,
    }

    impl InstallManifest {
        pub(crate) fn write_to(&self, dir: &Path) -> Result<(), InstallError> {
            let path = dir.join(MANIFEST_NAME);
            serde_json::to_string_pretty(self)
                .map_err(|e| {
                    InstallError::Io("manifest serialize".into(), std::io::Error::other(e))
                })
                .and_then(|s| {
                    std::fs::write(&path, s)
                        .map_err(|e| InstallError::Io("manifest write".into(), e))
                })
        }

        pub(crate) fn read_from(dir: &Path) -> Option<Self> {
            serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST_NAME)).ok()?).ok()
        }
    }

    /// The byte-range GET the installer needs. A trait so the machinery
    /// is testable without a network: production uses ureq, tests inject
    /// a fixture server.
    pub(crate) trait HttpFetch {
        /// Stream `url` (optionally resuming at `from` bytes) to `sink`,
        /// returning bytes written. `None` content-length = unknown.
        fn get_range(
            &self,
            url: &str,
            from: u64,
            sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<Option<u64>, String>;
    }

    /// Production fetcher: ureq with the wrapper User-Agent.
    pub(crate) struct UreqFetcher;

    impl HttpFetch for UreqFetcher {
        fn get_range(
            &self,
            url: &str,
            from: u64,
            sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<Option<u64>, String> {
            let mut req = ureq::get(url).set("User-Agent", HTTP_USER_AGENT);
            if from > 0 {
                req = req.set("Range", &format!("bytes={from}-"));
            }
            let resp = req.call().map_err(|e| format!("{e}"))?;
            // a Range request the server ignored (200 instead of 206)
            // invalidates resume — restart from zero
            let status = resp.status();
            // 416 = the part file is stale/over-long: same remedy
            if from > 0 && (status == 200 || status == 416) {
                return Err("resume-not-supported".into());
            }
            if !(200..300).contains(&status) {
                return Err(format!("HTTP {status}"));
            }
            let total = resp
                .header("Content-Length")
                .and_then(|v| v.parse::<u64>().ok());
            let mut reader = resp.into_reader();
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = reader.read(&mut buf).map_err(|e| format!("{e}"))?;
                if n == 0 {
                    break;
                }
                sink(&buf[..n])?;
            }
            Ok(total)
        }
    }

    /// Magic bytes for the archive formats the installer extracts
    /// (bzip2 `BZh`, gzip `1f 8b`); `None` = no known magic (raw file).
    pub(crate) fn archive_kind(path: &Path) -> Result<Option<&'static str>, InstallError> {
        let mut magic = [0u8; 3];
        let mut f =
            std::fs::File::open(path).map_err(|e| InstallError::Io("magic open".into(), e))?;
        let n = f
            .read(&mut magic)
            .map_err(|e| InstallError::Io("magic read".into(), e))?;
        if n >= 3 && &magic[..3] == b"BZh" {
            return Ok(Some("bz2"));
        }
        if n >= 2 && magic[..2] == [0x1f, 0x8b] {
            return Ok(Some("gz"));
        }
        Ok(None)
    }

    /// Fetch `url` into `dest` (a `.part` path), resuming when the part
    /// file already exists, verifying magic bytes + minimum size + sha256
    /// on completion. Returns the digest actually downloaded.
    #[allow(clippy::cast_precision_loss)] // progress fractions
    pub(crate) fn fetch_verified(
        fetcher: &dyn HttpFetch,
        url: &str,
        expected_sha256: &str,
        dest: &Path,
        expected_bytes: u64,
        progress: ProgressFn<'_>,
        cancel: CancelFn<'_>,
    ) -> Result<(), InstallError> {
        use sha2::Digest as _;
        let mut have = dest.metadata().map_or(0, |m| m.len());
        std::fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))
            .map_err(|e| InstallError::Io("part dir".into(), e))?;
        // append+create: resumes an existing part file, starts a new one
        // otherwise; a fresh (zero-length) part is truncated for good measure
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(dest)
            .map_err(|e| InstallError::Io("part open".into(), e))?;
        // NOTE: no truncate — an append-mode handle cannot set_len on
        // Windows (ERROR_ACCESS_DENIED), and `have == 0` already means
        // the file is empty; create() gave us a fresh one otherwise
        let mut hasher = sha2::Sha256::new();
        // hashing a resumed download needs the bytes we already have
        if have > 0 {
            let mut f =
                std::fs::File::open(dest).map_err(|e| InstallError::Io("part reopen".into(), e))?;
            let mut remaining = have;
            let mut buf = [0u8; 16 * 1024];
            while remaining > 0 {
                let n = std::cmp::min(remaining as usize, buf.len());
                let read = f
                    .read(&mut buf[..n])
                    .map_err(|e| InstallError::Io("part rehash".into(), e))?;
                if read == 0 {
                    break;
                }
                hasher.update(&buf[..read]);
                remaining -= read as u64;
            }
        }

        let start = have;
        let total_hint = expected_bytes.max(have);
        // one closure owns the byte count: write + hash + progress
        let res = fetcher.get_range(url, start, &mut |chunk| {
            if cancel() {
                return Err("cancelled".into());
            }
            file.write_all(chunk).map_err(|e| format!("{e}"))?;
            hasher.update(chunk);
            have += chunk.len() as u64;
            let frac = if total_hint > 0 {
                (have as f64 / total_hint as f64).min(0.70)
            } else {
                0.5
            };
            progress(frac, "download");
            Ok(())
        });
        match res {
            Ok(_) => {}
            Err(e) if e == "cancelled" => return Err(InstallError::Cancelled),
            Err(e) if e == "resume-not-supported" => {
                // server ignored Range: drop the handle (Windows will
                // not truncate an open file), reset, fetch whole
                drop(file);
                std::fs::write(dest, []).map_err(|e| InstallError::Io("part reset".into(), e))?;
                return fetch_verified(
                    fetcher,
                    url,
                    expected_sha256,
                    dest,
                    expected_bytes,
                    progress,
                    cancel,
                );
            }
            Err(e) => return Err(InstallError::Network(e)),
        }
        // release the handle BEFORE any removal/rotation — Windows
        // refuses to delete an open file (the .ok()s would silently
        // leave failed-download parts behind)
        drop(file);
        // size floor: a 200-with-"Not Found" body is tiny
        let got_len = dest.metadata().map_or(0, |m| m.len());
        if got_len < MIN_DOWNLOAD_BYTES {
            std::fs::remove_file(dest).ok();
            return Err(InstallError::Network(format!(
                "download implausibly small ({got_len} bytes < {MIN_DOWNLOAD_BYTES})"
            )));
        }
        progress(0.72, "verify");
        let digest = hasher.finalize();
        let mut got = String::with_capacity(digest.len() * 2);
        for b in digest {
            got.push(HEX[(b >> 4) as usize] as char);
            got.push(HEX[(b & 0xf) as usize] as char);
        }
        if !got.eq_ignore_ascii_case(expected_sha256) {
            std::fs::remove_file(dest).ok();
            return Err(InstallError::ChecksumMismatch {
                expected: expected_sha256.to_string(),
                got,
            });
        }
        progress(0.74, "verify");
        Ok(())
    }

    /// Extract a verified archive (by magic bytes) or copy a raw file
    /// into `dest_dir`, refusing path traversal. Archives with a single
    /// top-level directory are stripped (the canonical engine layout).
    #[allow(clippy::cast_precision_loss)] // progress fractions
    pub(crate) fn extract_into(
        archive: &Path,
        kind: Option<&str>,
        raw_target: &str,
        dest_dir: &Path,
        progress: ProgressFn<'_>,
        cancel: CancelFn<'_>,
    ) -> Result<Vec<String>, InstallError> {
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| InstallError::Io("extract mkdir".into(), e))?;
        let mut files = Vec::new();
        if let Some("bz2" | "gz") = kind {
            let f = std::fs::File::open(archive)
                .map_err(|e| InstallError::Io("archive open".into(), e))?;
            let reader: Box<dyn Read> = if kind == Some("bz2") {
                Box::new(bzip2_rs::DecoderReader::new(f))
            } else {
                Box::new(flate2::read::GzDecoder::new(f))
            };
            let mut tar = tar::Archive::new(reader);
            let entries: Vec<tar::Entry<'_, _>> = tar
                .entries()
                .map_err(|e| InstallError::Io("archive entries".into(), e))?
                .collect::<Result<_, _>>()
                .map_err(|e| InstallError::Io("archive read".into(), e))?;
            // strip a single top-level directory (the piper layout:
            // every entry nests under vits-piper-<voice>/)
            let strip = single_root_prefix(&entries);
            let total = entries.len().max(1);
            for (i, mut entry) in entries.into_iter().enumerate() {
                if cancel() {
                    return Err(InstallError::Cancelled);
                }
                let path = entry
                    .path()
                    .map_err(|e| InstallError::Io("entry path".into(), e))?
                    .to_path_buf();
                let etype = entry.header().entry_type();
                // hardlinks/symlinks/devices: skip (models need none) —
                // only regular files and directories extract
                if !etype.is_file() && !etype.is_dir() {
                    continue;
                }
                if path.to_string_lossy().contains("..") {
                    return Err(InstallError::UnsupportedUrl(format!(
                        "archive entry escapes destination: {}",
                        path.display()
                    )));
                }
                let rel = strip_components(&path, strip);
                let Some(rel) = rel else { continue };
                if rel.as_os_str().is_empty() {
                    continue;
                }
                let target = dest_dir.join(&rel);
                // traversal guard: the parent must stay inside dest
                if let Some(parent) = target.parent() {
                    let dest_s = dest_dir.to_string_lossy();
                    if !parent.starts_with(&*dest_s) {
                        return Err(InstallError::UnsupportedUrl(format!(
                            "archive entry escapes destination: {}",
                            path.display()
                        )));
                    }
                }
                if entry.header().entry_type().is_dir() {
                    std::fs::create_dir_all(&target)
                        .map_err(|e| InstallError::Io("entry mkdir".into(), e))?;
                    continue;
                }
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                std::fs::create_dir_all(target.parent().unwrap_or(dest_dir))
                    .map_err(|e| InstallError::Io("entry parent".into(), e))?;
                let mut out = std::fs::File::create(&target)
                    .map_err(|e| InstallError::Io("entry create".into(), e))?;
                std::io::copy(&mut entry, &mut out)
                    .map_err(|e| InstallError::Io("entry extract".into(), e))?;
                files.push(rel.to_string_lossy().to_string());
                progress(0.75 + 0.20 * (i as f64 / total as f64), "extract");
            }
        } else {
            // raw single file: copied under the URL's last segment —
            // the caller passes it explicitly because the archive
            // path here is a hidden `.part` file
            let target = dest_dir.join(raw_target);
            std::fs::copy(archive, &target).map_err(|e| InstallError::Io("raw copy".into(), e))?;
            files.push(raw_target.to_string());
        }
        Ok(files)
    }

    fn strip_components(path: &Path, n: usize) -> Option<PathBuf> {
        let mut comps: Vec<_> = path.components().collect();
        if comps.len() <= n {
            return None;
        }
        comps.drain(..n);
        Some(comps.iter().collect())
    }

    /// `Some(k)` when every entry in the archive nests under ONE shared
    /// top-level directory that at least one entry sits inside (k = that
    /// prefix's component count); `Some(0)` when files already sit at the
    /// archive root.
    fn single_root_prefix<R: Read>(entries: &[tar::Entry<'_, R>]) -> usize {
        let mut shared: Option<std::ffi::OsString> = None;
        let mut nested = false;
        for e in entries {
            let Ok(path) = e.path() else { continue };
            let mut comps = path.components();
            let Some(first) = comps.next() else { continue };
            let first = first.as_os_str().to_owned();
            match &shared {
                None => shared = Some(first),
                Some(s) if *s == first => {}
                Some(_) => return 0, // two different roots: nothing to strip
            }
            if comps.next().is_some() {
                nested = true;
            }
        }
        usize::from(nested)
    }

    /// Disk preflight (#102 F7): `wanted` bytes (download + expansion
    /// headroom, computed by the caller) vs available space on the
    /// models filesystem.
    pub(crate) fn preflight_disk(dir: &Path, wanted: u64) -> Result<(), InstallError> {
        use fs2::available_space;
        std::fs::create_dir_all(dir).map_err(|e| InstallError::Io("models dir".into(), e))?;
        let free = available_space(dir).map_err(|e| InstallError::Io("disk probe".into(), e))?;
        if free < wanted {
            return Err(InstallError::InsufficientDisk(wanted, free));
        }
        Ok(())
    }

    /// The process-wide install lock (#102 F6: ONE install per process).
    static INSTALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `body` under the install lock, failing fast with
    /// [`InstallError::InstallInProgress`] when another install runs.
    pub(crate) fn with_install_lock<T>(body: impl FnOnce() -> T) -> Result<T, InstallError> {
        match INSTALL_LOCK.try_lock() {
            Ok(_guard) => Ok(body()),
            Err(std::sync::TryLockError::Poisoned(p)) => {
                // a panicked install must not brick installs forever
                let _guard = p.into_inner();
                Ok(body())
            }
            Err(std::sync::TryLockError::WouldBlock) => Err(InstallError::InstallInProgress),
        }
    }

    /// Atomic swap of `staging` into `final_dir` (#102 F6): Unix rename
    /// semantics leave open mmaps on the old inode; on Windows an open
    /// model file fails the swap with an `InUse` detail instead of
    /// corrupting anything.
    pub(crate) fn atomic_swap(staging: &Path, final_dir: &Path) -> Result<(), InstallError> {
        let parent = final_dir.parent().ok_or_else(|| {
            InstallError::Io("swap parent".into(), std::io::Error::other("no parent"))
        })?;
        let old = parent.join(format!(
            ".old-{}",
            final_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("model")
        ));
        let _ = std::fs::remove_dir_all(&old);
        if final_dir.exists() {
            std::fs::rename(final_dir, &old).map_err(|e| {
                InstallError::InUse(format!("cannot move the current model dir: {e}"))
            })?;
        }
        std::fs::rename(staging, final_dir).map_err(|e| {
            // put the old copy back if we can
            if old.exists() {
                std::fs::rename(&old, final_dir).ok();
            }
            InstallError::InUse(format!("cannot swap in the new model dir: {e}"))
        })?;
        std::fs::remove_dir_all(&old).ok();
        Ok(())
    }
}

/// Env-mutation serializer for tests that set TTS_MODELS_DIR
/// (process-wide; parallel set/restore races make one test see
/// another's restore). Shared by every model-source test module.
#[cfg(test)]
pub(crate) mod test_util {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the lock; poisoned locks recover (a panicked test must
    /// not brick the others).
    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // env-mutating tests take crate::local_models::test_util::env_lock()
    // (the ONE lock — a twin here would let a future test serialize on
    // the wrong mutex and reintroduce the race; loop-2 review)

    // ── availability vocabulary ──

    #[test]
    fn availability_round_trips_every_readiness_value() {
        for readiness in [
            "ready",
            "needs_bundle",
            "unverified",
            "needs_assets:vocoder",
            "needs_assets:vocoder-reference",
            "blocked:no-url",
            "blocked:no-sha256",
            "blocked:deprecated",
            "blocked:g2p-unavailable",
            "blocked:no-bundle",
            "blocked:bundle-not-validated",
        ] {
            let avail = Availability::from_readiness(readiness);
            assert_eq!(
                avail.to_readiness(),
                readiness,
                "{readiness} must survive the unified round-trip"
            );
        }
    }

    #[test]
    fn availability_json_carries_data_companions() {
        let mut e = ModelEntry {
            engine: "sherpaonnx",
            id: "m".into(),
            name: "M".into(),
            model_type: "matcha".into(),
            quality: String::new(),
            languages: vec![],
            num_speakers: 1,
            sample_rate: 16000,
            filesize_mb: 1.0,
            license: String::new(),
            installed: true,
            update_available: false,
            experimental: false,
            availability: Availability::NeedsAssets(vec!["vocoder"]),
        };
        let j = e.to_json();
        assert_eq!(j["availability"], "needs_assets");
        assert_eq!(j["missing"][0], "vocoder");
        e.availability = Availability::Blocked {
            reason: "no-sha256".into(),
        };
        let j = e.to_json();
        assert_eq!(j["availability"], "blocked");
        assert_eq!(j["reason"], "no-sha256");
    }

    /// The ONE mapping site pinned against the real registry: every
    /// model's readiness value must survive
    /// from_readiness→to_readiness unchanged (the #102 F2 shim
    /// contract — byte-for-byte legacy derivability).
    #[cfg(feature = "sherpaonnx")]
    #[test]
    fn every_registry_readiness_survives_the_unified_round_trip() {
        for (id, model) in sherpa_onnx_models::models() {
            let info = crate::sherpa_registry_info(model);
            let readiness = crate::sherpa_catalog::readiness_for(
                &info,
                model.sha256.as_deref().is_some_and(|s| !s.is_empty()),
                model.deprecated,
            );
            assert_eq!(
                Availability::from_readiness(&readiness).to_readiness(),
                readiness,
                "registry model {id}: readiness {readiness} broke the round-trip"
            );
        }
    }

    #[cfg(feature = "sherpaonnx")]
    #[test]
    fn catalogue_reports_installed_and_needs_download() {
        // point the models dir at an empty temp dir: nothing installed.
        // set/restore around the call (process env, like the
        // VOICEGARDEN_LEXICON_DIR tests — run with --test-threads=1
        // when combining env-sensitive suites)
        let _env = crate::local_models::test_util::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let prior = std::env::var_os("TTS_MODELS_DIR");
        std::env::set_var("TTS_MODELS_DIR", tmp.path());
        let entries = crate::sherpa_model_source::SHERPA_SOURCE.list();
        match prior {
            Some(p) => std::env::set_var("TTS_MODELS_DIR", p),
            None => std::env::remove_var("TTS_MODELS_DIR"),
        }
        assert!(!entries.is_empty(), "registry catalogue is non-empty");
        // a checksummed, downloadable model reports needs_download
        let amy = entries
            .iter()
            .find(|e| e.id == "piper-en_US-amy-low")
            .expect("amy in catalogue");
        assert_eq!(amy.availability, Availability::NeedsDownload);
        assert!(!amy.installed);
        // floravox-student rows are NOT sherpa's to serve (the engines
        // routing filter — a student row here would install a voice
        // the sherpa vits config cannot load)
        assert!(
            entries
                .iter()
                .all(|e| !e.id.starts_with("willwade-student-")),
            "floravox-student rows must not appear in the sherpa catalogue"
        );
        // the unvalidated published locales are blocked with the
        // parameterized reason (post-checksum-campaign the registry
        // has no no-sha256/no-url states left — those reasons remain
        // reachable only for future registry regressions)
        assert!(
            entries.iter().any(|e| e.availability
                == Availability::Blocked {
                    reason: "bundle-not-validated".into()
                }),
            "the pinned registry carries bundle-not-validated entries"
        );
        // entry() finds by id
        assert!(crate::sherpa_model_source::SHERPA_SOURCE
            .entry("piper-en_US-amy-low")
            .is_some());
        assert!(crate::sherpa_model_source::SHERPA_SOURCE
            .entry("no-such-model")
            .is_none());
    }
}

#[cfg(all(test, feature = "sherpaonnx"))]
mod install_tests {
    use super::install::*;
    use super::Availability;
    use std::io::Write as _;

    const HEX: &[u8; 16] = b"0123456789abcdef";

    /// A fake HTTP source serving one deterministic body in chunks.
    struct FakeHttp {
        body: Vec<u8>,
        chunk: usize,
        fail_after: Option<usize>, // bytes after which the stream errors
    }

    impl HttpFetch for FakeHttp {
        fn get_range(
            &self,
            _url: &str,
            from: u64,
            sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        ) -> Result<Option<u64>, String> {
            let slice = &self.body[from as usize..];
            let mut delivered = from as usize;
            for piece in slice.chunks(self.chunk) {
                if let Some(cut) = self.fail_after {
                    if delivered + piece.len() > cut {
                        // deliver what fits, then the connection dies
                        if delivered < cut {
                            sink(&piece[..cut - delivered])?;
                        }
                        return Err("connection reset".into());
                    }
                }
                sink(piece)?;
                delivered += piece.len();
            }
            Ok(Some(self.body.len() as u64))
        }
    }

    fn big_body(seed: u8) -> Vec<u8> {
        // magic bytes (bz2) + enough mass to clear the size floor
        let mut v = vec![b'B', b'Z', b'h'];
        v.extend(std::iter::repeat_n(seed, 8 * 1024));
        v
    }

    fn sha(body: &[u8]) -> String {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(body);
        let d = h.finalize();
        let mut s = String::with_capacity(d.len() * 2);
        for b in d {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0xf) as usize] as char);
        }
        s
    }

    #[test]
    fn fetch_verifies_digest_and_reports_mismatch() {
        let body = big_body(7);
        let fetcher = FakeHttp {
            body: body.clone(),
            chunk: 1024,
            fail_after: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("m.part");
        let stages = std::cell::RefCell::new(Vec::new());
        fetch_verified(
            &fetcher,
            "mem://m",
            &sha(&body),
            &part,
            body.len() as u64,
            &|_, s| stages.borrow_mut().push(s.to_string()),
            &|| false,
        )
        .expect("verified fetch");
        let stages = stages.into_inner();
        assert_eq!(std::fs::read(&part).unwrap(), body);
        assert!(stages.contains(&"verify".to_string()));

        // wrong digest → mismatch + part removed
        let err = fetch_verified(
            &fetcher,
            "mem://m",
            &sha(&big_body(9)),
            &part,
            body.len() as u64,
            &|_, _| {},
            &|| false,
        )
        .unwrap_err();
        assert!(
            matches!(err, super::InstallError::ChecksumMismatch { .. }),
            "{err}"
        );
        assert!(!part.exists(), "mismatched part removed");
    }

    #[test]
    fn fetch_resumes_from_part_file() {
        let body = big_body(3);
        // first attempt dies after 4 KiB
        let failing = FakeHttp {
            body: body.clone(),
            chunk: 1024,
            fail_after: Some(4096),
        };
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("m.part");
        let err = fetch_verified(
            &failing,
            "mem://m",
            &sha(&body),
            &part,
            body.len() as u64,
            &|_, _| {},
            &|| false,
        )
        .unwrap_err();
        assert!(matches!(err, super::InstallError::Network(_)));
        let have = part.metadata().unwrap().len();
        assert_eq!(have, 4096, "partial bytes persisted");

        // retry RESUMES (server sees Range from 4096) and completes
        let resuming = FakeHttp {
            body: body.clone(),
            chunk: 1024,
            fail_after: None,
        };
        fetch_verified(
            &resuming,
            "mem://m",
            &sha(&body),
            &part,
            body.len() as u64,
            &|_, _| {},
            &|| false,
        )
        .expect("resumed fetch completes");
        assert_eq!(std::fs::read(&part).unwrap(), body);
    }

    #[test]
    fn fetch_cancel_aborts_and_small_bodies_rejected() {
        let body = big_body(5);
        let fetcher = FakeHttp {
            body: body.clone(),
            chunk: 1024,
            fail_after: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("m.part");
        let err = fetch_verified(
            &fetcher,
            "mem://m",
            &sha(&body),
            &part,
            body.len() as u64,
            &|_, _| {},
            &|| true,
        )
        .unwrap_err();
        assert!(matches!(err, super::InstallError::Cancelled));

        // the GitHub "200 + 9-byte Not Found" lesson: tiny body rejected
        let tiny = FakeHttp {
            body: b"Not Found".to_vec(),
            chunk: 9,
            fail_after: None,
        };
        let err = fetch_verified(
            &tiny,
            "mem://nf",
            &sha(b"Not Found"),
            &dir.path().join("nf.part"),
            9,
            &|_, _| {},
            &|| false,
        )
        .unwrap_err();
        assert!(matches!(err, super::InstallError::Network(_)), "{err}");
    }

    /// Build a fixture tar.gz (nested one root dir, like piper) and
    /// extract it: root stripped, files land, manifest-compatible.
    #[test]
    fn extract_strips_single_root_and_lands_files() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            let mut file = std::fs::File::create(dir.path().join("model.onnx")).unwrap();
            file.write_all(b"model-bytes").unwrap();
            let mut tokens = std::fs::File::create(dir.path().join("tokens.txt")).unwrap();
            tokens.write_all(b"a 0\nb 1\n").unwrap();
            tar.append_path_with_name(dir.path().join("model.onnx"), "vits-voice/model.onnx")
                .unwrap();
            tar.append_path_with_name(dir.path().join("tokens.txt"), "vits-voice/tokens.txt")
                .unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.path().join("out");
        let files =
            extract_into(&archive, Some("gz"), "m.bin", &out, &|_, _| {}, &|| false).unwrap();
        assert!(out.join("model.onnx").is_file(), "root stripped: {files:?}");
        assert!(out.join("tokens.txt").is_file());
        assert_eq!(files.len(), 2);
    }

    /// Flat archives (no root) extract as-is; a nested two-root
    /// archive keeps both paths (no strip).
    #[test]
    fn extract_keeps_flat_layout() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("flat.tar.gz");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            let p = dir.path().join("tokens.txt");
            std::fs::write(&p, b"a 0\n").unwrap();
            tar.append_path_with_name(&p, "tokens.txt").unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.path().join("out");
        let files =
            extract_into(&archive, Some("gz"), "m.bin", &out, &|_, _| {}, &|| false).unwrap();
        assert!(out.join("tokens.txt").is_file(), "{files:?}");
    }

    /// Raw single files copy under the CALLER's name (the archive path
    /// is a hidden `.part` — using its basename would hide the model).
    #[test]
    fn raw_file_copies_under_the_url_name() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(".voice.part");
        std::fs::write(&src, b"model-bytes").unwrap();
        let out = dir.path().join("out");
        let files = extract_into(&src, None, "voice.onnx", &out, &|_, _| {}, &|| false).unwrap();
        assert!(out.join("voice.onnx").is_file(), "{files:?}");
        assert!(
            !out.join(".voice.part").exists(),
            "part name must not survive"
        );
    }

    /// Path-traversal attempts are rejected, not extracted. The tar
    /// BUILDER refuses `..` paths, so the hostile archive is
    /// hand-crafted bytes — exactly what an attacker controls.
    #[test]
    fn hostile_archive_entries_are_rejected() {
        // one 512-byte header + 512-byte data block, name "../evil.txt"
        let mut header = [0u8; 512];
        let name = b"../evil.txt";
        header[..name.len()].copy_from_slice(name);
        let size = b"4                        "; // octal, 12 bytes at 124
        header[124..124 + size.len()].copy_from_slice(size);
        header[156] = b'0'; // regular file
        header[257..262].copy_from_slice(b"ustar"); // magic for gnu parsing
                                                    // checksum: spaces in the field during computation
        header[148..156].iter_mut().for_each(|b| *b = b' ');
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        let chk = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(chk.as_bytes());
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(&{
            let mut d = [0u8; 512];
            d[..4].copy_from_slice(b"evil");
            d
        });
        bytes.extend_from_slice(&[0u8; 1024]); // two zero blocks end the archive

        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.tar.gz");
        {
            use flate2::write::GzEncoder;
            let f = std::fs::File::create(&archive).unwrap();
            let mut enc = GzEncoder::new(f, flate2::Compression::default());
            std::io::Write::write_all(&mut enc, &bytes).unwrap();
            enc.finish().unwrap();
        }
        let out = dir.path().join("out");
        let err =
            extract_into(&archive, Some("gz"), "m.bin", &out, &|_, _| {}, &|| false).unwrap_err();
        assert!(
            matches!(err, super::InstallError::UnsupportedUrl(_)),
            "{err}"
        );
        assert!(!dir.path().join("evil.txt").exists(), "nothing escaped");
    }

    /// Manifest round-trip + update detection.
    #[test]
    fn manifest_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let m = InstallManifest {
            id: "x".into(),
            engine: "sherpaonnx".into(),
            sha256: "abc".into(),
            files: vec!["model.onnx".into()],
            installed_at: 42,
        };
        m.write_to(dir.path()).unwrap();
        assert_eq!(
            InstallManifest::read_from(dir.path()).unwrap().sha256,
            "abc"
        );
        assert!(dir.path().join(MANIFEST_NAME).is_file());
    }

    /// Disk preflight reports the bytes it wanted vs free.
    #[test]
    fn preflight_reports_shortfall() {
        let dir = tempfile::tempdir().unwrap();
        let err = preflight_disk(dir.path(), u64::MAX / 2).unwrap_err();
        assert!(
            matches!(err, super::InstallError::InsufficientDisk(w, f) if w > f),
            "{err}"
        );
        // a tiny want passes on any real filesystem
        preflight_disk(dir.path(), 1).expect("1 byte fits");
    }

    /// The unified availability keeps its round-trip under the
    /// install module's dependencies too.
    #[test]
    fn availability_aliases_align() {
        assert_eq!(Availability::NeedsDownload.as_str(), "needs_download");
    }
}

/// The floravox source (#102 Phase 1, experimental engine): catalogue
/// rows from the shared registry, every entry badged experimental.
#[cfg(feature = "floravox")]
#[test]
fn floravox_catalogue_is_experimental() {
    let _env = crate::local_models::test_util::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let prior = std::env::var_os("TTS_MODELS_DIR");
    std::env::set_var("TTS_MODELS_DIR", tmp.path());
    let entries = crate::floravox_model_source::FLORAVOX_SOURCE.list();
    match prior {
        Some(p) => std::env::set_var("TTS_MODELS_DIR", p),
        None => std::env::remove_var("TTS_MODELS_DIR"),
    }
    assert!(!entries.is_empty(), "the registry carries floravox rows");
    assert!(
        entries.len() >= 19,
        "students + patched voices (got {})",
        entries.len()
    );
    for e in &entries {
        assert_eq!(e.engine, "floravox");
        assert!(e.experimental, "{}: experimental badge", e.id);
        // nothing installed in the temp dir: installable or blocked,
        // never ready
        assert_ne!(e.availability, Availability::Ready);
    }
    // the JSON carries the badge
    let j = entries[0].to_json();
    assert_eq!(j["experimental"], serde_json::json!(true));
    // the shared (sherpa+floravox) patched rows resolve under BOTH ids
    assert!(crate::floravox_model_source::FLORAVOX_SOURCE
        .entry("willwade-patched-en_GB-alan-medium")
        .is_some());
    assert_eq!(
        crate::floravox_model_source::FLORAVOX_SOURCE.engine_id(),
        "floravox"
    );
}

/// The omnivoice source (#102 Phase 2): the published GGUF pair,
/// catalogue + two-file install, every byte checksum-gated.
#[cfg(feature = "omnivoice-local")]
#[test]
fn omnivoice_catalogue_and_install_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = crate::local_models::test_util::env_lock();
    let prior = std::env::var_os("TTS_MODELS_DIR");
    std::env::set_var("TTS_MODELS_DIR", tmp.path());
    let source = &crate::omnivoice_model_source::OMNIVOICE_SOURCE;
    let entries = source.list();
    match prior {
        Some(p) => std::env::set_var("TTS_MODELS_DIR", p),
        None => std::env::remove_var("TTS_MODELS_DIR"),
    }
    // the registry (1.3.0) carries exactly one omnivoice row
    assert_eq!(entries.len(), 1, "{entries:?}");
    let e = &entries[0];
    assert_eq!(e.id, "omnivoice-base-q8_0");
    assert_eq!(e.engine, "omnivoice-local");
    assert_eq!(e.availability, Availability::NeedsDownload);
    assert!(!e.installed);
    assert!(!e.experimental);
    // the download total covers base + codec
    assert!(e.filesize_mb > 1300.0, "total MB: {}", e.filesize_mb);
    // the models dir honors TTS_MODELS_DIR + the engine segment
    let dir = source.models_dir();
    assert!(dir.ends_with("omnivoice-local"), "{}", dir.display());
    // uninstall of nothing is ModelNotFound
    assert!(matches!(
        source.uninstall("omnivoice-base-q8_0"),
        Err(InstallError::ModelNotFound)
    ));
    // an unknown id never resolves
    assert!(source.entry("zzz").is_none());
}
