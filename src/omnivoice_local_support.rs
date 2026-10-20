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

/// omnivoice.h ABI version this binding targets (read at ov_init /
/// ov_synthesize; fields beyond the caller's version are ignored).
pub const OV_ABI_VERSION: c_int = 4;

// ── Opaque handle ────────────────────────────────────────────────────
#[repr(C)]
pub struct ov_context {
    _private: [u8; 0],
}

// ── Status: the C side returns NEGATIVE codes on error; a fieldless
// repr(C) enum with only non-negative variants would be UB. Use c_int
// constants matching the header exactly. ─────────────────────────────
pub type ov_status = c_int;
pub const OV_STATUS_OK: c_int = 0;
pub const OV_STATUS_ERROR: c_int = -1;
pub const OV_STATUS_INVALID_PARAMS: c_int = -1;
pub const OV_STATUS_INSTRUCT_INVALID: c_int = -2;
pub const OV_STATUS_GENERATE_FAILED: c_int = -3;
pub const OV_STATUS_OUT_OF_MEMORY: c_int = -4;
pub const OV_STATUS_CANCELLED: c_int = -5;

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
    pub fn ov_language_name(id: *const c_char) -> *const c_char;
    pub fn ov_duration_sec_to_tokens(ctx: *const ov_context, seconds: f32) -> c_int;
}

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

/// Curated BCP-47 codes for common OmniVoice languages (metadata for
/// get_voices). The runtime resolves any ISO code or language name at
/// synthesis time via its 600+ language table.
#[must_use]
pub fn omnivoice_language_codes() -> Vec<crate::types::LanguageCode> {
    [
        ("en", "English"),
        ("en-GB", "English (British)"),
        ("en-US", "English (American)"),
        ("fr", "French"),
        ("de", "German"),
        ("es", "Spanish"),
        ("it", "Italian"),
        ("pt", "Portuguese"),
        ("ru", "Russian"),
        ("zh", "Chinese"),
        ("ja", "Japanese"),
        ("ko", "Korean"),
        ("hi", "Hindi"),
        ("ar", "Arabic"),
        ("tr", "Turkish"),
        ("pl", "Polish"),
        ("nl", "Dutch"),
        ("sv", "Swedish"),
        ("da", "Danish"),
        ("no", "Norwegian"),
        ("fi", "Finnish"),
        ("cs", "Czech"),
        ("el", "Greek"),
        ("he", "Hebrew"),
        ("th", "Thai"),
        ("vi", "Vietnamese"),
        ("id", "Indonesian"),
        ("ms", "Malay"),
        ("uk", "Ukrainian"),
        ("ro", "Romanian"),
        ("hu", "Hungarian"),
        ("bg", "Bulgarian"),
        ("hr", "Croatian"),
        ("sk", "Slovak"),
        ("sl", "Slovenian"),
        ("sr", "Serbian"),
        ("lt", "Lithuanian"),
        ("lv", "Latvian"),
        ("et", "Estonian"),
        ("is", "Icelandic"),
        ("ga", "Irish"),
        ("cy", "Welsh"),
        ("sw", "Swahili"),
        ("af", "Afrikaans"),
        ("bn", "Bengali"),
        ("ta", "Tamil"),
        ("te", "Telugu"),
        ("kn", "Kannada"),
        ("ml", "Malayalam"),
        ("mr", "Marathi"),
        ("gu", "Gujarati"),
        ("pa", "Punjabi"),
        ("ur", "Urdu"),
        ("si", "Sinhala"),
        ("my", "Burmese"),
        ("km", "Khmer"),
        ("lo", "Lao"),
        ("ne", "Nepali"),
        ("fa", "Persian"),
        ("ps", "Pashto"),
        ("az", "Azerbaijani"),
        ("kk", "Kazakh"),
        ("ky", "Kyrgyz"),
        ("uz", "Uzbek"),
    ]
    .iter()
    .map(|(code, name)| crate::types::LanguageCode {
        bcp47: (*code).to_string(),
        iso639_3: String::new(),
        display: (*name).to_string(),
    })
    .collect()
}

/// Map a BCP-47 code to the OmniVoice language parameter. Returns ""
/// (auto-detect) when no mapping is found — the runtime's 600+ table
/// handles most names/ISO codes natively.
#[must_use]
pub fn bcp47_to_omnivoice_lang(bcp47: &str) -> String {
    let segments: Vec<&str> = bcp47.split('-').collect();
    let base = segments.first().copied().unwrap_or("").to_ascii_lowercase();
    let region = segments.get(1).copied().unwrap_or("").to_ascii_lowercase();
    match base.as_str() {
        "en" => match region.as_str() {
            "gb" | "uk" => "English (British)",
            "us" => "English (American)",
            "au" => "English (Australian)",
            _ => "English",
        }
        .to_string(),
        // Pass the ISO subtag directly — the runtime's 600+ table
        // resolves it. This makes voice="fr-FR" actually select French
        // instead of falling through to auto-detect.
        _ => base,
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
        assert_eq!(bcp47_to_omnivoice_lang("en-GB"), "English (British)");
        assert_eq!(bcp47_to_omnivoice_lang("en-US"), "English (American)");
        assert_eq!(bcp47_to_omnivoice_lang("en"), "English");
        assert_eq!(bcp47_to_omnivoice_lang("fr-FR"), "fr"); // ISO subtag passed through
                                                            // "en-custom" must NOT match "us" (the old substring bug)
        assert_eq!(bcp47_to_omnivoice_lang("en-custom"), "English");
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
