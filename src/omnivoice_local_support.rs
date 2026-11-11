//! Shared FFI + helpers for the OmniVoice local engine
//! (ServeurpersoCom/omnivoice.cpp — C ABI, offline, GGUF models).
//!
//! Build the runtime with `-DOMNIVOICE_SHARED=ON` and point
//! `OMNIVOICE_LIB` at the repo root. See VOICE_CLONING_PLAN.md for
//! the full spike notes including the critical `--duration` finding.
//!
//! FFI layouts verified field-for-field against omnivoice.h (ABI v4).
//! Layout assertions at the bottom guard against silent drift.

#![allow(
    non_camel_case_types,
    non_snake_case,
    dead_code,
    clippy::too_many_lines
)]

use std::ffi::{c_char, c_int, c_void};
use std::fmt;
use std::sync::OnceLock;

/// omnivoice.h ABI version this binding targets (read at ov_init /
/// ov_synthesize; fields beyond the caller's version are ignored).
pub(crate) const OV_ABI_VERSION: c_int = 4;

// ── Opaque handle ────────────────────────────────────────────────────
#[repr(C)]
pub struct ov_context {
    _private: [u8; 0],
}

// ── Status: the C side returns NEGATIVE codes on error; a fieldless
// repr(C) enum with only non-negative variants would be UB. Use c_int
// constants matching the header exactly. ─────────────────────────────
pub type ov_status = c_int;
pub(crate) const OV_STATUS_OK: c_int = 0;
pub(crate) const OV_STATUS_ERROR: c_int = -1;
pub(crate) const OV_STATUS_INVALID_PARAMS: c_int = -1;
pub(crate) const OV_STATUS_INSTRUCT_INVALID: c_int = -2;
pub(crate) const OV_STATUS_GENERATE_FAILED: c_int = -3;
pub(crate) const OV_STATUS_OUT_OF_MEMORY: c_int = -4;
pub(crate) const OV_STATUS_CANCELLED: c_int = -5;

// ── Callbacks ────────────────────────────────────────────────────────
pub type ov_cancel_cb = Option<unsafe extern "C" fn(user_data: *mut c_void) -> bool>;
pub type ov_audio_chunk_cb =
    Option<unsafe extern "C" fn(samples: *const f32, n: c_int, user_data: *mut c_void) -> bool>;

// ── Structs (exact v4 layouts from omnivoice.h) ─────────────────────
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ov_audio {
    pub samples: *const f32,
    pub n_samples: c_int,
    pub sample_rate: c_int,
    pub channels: c_int,
}

/// omnivoice.h v4: 40 bytes on x86-64. `use_fa` / `clamp_fp16` are
/// packed bools at offsets 24/25; `device` selects the ggml backend.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ov_init_params {
    pub abi_version: c_int,
    pub model_path: *const c_char,
    pub codec_path: *const c_char,
    pub use_fa: bool,
    pub clamp_fp16: bool,
    pub device: *const c_char,
}

/// omnivoice.h v4: 27 fields. Order matters — verified field-for-field.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ov_tts_params {
    pub abi_version: c_int,
    pub text: *const c_char,
    pub lang: *const c_char,
    pub instruct: *const c_char,
    pub T_override: c_int,
    pub chunk_duration_sec: f32,
    pub chunk_threshold_sec: f32,
    pub denoise: bool,
    pub preprocess_prompt: bool,
    pub mg_num_step: c_int,
    pub mg_guidance_scale: f32,
    pub mg_t_shift: f32,
    pub mg_layer_penalty_factor: f32,
    pub mg_position_temperature: f32,
    pub mg_class_temperature: f32,
    pub mg_seed: u64,
    pub ref_audio_tokens: *const i32,
    pub ref_T: c_int,
    pub ref_audio_24k: *const f32,
    pub ref_n_samples: c_int,
    pub ref_text: *const c_char,
    pub dump_dir: *const c_char,
    pub cancel: ov_cancel_cb,
    pub cancel_user_data: *mut c_void,
    pub on_chunk: ov_audio_chunk_cb,
    pub on_chunk_user_data: *mut c_void,
    pub postproc: bool,
}

