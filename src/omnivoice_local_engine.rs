//! OmniVoice local engine: offline TTS + voice cloning via the
//! ServeurpersoCom/omnivoice.cpp C ABI (GGML, GGUF models, 600+ languages).
//!
//! # Setup
//! Build the runtime (`-DOMNIVOICE_SHARED=ON`) and point `model_path` /
//! `codec_path` at the GGUFs from `Serveurperso/OmniVoice-GGUF`.
//! See VOICE_CLONING_PLAN.md → "OmniVoice spike notes".
//!
//! # Duration is load-bearing for cloning
//! The built-in duration estimator over-allocates in clone mode and the
//! diffusion model fills the excess frames with reference-like content.
//! This engine always computes and passes `T_override` from a syllable
//! prior (or `duration_override`), and couples `rate` into duration:
//! `rate > 1.0` shortens the output.
//!
//! # Streaming
//! **Not yet implemented.** The C ABI exposes `on_chunk` but this engine
//! currently uses the buffered path: the full audio is synthesised, then
//! delivered through `on_audio` in ~150 ms chunks. Streaming requires a
//! trampoline for the C callback (the `on_chunk_user_data` field is
//! available); tracked as a follow-up.
//!
//! # SSML
//! OmniVoice has no SSML parser. Input matching `<speak>` is
//! interpreted locally: `<say-as>` variants expand via
//! [`crate::ssml_expand`], `<sub alias>` substitutes, and tags are
//! stripped — the same pipeline as qwen3-local.
//!
//! # Word timing
//! No per-word timestamps from the runtime. Boundaries are
//! duration-anchored estimates from [`crate::engine::weighted_boundaries_scaled`]
//! (syllable-weighted, mapped onto the exact output duration).
//!
//! # Rate, pitch, volume
//! **rate** is coupled into the duration prior (rate 2.0 ≈ half-length
//! output). **pitch** has no pipeline control and is ignored. **volume**
//! is linear PCM gain (clamped 0–4×) applied before delivery.

use crate::engine::TtsEngine;
use crate::omnivoice_local_support::{
    bcp47_to_omnivoice_lang, estimate_duration_secs, load_wav_24k_mono, omnivoice_language_codes,
    ov_audio, ov_audio_free, ov_context, ov_duration_sec_to_tokens, ov_free, ov_init,
    ov_init_default_params, ov_init_params, ov_last_error, ov_synthesize, ov_tts_default_params,
    ov_tts_params, OmniVoiceError, OV_ABI_VERSION, OV_STATUS_OK,
};
use crate::types::{Gender, TtsError, TtsResult, Voice};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

static OV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn ov_lock() -> &'static Mutex<()> {
    OV_LOCK.get_or_init(|| Mutex::new(()))
}

pub struct OmniVoiceLocalEngine {
    ctx: *mut ov_context,
    params: OmniVoiceLocalParams,
    /// Cached reference audio (24 kHz mono f32) + transcript for cloning
    ref_audio: Option<Vec<f32>>,
    ref_text: Option<String>,
    /// Cooperative cancellation flag (polled via ov_cancel_cb)
    cancel_requested: AtomicBool,
}

#[derive(Clone, Debug)]
pub struct OmniVoiceLocalParams {
    /// Path to the base model GGUF (omnivoice-base-Q8_0.gguf)
    pub model_path: String,
    /// Path to the codec GGUF (omnivoice-tokenizer-F32.gguf)
    pub codec_path: String,
    /// MaskGIT sampling steps (default 32; 16 for faster inference)
    pub num_steps: i32,
    /// Seed for stochastic sampling (default 42)
    pub seed: u64,
    /// Voice-design attribute string ("female, child, british accent");
    /// validated against the model's token vocabulary by the runtime
    pub instruct: Option<String>,
    /// Override the syllable-prior duration (seconds); None = auto
    pub duration_override: Option<f32>,
    /// Language name or ISO code ("" = auto-detect). Per-call override
    /// is available by passing a BCP-47 voice name to speak().
    pub language: String,
}

impl Default for OmniVoiceLocalParams {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            codec_path: String::new(),
            num_steps: 32,
            seed: 42,
            instruct: None,
            duration_override: None,
            language: String::new(),
        }
    }
}