// Wrapped in a macro: cbindgen's synth parser cannot expand macro_rules!,
// keeping these external-library bindings out of the generated C header
// (same pattern as qwen3-local).
macro_rules! omnivoice_ffi {
    () => {
        extern "C" {
            pub fn ov_init_default_params(p: *mut ov_init_params);
            pub fn ov_init(params: *const ov_init_params) -> *mut ov_context;
            pub fn ov_free(ctx: *mut ov_context);
            pub fn ov_last_error() -> *const c_char;
            pub fn ov_tts_default_params(p: *mut ov_tts_params);
            pub fn ov_synthesize(
                ctx: *mut ov_context,
                params: *const ov_tts_params,
                out: *mut ov_audio,
            ) -> ov_status;
            pub fn ov_audio_free(audio: *mut ov_audio);
            pub fn ov_n_languages() -> c_int;
            pub fn ov_language_id(index: c_int) -> *const c_char;
            // NOTE: the runtime's ov_language_name takes the same INDEX
            // as ov_language_id (name/id are two views of one table) —
            // verified against omnivoice.cpp src/omnivoice.h (ABI v4).
            // An earlier revision declared (id: *const c_char); the
            // mismatched ABI truncates the pointer into an index (UB).
            pub fn ov_language_name(index: c_int) -> *const c_char;
            pub fn ov_duration_sec_to_tokens(ctx: *const ov_context, seconds: f32) -> c_int;
        }
    };
}

omnivoice_ffi!();

// ── Layout assertions (x86-64) ──────────────────────────────────────
// These catch any drift between the Rust and C layouts — the exact
// class of bug where a wrong struct silently corrupts neighbouring
// fields (e.g. writing `n_threads` over `use_fa` + `clamp_fp16`).
#[cfg(target_arch = "x86_64")]
mod layout_asserts {
    use super::{ov_audio, ov_init_params, ov_tts_params};
    const _: () = assert!(std::mem::size_of::<ov_audio>() == 24);
    const _: () = assert!(std::mem::size_of::<ov_init_params>() == 40);
    // ov_tts_params: 27 fields, ABI v4
    const _: () = assert!(std::mem::size_of::<ov_tts_params>() == 168);
}

// ── Safe wrappers ────────────────────────────────────────────────────

#[derive(Debug)]
pub struct OmniVoiceError(pub String);

impl fmt::Display for OmniVoiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Estimate target duration in seconds from the plain text using the
/// same syllable-weight prior as the qwen3-local engine. CRITICAL for
/// clone mode: without a tight duration the diffusion model bleeds
/// reference-like content into the excess frames.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn estimate_duration_secs(text: &str) -> f32 {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return 1.0;
    }
    let total_syllables: f64 = words.iter().map(|w| crate::engine::word_weight(w)).sum();
    let secs = total_syllables * 0.26;
    secs.clamp(0.5, 60.0) as f32
}

/// Load a WAV file as 24 kHz mono f32 samples for the reference path.
/// Returns `None` on any read/decode error (callers should surface it).
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn load_wav_24k_mono(path: &str) -> Option<Vec<f32>> {
    let reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(Result::ok)
            .collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(Result::ok)
                .map(|s| s as f32 / max)
                .collect()
        }
    };
    // mono downmix
    let ch = spec.channels.max(1) as usize;
    let mono: Vec<f32> = samples
        .chunks(ch)
        .map(|c| c.iter().sum::<f32>() / ch as f32)
        .collect();
    // resample to 24 kHz if needed (linear; fine for a reference)
    if spec.sample_rate == 24_000 {
        return Some(mono);
    }
    let ratio = 24_000.0 / f64::from(spec.sample_rate);
    let n_out = (f64::from(mono.len() as u32) * ratio) as usize;
    Some(
        (0..n_out)
            .map(|i| {
                let src = f64::from(i as u32) / ratio;
                let i0 = src.floor() as usize;
                let i1 = (i0 + 1).min(mono.len().saturating_sub(1));
                let frac = (src - src.floor()) as f32;
                mono[i0] * (1.0 - frac) + mono[i1] * frac
            })
            .collect(),
    )
}