impl OmniVoiceLocalEngine {
    /// Create an engine. Both `model_path` and `codec_path` are required.
    ///
    /// # Errors
    /// Returns error when paths are empty, contain null bytes, or when
    /// `ov_init` fails (bad model files, missing GGUF).
    pub fn new(params: OmniVoiceLocalParams) -> TtsResult<Self> {
        if params.model_path.is_empty() || params.codec_path.is_empty() {
            return Err(TtsError(
                "omnivoice-local: model_path and codec_path required".into(),
            ));
        }
        let model_c = CString::new(params.model_path.clone())
            .map_err(|_| TtsError("omnivoice-local: model path contains null".into()))?;
        let codec_c = CString::new(params.codec_path.clone())
            .map_err(|_| TtsError("omnivoice-local: codec path contains null".into()))?;

        let mut iparams: ov_init_params = unsafe { std::mem::zeroed() };
        unsafe {
            ov_init_default_params(&raw mut iparams);
            iparams.abi_version = OV_ABI_VERSION;
            iparams.model_path = model_c.as_ptr();
            iparams.codec_path = codec_c.as_ptr();
        }

        // Lock: the runtime may not be init-thread-safe
        let guard = ov_lock().lock().unwrap_or_else(PoisonError::into_inner);
        let ctx = unsafe { ov_init(&raw const iparams) };
        drop(guard);

        if ctx.is_null() {
            return Err(TtsError("omnivoice-local: ov_init failed".into()));
        }

        Ok(Self {
            ctx,
            params,
            ref_audio: None,
            ref_text: None,
            cancel_requested: AtomicBool::new(false),
        })
    }

    /// Set the reference voice for cloning mode. `ref_wav_path` is a
    /// WAV file (any sample rate; resampled to 24 kHz mono internally).
    /// `ref_text` is the transcript of the reference audio.
    ///
    /// # Errors
    /// Returns error when the WAV file cannot be read or decoded.
    pub fn set_reference(&mut self, ref_wav_path: &str, ref_text: &str) -> TtsResult<()> {
        let samples = load_wav_24k_mono(ref_wav_path).ok_or_else(|| {
            TtsError(format!(
                "omnivoice-local: failed to load reference WAV: {ref_wav_path}"
            ))
        })?;
        if samples.is_empty() {
            return Err(TtsError(format!(
                "omnivoice-local: reference WAV is empty: {ref_wav_path}"
            )));
        }
        self.ref_audio = Some(samples);
        self.ref_text = Some(ref_text.to_string());
        Ok(())
    }

    /// Set the reference from pre-loaded 24 kHz mono f32 samples.
    ///
    /// # Errors
    /// Returns error when `samples` is empty.
    pub fn set_reference_samples(&mut self, samples: &[f32], ref_text: &str) -> TtsResult<()> {
        if samples.is_empty() {
            return Err(TtsError(
                "omnivoice-local: reference samples are empty".into(),
            ));
        }
        self.ref_audio = Some(samples.to_vec());
        self.ref_text = Some(ref_text.to_string());
        Ok(())
    }

    /// Clear the reference (switch to voice-design / default mode).
    pub fn clear_reference(&mut self) {
        self.ref_audio = None;
        self.ref_text = None;
    }

    /// Compute target duration in seconds, coupling rate into the
    /// syllable prior: rate > 1.0 = faster speech = shorter duration.
    fn target_duration(&self, text: &str, rate: f32) -> f32 {
        let base = self
            .params
            .duration_override
            .unwrap_or_else(|| estimate_duration_secs(text));
        let rate_clamped = rate.clamp(0.25, 4.0);
        (base / rate_clamped).clamp(0.3, 120.0)
    }

    /// Read the runtime's last error message (thread-local).
    fn last_error() -> String {
        unsafe {
            let ptr = ov_last_error();
            if ptr.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        }
    }

    /// C trampoline for cooperative cancellation: polled by the runtime
    /// between generation steps. Returns true to abort.
    unsafe extern "C" fn cancel_trampoline(user_data: *mut std::ffi::c_void) -> bool {
        let flag = user_data as *const AtomicBool;
        (*flag).load(Ordering::Relaxed)
    }

    #[allow(clippy::too_many_lines)]
    fn synthesize_ov(
        &self,
        text: &str,
        lang_override: Option<&str>,
        rate: f32,
    ) -> Result<Vec<f32>, OmniVoiceError> {
        let _guard = ov_lock().lock().unwrap_or_else(PoisonError::into_inner);
        // Note: reset-after-stop race — a stop() arriving just before this
        // line is erased. Acceptable: stop() targets an in-flight synthesis.
        self.cancel_requested.store(false, Ordering::Relaxed);

        let text_c =
            CString::new(text).map_err(|_| OmniVoiceError("text contains null byte".into()))?;
        let lang_str = lang_override.map_or_else(
            || self.params.language.clone(),
            std::string::ToString::to_string,
        );
        let lang_c =
            CString::new(lang_str).map_err(|_| OmniVoiceError("lang contains null byte".into()))?;
        let instruct_c = self
            .params
            .instruct
            .as_deref()
            .map(|s| {
                CString::new(s).map_err(|_| OmniVoiceError("instruct contains null byte".into()))
            })
            .transpose()?;

        // Duration from the syllable prior + rate coupling — the
        // critical clone-quality knob. Convert to frame count via the
        // runtime's own sec→tokens helper (handles codec hop size).
        let duration = self.target_duration(text, rate);
        let t_override = unsafe { ov_duration_sec_to_tokens(self.ctx, duration) };
        if t_override <= 0 {
            return Err(OmniVoiceError("duration→tokens conversion failed".into()));
        }

        let (ref_ptr, ref_n, ref_text_c) = match (&self.ref_audio, &self.ref_text) {
            (Some(audio), Some(text)) if !audio.is_empty() => {
                let rc = CString::new(text.as_str())
                    .map_err(|_| OmniVoiceError("ref text null".into()))?;
                (audio.as_ptr(), audio.len() as i32, Some(rc))
            }
            _ => (std::ptr::null(), 0, None),
        };

        let mut params: ov_tts_params = unsafe { std::mem::zeroed() };
        unsafe {
            ov_tts_default_params(&raw mut params);
            params.abi_version = OV_ABI_VERSION;
            params.text = text_c.as_ptr();
            params.lang = lang_c.as_ptr();
            params.instruct = instruct_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            params.T_override = t_override;
            params.mg_num_step = self.params.num_steps;
            params.mg_seed = self.params.seed;
            params.ref_audio_24k = ref_ptr;
            params.ref_n_samples = ref_n;
            params.ref_text = ref_text_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            // Cooperative cancellation
            params.cancel = Some(Self::cancel_trampoline);
            params.cancel_user_data = &raw const self.cancel_requested as *mut std::ffi::c_void;
        }

        let mut audio = ov_audio {
            samples: std::ptr::null(),
            n_samples: 0,
            sample_rate: 0,
            channels: 0,
        };

        let status = unsafe { ov_synthesize(self.ctx, &raw const params, &raw mut audio) };
        if status != OV_STATUS_OK {
            let detail = Self::last_error();
            return Err(OmniVoiceError(format!(
                "ov_synthesize failed (status {status}): {detail}"
            )));
        }

        let n = audio.n_samples as usize;
        if n == 0 || audio.samples.is_null() {
            unsafe { ov_audio_free(&raw mut audio) };
            return Err(OmniVoiceError("empty audio output".into()));
        }

        let samples = unsafe { std::slice::from_raw_parts(audio.samples, n) }.to_vec();
        unsafe { ov_audio_free(&raw mut audio) };
        Ok(samples)
    }
}

/// SSML-in: strip/interpret for engines without SSML parsing.
fn plain_text_for_omnivoice(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.to_ascii_lowercase().starts_with("<speak") {
        if let Some(expanded) = crate::ssml_expand::expand_ssml_to_text(trimmed) {
            return expanded;
        }
        return crate::engine::strip_ssml_to_text(trimmed);
    }
    text.to_string()
}