// ── Language support ────────────────────────────────────────────────

/// The runtime's own accepted-language table, enumerated once via the
/// omnivoice.cpp introspection API (`ov_n_languages` /
/// `ov_language_id` / `ov_language_name`) and cached (GH #64).
///
/// The wrapper used to advertise a hand-curated BCP-47 list that
/// drifted from what `ov_synthesize` actually accepts — e.g. `ar` was
/// listed but the runtime answers "unknown language", so AAC users
/// could select a voice that hard-fails at synthesis. Sourcing the
/// list from the runtime means advertised == accepted by construction.
///
/// Entries are `(id, display_name)` as the runtime reports them. The
/// table needs no context (it is static in the runtime), so this is
/// safe to call before `ov_init`. Returns an empty table if the
/// runtime reports none (a broken build); callers degrade gracefully.
#[must_use]
pub fn omnivoice_runtime_languages() -> &'static Vec<(String, String)> {
    static TABLE: OnceLock<Vec<(String, String)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out = Vec::new();
        // SAFETY: the ov_* functions are plain C ABI calls; none of the
        // outputs outlive the call (we copy immediately).
        let n = unsafe { ov_n_languages() };
        if n <= 0 {
            eprintln!(
                "omnivoice-local: runtime reported {n} languages; \
                 advertising an empty language list (GH #64)"
            );
            return out;
        }
        for i in 0..n {
            // SAFETY: index < count returned by the runtime.
            let id_ptr = unsafe { ov_language_id(i) };
            if id_ptr.is_null() {
                continue;
            }
            // SAFETY: the runtime returns a static C string.
            let id = unsafe { std::ffi::CStr::from_ptr(id_ptr) }
                .to_string_lossy()
                .into_owned();
            if id.is_empty() {
                continue;
            }
            // ov_language_name takes the SAME INDEX (name/id are two
            // views of one table) — see the FFI note above.
            // SAFETY: index < count returned by the runtime.
            let name_ptr = unsafe { ov_language_name(i) };
            let name = if name_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: the runtime returns a static C string.
                unsafe { std::ffi::CStr::from_ptr(name_ptr) }
                    .to_string_lossy()
                    .into_owned()
            };
            out.push((id, name));
        }
        out
    })
}

/// Language codes for `get_voices`: exactly the languages the runtime
/// accepts, as the runtime reports them (GH #64). The runtime's table
/// (omnivoice.cpp `lang-map.h`) is a name→id map — ids are bare ISO
/// 639-3 codes (`en`, `pt`, `zh`, `arb`, …, 646 of them); display
/// names come from `ov_language_name`. Every advertised entry is one
/// the runtime will synthesize, which is the contract `get_voices`
/// makes.
///
/// Falls back to a small verified core (`en`/`fr`/`de`/`es`…) when the
/// runtime reports an empty table, so a voice listing is never silently
/// empty on a degraded build. The fallback only contains ids present
/// in the real runtime's table (verified against `lang-map.h`) — it
/// must never reintroduce the advertised-but-rejected bug class.
#[must_use]
pub fn omnivoice_language_codes() -> Vec<crate::types::LanguageCode> {
    let table = omnivoice_runtime_languages();
    let entries: Vec<(String, String)> = if table.is_empty() {
        warn_once_fallback();
        CORE_VERIFIED_LANGUAGES
            .iter()
            .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
            .collect()
    } else {
        table.clone()
    };
    entries
        .into_iter()
        .map(|(id, name)| crate::types::LanguageCode {
            bcp47: id,
            iso639_3: String::new(),
            display: name,
        })
        .collect()
}

/// Log the degraded-table fallback exactly once (get_voices may be
/// called repeatedly).
fn warn_once_fallback() {
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!(
            "omnivoice-local: empty runtime language table, falling back to \
             the verified core list (GH #64)"
        );
    });
}

/// Languages verified against the real runtime's `lang-map.h` table
/// (GH #64 / PR #63 probes). Only used when the runtime's own table is
/// unavailable. NOTE: the runtime has no regional ids (`pt-br`,
/// `zh-cn` do not exist) and no macrolanguage `ar` (only `arb`,
/// `arq`, …) — nothing here may claim otherwise.
const CORE_VERIFIED_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("fr", "French"),
    ("de", "German"),
    ("es", "Spanish"),
    ("pt", "Portuguese"),
    ("zh", "Chinese"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("hi", "Hindi"),
];

/// Map a BCP-47 code to the OmniVoice language parameter (GH #63/#64).
///
/// Resolution order, all against the runtime's own accepted table:
/// 1. Exact match on the full lowercased code (defensive: the current
///    runtime table has no regional ids — verified against
///    lang-map.h — but honour one if a future table adds it).
/// 2. Base ISO subtag match (`en-GB` → `en`) — the runtime's table has
///    no en-XX entries (#63).
/// 3. Macrolanguage fixup (GH #64): BCP-47 `ar` has no matching id in
///    the runtime's table (which uses ISO 639-3 individual codes like
///    `arb` "standard arabic"); map the known macrolanguages to their
///    primary individual code so the user's selection is honoured
///    instead of falling through to auto-detect.
/// 4. Bare base subtag passthrough when nothing matched — the runtime
///    resolves most ISO codes and names natively; region subtags map
///    to accent instructs separately (see [`bcp47_region_to_instruct`]).
#[must_use]
pub fn bcp47_to_omnivoice_lang(bcp47: &str) -> String {
    let table = omnivoice_runtime_languages();
    let normalized = bcp47.trim().to_ascii_lowercase();
    if table.is_empty() {
        return macrolanguage_fixup(normalized.split(['-', '_']).next().unwrap_or(""), &[]);
    }
    // Exact id match (defensive: the current runtime table has no
    // regional ids, but honour one if a future table adds them).
    if table.iter().any(|(id, _)| *id == normalized) {
        return normalized;
    }
    // Base subtag match (en-GB → en, fr-CA → fr).
    let base = normalized
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_string();
    if table.iter().any(|(id, _)| *id == base) {
        return base;
    }
    // Macrolanguage: ar → arb, but only when the runtime's table has
    // the individual code (verified against lang-map.h).
    macrolanguage_fixup(&base, table)
}

/// BCP-47 macrolanguages whose ISO 639-3 primary individual code
/// differs from the BCP-47 primary subtag. Verified against the
/// runtime's `lang-map.h` (646 ids; every other common base code is
/// present verbatim).
const MACROLANGUAGE_MAP: &[(&str, &str)] = &[("ar", "arb")];

fn macrolanguage_fixup(base: &str, table: &[(String, String)]) -> String {
    for (macro_code, individual) in MACROLANGUAGE_MAP {
        if base == *macro_code && (table.is_empty() || table.iter().any(|(id, _)| id == individual))
        {
            return (*individual).to_string();
        }
    }
    // Unknown to the table: pass the base through and let the runtime
    // decide — synthesize_ov falls back to auto-detect if it rejects
    // the language (GH #64).
    base.to_string()
}