impl TtsEngine for OmniVoiceLocalEngine {
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        volume: f32,
        on_audio: Option<crate::engine::OnAudioCallback>,
        on_boundary: Option<crate::engine::OnBoundaryCallback>,
        _on_mark: Option<crate::engine::OnMarkCallback>,
    ) -> TtsResult<()> {
        // SSML interpretation (say-as expansion, sub alias, phoneme display)
        let plain = plain_text_for_omnivoice(text);

        // Per-call language override: voice param carries a BCP-47 code
        let lang_override = voice.map(bcp47_to_omnivoice_lang);
        let lang_opt = lang_override.as_deref().filter(|s| !s.is_empty());

        let pcm_f32 = self
            .synthesize_ov(&plain, lang_opt, rate)
            .map_err(|e| TtsError(format!("omnivoice-local: {e}")))?;

        // Volume: linear PCM gain (clamped)
        let pcm_f32: Vec<f32> = if (volume - 1.0).abs() > f32::EPSILON {
            let g = volume.clamp(0.0, 4.0);
            pcm_f32.iter().map(|s| (s * g).clamp(-1.0, 1.0)).collect()
        } else {
            pcm_f32
        };

        // Convert to PCM16 bytes for delivery (~150 ms chunks)
        let pcm16: Vec<i16> = pcm_f32
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * 32_767.0) as i16)
            .collect();
        let bytes: Vec<u8> = pcm16.iter().flat_map(|s| s.to_le_bytes()).collect();

        if let Some(cb) = on_audio {
            for chunk in bytes.chunks(7200) {
                cb(chunk);
            }
        }

        // Duration-anchored word boundaries (syllable-weighted)
        if let Some(cb) = on_boundary {
            let total_secs = pcm_f32.len() as f32 / 24_000.0;
            let boundaries = crate::engine::weighted_boundaries_scaled(&plain, total_secs);
            for b in &boundaries {
                let start = b.offset as f32 / 1000.0;
                let end = (b.offset + b.duration) as f32 / 1000.0;
                cb(&b.text, start, end, -1, -1, true);
            }
        }

        Ok(())
    }

    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<crate::engine::OnAudioCallback>,
        on_boundary: Option<crate::engine::OnBoundaryCallback>,
        on_mark: Option<crate::engine::OnMarkCallback>,
    ) -> TtsResult<()> {
        self.speak(
            text,
            voice,
            rate,
            pitch,
            volume,
            on_audio,
            on_boundary,
            on_mark,
        )
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        Ok(vec![Voice {
            id: "omnivoice-local".to_string(),
            name: "OmniVoice Local (600+ languages, offline)".to_string(),
            gender: Gender::Unknown,
            provider: "omnivoice".to_string(),
            language_codes: omnivoice_language_codes(),
        }])
    }

    fn engine_id(&self) -> &'static str {
        "omnivoice-local"
    }

    fn stop(&self) -> TtsResult<()> {
        self.cancel_requested.store(true, Ordering::Relaxed);
        Ok(())
    }
}

impl std::fmt::Debug for OmniVoiceLocalEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OmniVoiceLocalEngine")
            .field("params", &self.params)
            .field("has_ref", &self.ref_audio.is_some())
            .field("ref_text", &self.ref_text)
            .field("ctx_loaded", &!self.ctx.is_null())
            .field(
                "cancel_requested",
                &self.cancel_requested.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl Drop for OmniVoiceLocalEngine {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            let _guard = ov_lock().lock().unwrap_or_else(PoisonError::into_inner);
            unsafe { ov_free(self.ctx) };
            self.ctx = std::ptr::null_mut();
        }
    }
}

// SAFETY: the C context is used behind a global mutex; the struct is
// never shared across threads without the lock.
unsafe impl Send for OmniVoiceLocalEngine {}
unsafe impl Sync for OmniVoiceLocalEngine {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_default() {
        let p = OmniVoiceLocalParams::default();
        assert!(p.model_path.is_empty());
        assert_eq!(p.num_steps, 32);
        assert_eq!(p.seed, 42);
    }

    #[test]
    fn engine_new_requires_paths() {
        let result = OmniVoiceLocalEngine::new(OmniVoiceLocalParams::default());
        assert!(result.is_err());
    }

    #[test]
    fn ssml_interpretation() {
        let out = plain_text_for_omnivoice(
            "<speak>Hello <say-as interpret-as=\"cardinal\">42</say-as></speak>",
        );
        assert_eq!(out, "Hello forty two");
    }

    #[test]
    fn plain_text_passthrough() {
        assert_eq!(plain_text_for_omnivoice("Hello world"), "Hello world");
    }

    #[test]
    fn set_reference_missing_file_errors() {
        assert!(crate::omnivoice_local_support::load_wav_24k_mono("/nonexistent.wav").is_none());
    }

    #[test]
    fn target_duration_rate_couples() {
        let engine = OmniVoiceLocalEngine {
            ctx: std::ptr::null_mut(),
            params: OmniVoiceLocalParams {
                model_path: String::new(),
                codec_path: String::new(),
                duration_override: Some(4.0),
                ..Default::default()
            },
            ref_audio: None,
            ref_text: None,
            cancel_requested: AtomicBool::new(false),
        };
        let normal = engine.target_duration("test", 1.0);
        let fast = engine.target_duration("test", 2.0);
        assert!(
            (fast - normal / 2.0).abs() < 0.01,
            "fast {fast} vs {normal}"
        );
    }
}