/// Map a BCP-47 region code to an OmniVoice voice-design instruct
/// token, if the runtime's vocabulary has a matching accent. Returns
/// None when there's no accent equivalent (the region is dropped).
///
/// The runtime's valid English instruct tokens (verified):
/// british accent, american accent, australian accent, indian accent,
/// japanese accent, korean accent, chinese accent, russian accent,
/// portuguese accent.
#[must_use]
pub fn bcp47_region_to_instruct(bcp47: &str) -> Option<String> {
    let segments: Vec<&str> = bcp47.split(['-', '_']).collect();
    let base = segments.first().copied().unwrap_or("").to_ascii_lowercase();
    let region = segments.get(1).copied().unwrap_or("").to_ascii_lowercase();
    match (base.as_str(), region.as_str()) {
        // English regions → accent tokens the runtime supports
        ("en", "gb" | "uk") => Some("british accent".into()),
        ("en", "us") => Some("american accent".into()),
        ("en", "au") => Some("australian accent".into()),
        ("en", "in") => Some("indian accent".into()),
        // Non-English regions: the runtime doesn't have accent tokens
        // for fr-CA vs fr-FR etc. — drop the region, keep the base.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_ov_init_params_40_bytes() {
        assert_eq!(std::mem::size_of::<ov_init_params>(), 40);
    }

    #[test]
    fn layout_ov_tts_params_168_bytes() {
        assert_eq!(std::mem::size_of::<ov_tts_params>(), 168);
    }

    #[test]
    fn layout_ov_audio_24_bytes() {
        assert_eq!(std::mem::size_of::<ov_audio>(), 24);
    }

    #[test]
    fn bcp47_mapping_exact_regions() {
        assert_eq!(bcp47_to_omnivoice_lang("en-GB"), "en"); // bare ISO code (runtime table has no regions)
        assert_eq!(bcp47_to_omnivoice_lang("en-US"), "en");
        assert_eq!(bcp47_to_omnivoice_lang("en"), "en");
        assert_eq!(bcp47_to_omnivoice_lang("fr-FR"), "fr"); // ISO subtag passed through
                                                            // "en-custom" must NOT match "us" (the old substring bug)
        assert_eq!(bcp47_to_omnivoice_lang("en-custom"), "en");
    }

    #[test]
    fn runtime_table_is_enumerated_not_curated() {
        // GH #64: the advertised list must come from the runtime's own
        // table (ov_n_languages/ov_language_id/ov_language_name), not a
        // curated copy that can drift.
        let table = omnivoice_runtime_languages();
        assert!(!table.is_empty(), "runtime reported no languages");
        for (id, name) in table {
            assert!(!id.is_empty(), "empty id in runtime table");
            assert!(!name.is_empty(), "display name missing for {id}");
        }
    }

    #[test]
    fn advertised_codes_match_runtime_table_exactly() {
        // GH #64 core invariant: advertised == accepted. Whatever the
        // runtime table says, the voice list says — nothing more. (The
        // dev stub runtime deliberately omits "ar" while the old
        // curated table included it — the exact bug from the issue.)
        let codes = omnivoice_language_codes();
        let table = omnivoice_runtime_languages();
        let advertised: Vec<&str> = codes.iter().map(|c| c.bcp47.as_str()).collect();
        let accepted: Vec<&str> = table.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(advertised, accepted);
        // Sanity: the language the issue probed ("ar") must only be
        // advertised when the runtime accepts it.
        assert_eq!(accepted.contains(&"ar"), advertised.contains(&"ar"));
    }

    #[test]
    fn bcp47_full_id_match_beats_base_subtag() {
        // The current runtime table has no regional ids (verified
        // against lang-map.h); if a future table adds one, the exact
        // match must win over the base subtag. Guarded so the test
        // holds on both today's table and that future one.
        if omnivoice_runtime_languages()
            .iter()
            .any(|(id, _)| id == "pt-br")
        {
            assert_eq!(bcp47_to_omnivoice_lang("pt-BR"), "pt-br");
        }
        // Unknown codes degrade to the base subtag (the runtime gets
        // the final say; synthesize falls back to auto-detect).
        assert_eq!(bcp47_to_omnivoice_lang("xx-YY"), "xx");
        assert_eq!(bcp47_to_omnivoice_lang("EN_us"), "en");
    }

    #[test]
    fn macrolanguage_ar_maps_to_arb() {
        // GH #64: BCP-47 `ar` has no id in the runtime's ISO 639-3
        // table (only arb/arq/ars/ary/arz) — the mapping must honour
        // the selection with the primary individual code instead of
        // passing a code the runtime rejects.
        let table = omnivoice_runtime_languages();
        if table.iter().any(|(id, _)| id == "arb") {
            assert_eq!(bcp47_to_omnivoice_lang("ar"), "arb");
            assert_eq!(bcp47_to_omnivoice_lang("ar-EG"), "arb");
        } else {
            eprintln!("skipping: runtime table has no 'arb'");
        }
    }

    #[test]
    fn wav_loader_missing_file() {
        assert!(load_wav_24k_mono("/nonexistent/file.wav").is_none());
    }

    #[test]
    fn duration_estimation_reasonable() {
        let d = estimate_duration_secs("Hello world this is a test");
        assert!(d > 0.5 && d < 10.0, "duration {d}");
    }
}
