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
//! # Voice cloning: codes, not samples
//! The reference is codec-encoded **once** (at `set_reference` /
//! `set_reference_samples` time, via the runtime's
//! `ov_extract_voice_ref`) and the RVQ codes are cached and reused for
//! every synthesis — the per-call codec encode (HuBERT + DAC encoder +
//! RVQ, several seconds of CPU per call) is skipped. The codes are
//! bit-identical to the raw-samples path the runtime documents.
//!
//! # Streaming
//! Implemented (issue #151 P5). When the caller registers `on_audio`,
//! synthesis rides the runtime's `on_chunk` callback: each chunk is
//! converted to PCM16 and delivered as it is produced (the
//! `omnivoice_stream` trampoline). Long text (duration prior above
//! ~30 s, the runtime's chunk threshold) drops `T_override` so the
//! runtime's chunked long-form pipeline runs — progressive chunks
//! with cross-fade and speaker consistency; short text keeps the
//! tight duration prior (the clone-quality knob) and arrives as one
//! final chunk. `stop()` mid-stream truncates the utterance: the
//! already-delivered audio stands and `speak` returns `Ok` (the
//! qwen3-local precedent). Without `on_audio` the buffered path is
//! unchanged (full audio, then ~150 ms chunks).
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
    bcp47_region_to_instruct, bcp47_to_omnivoice_lang, estimate_duration_secs, load_wav_24k_mono,
    omnivoice_language_codes, ov_audio, ov_audio_free, ov_context, ov_duration_sec_to_tokens,
    ov_free, ov_init, ov_init_default_params, ov_init_params, ov_last_error, ov_synthesize,
    ov_tts_default_params, ov_tts_params, OmniVoiceError, OvVoiceRef, OV_ABI_VERSION,
    OV_STATUS_CANCELLED, OV_STATUS_OK,
};
use crate::types::{Gender, TtsError, TtsResult, Voice};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

static OV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn ov_lock() -> &'static Mutex<()> {
    OV_LOCK.get_or_init(|| Mutex::new(()))
}

/// Above this estimated duration, streaming calls drop `T_override`
/// and let the runtime run its chunked long-form pipeline. Mirrors
/// the runtime's default `chunk_threshold_sec` (30 s).
const LONG_FORM_THRESHOLD_SECS: f32 = 30.0;

/// Offline OmniVoice engine over the omnivoice.cpp C ABI: 646
/// languages, voice-design instructs, zero-shot cloning from a
/// reference WAV. One `ov_context`; calls serialized by a global lock
/// (the runtime is not thread-safe).
pub struct OmniVoiceLocalEngine {
    ctx: *mut ov_context,
    params: OmniVoiceLocalParams,
    /// Cached voice-reference codes for cloning (extracted ONCE via
    /// `ov_extract_voice_ref` at `set_reference*` time — the per-call
    /// codec encode is skipped, issue #151 P2)
    voice_ref: Option<OvVoiceRef>,
    /// Transcript of the reference audio (still sent per call: it is
    /// part of the model prompt, not the codec encode).
    ref_text: Option<String>,
    /// Cooperative cancellation flag (polled via ov_cancel_cb)
    cancel_requested: AtomicBool,
}

/// Construction and per-engine parameters for
/// [`OmniVoiceLocalEngine::new`].
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
    /// GGML backend device override ("CUDA0", "Vulkan0", "MTL0",
    /// "CPU"); None = the runtime's best-available default (issue
    /// #151 P4 — GPU machines get interactive RTF, ~RTX 5090 RTF 0.023)
    pub device: Option<String>,
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
            device: None,
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
        // GGML backend device ("CUDA0" / "Vulkan0" / "MTL0" / "CPU");
        // None = the runtime's default (best available). Issue #151 P4.
        let device_c = params
            .device
            .as_deref()
            .filter(|d| !d.is_empty())
            .map(CString::new)
            .transpose()
            .map_err(|_| TtsError("omnivoice-local: device contains null".into()))?;

        let mut iparams: ov_init_params = unsafe { std::mem::zeroed() };
        unsafe {
            ov_init_default_params(&raw mut iparams);
            iparams.abi_version = OV_ABI_VERSION;
            iparams.model_path = model_c.as_ptr();
            iparams.codec_path = codec_c.as_ptr();
            iparams.device = device_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        }

        // Lock: the runtime may not be init-thread-safe
        let guard = ov_lock().lock().unwrap_or_else(PoisonError::into_inner);
        let ctx = unsafe { ov_init(&raw const iparams) };
        drop(guard);

        if ctx.is_null() {
            // Surface the runtime's own diagnostic — with the device
            // credential a typo'd backend ("cuda0") fails exactly here
            // (loop-review finding: bare "ov_init failed" gave no hint).
            let detail = crate::omnivoice_local_support::last_error_string();
            return Err(TtsError(format!(
                "omnivoice-local: ov_init failed: {detail}"
            )));
        }

        Ok(Self {
            ctx,
            params,
            voice_ref: None,
            ref_text: None,
            cancel_requested: AtomicBool::new(false),
        })
    }

    /// Set the reference voice for cloning mode. `ref_wav_path` is a
    /// WAV file (any sample rate; resampled to 24 kHz mono internally).
    /// `ref_text` is the transcript of the reference audio.
    ///
    /// The reference is codec-encoded **once** here — the RVQ codes are
    /// cached and reused for every synthesis (bit-identical to the
    /// raw-samples path; the per-call codec encode is skipped).
    ///
    /// # Errors
    /// Returns error when the WAV file cannot be read/decoded, or when
    /// the runtime's code extraction fails.
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
        self.install_reference(&samples, ref_text)
    }

    /// Set the reference from pre-loaded 24 kHz mono f32 samples.
    /// The codec encode happens once here; see [`Self::set_reference`].
    ///
    /// # Errors
    /// Returns error when `samples` is empty or code extraction fails.
    pub fn set_reference_samples(&mut self, samples: &[f32], ref_text: &str) -> TtsResult<()> {
        if samples.is_empty() {
            return Err(TtsError(
                "omnivoice-local: reference samples are empty".into(),
            ));
        }
        self.install_reference(samples, ref_text)
    }

    /// Extract + cache the reference codes shared by both setters.
    fn install_reference(&mut self, samples: &[f32], ref_text: &str) -> TtsResult<()> {
        let guard = ov_lock().lock().unwrap_or_else(PoisonError::into_inner);
        let voice = OvVoiceRef::extract(self.ctx, samples)
            .map_err(|e| TtsError(format!("omnivoice-local: {}", e.0)))?;
        drop(guard);
        self.voice_ref = Some(voice);
        self.ref_text = Some(ref_text.to_string());
        Ok(())
    }

    /// Clear the reference (switch to voice-design / default mode).
    /// Frees the cached reference codes.
    pub fn clear_reference(&mut self) {
        self.voice_ref = None;
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
        instruct_override: Option<&str>,
        mut stream: Option<&mut crate::omnivoice_stream::OvStreamState<'_>>,
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
        let lang_c = CString::new(lang_str.clone())
            .map_err(|_| OmniVoiceError("lang contains null byte".into()))?;
        // Per-call instruct (from accent) overrides params.instruct;
        // an explicit params.instruct always wins over a derived accent.
        let effective_instruct = instruct_override
            .filter(|_| self.params.instruct.is_none())
            .or(self.params.instruct.as_deref());
        let instruct_c = effective_instruct
            .map(|s| {
                CString::new(s).map_err(|_| OmniVoiceError("instruct contains null byte".into()))
            })
            .transpose()?;

        // Duration from the syllable prior + rate coupling — the
        // critical clone-quality knob. Convert to frame count via the
        // runtime's own sec→tokens helper (handles codec hop size).
        let duration = self.target_duration(text, rate);
        // Streaming + long text: drop T_override so the runtime's
        // chunked long-form pipeline engages (progressive chunks,
        // cross-fade, speaker consistency — the upstream reference
        // behavior for long input). Buffered calls and short text
        // keep the tight prior: without it the estimator over-allocates
        // and clone output bleeds reference content. An explicit
        // `duration_override` always wins (the user asked for it).
        // Threshold mirrors the runtime's default chunk_threshold_sec
        // (30 s) so our rule and its internal rule agree.
        let long_form = stream.is_some()
            && self.params.duration_override.is_none()
            && duration > LONG_FORM_THRESHOLD_SECS;
        let t_override = if long_form {
            0
        } else {
            let t = unsafe { ov_duration_sec_to_tokens(self.ctx, duration) };
            if t <= 0 {
                return Err(OmniVoiceError("duration→tokens conversion failed".into()));
            }
            t
        };

        let (tokens_ptr, ref_t, ref_text_c) = match (&self.voice_ref, &self.ref_text) {
            (Some(voice), Some(text)) => {
                let rc = CString::new(text.as_str())
                    .map_err(|_| OmniVoiceError("ref text null".into()))?;
                (voice.codes().as_ptr(), voice.ref_t(), Some(rc))
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
            // Long-form streaming: our prior above the threshold is the
            // DECISION; zeroing the runtime's own threshold is the
            // MECHANISM. Without this the runtime re-tests its own
            // (estimator-based) T_total against chunk_threshold_sec and
            // keeps borderline texts single-shot — observed live: a
            // ~35 s-prior text chunked zero times, all chunks arriving
            // in one end-burst. threshold 0 + T_override 0 ⇒ always
            // chunked (no_chunk requires T_override > 0 OR
            // chunk_duration <= 0 OR T_total <= threshold).
            if long_form {
                params.chunk_threshold_sec = 0.0;
            }
            params.mg_num_step = self.params.num_steps;
            params.mg_seed = self.params.seed;
            // Pre-encoded reference codes (extracted once at
            // set_reference time) — skips the per-call codec encode.
            // ref_text still rides the prompt (it is text, not codec).
            params.ref_audio_tokens = tokens_ptr;
            params.ref_T = ref_t;
            params.ref_text = ref_text_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            // Cooperative cancellation
            params.cancel = Some(Self::cancel_trampoline);
            params.cancel_user_data = &raw const self.cancel_requested as *mut std::ffi::c_void;
            // SAFETY: `stream` borrows state living on speak()'s stack
            // for the whole ov_synthesize call; the runtime invokes the
            // callback synchronously on this thread and never stores
            // the pointer beyond the call.
            if let Some(state) = stream.as_deref_mut() {
                let state_ptr: *mut crate::omnivoice_stream::OvStreamState = state;
                params.on_chunk = Some(crate::omnivoice_stream::ov_chunk_trampoline);
                params.on_chunk_user_data = state_ptr.cast::<std::ffi::c_void>();
            }
        }

        let mut audio = ov_audio {
            samples: std::ptr::null(),
            n_samples: 0,
            sample_rate: 0,
            channels: 0,
        };

        let status = unsafe { ov_synthesize(self.ctx, &raw const params, &raw mut audio) };
        // Streaming + user stop: a truncated utterance is a success —
        // the already-delivered chunks stand (qwen3-local precedent).
        if status == OV_STATUS_CANCELLED && stream.is_some() {
            // `out` is unused in streaming mode; the zeroed free is a
            // documented no-op, kept for ownership clarity.
            unsafe { ov_audio_free(&raw mut audio) };
            return Ok(Vec::new());
        }
        if status != OV_STATUS_OK {
            let detail = Self::last_error();
            // GH #64: a language the runtime rejects must not silence
            // the whole utterance — fall back to auto-detection and
            // warn. A wrong-language voice beats a silent no-speak for
            // AAC users. (Single retry; auto-detect cannot itself fail
            // with "unknown language".)
            //
            // NOTE: detection is coupled to the runtime's error string
            // ("unknown language" — omnivoice.cpp formats it as
            // "ov_synthesize: unknown language '%s'"); the ABI exposes
            // no dedicated status code. If an omnivoice.cpp update
            // rewords it, this fallback silently stops firing — recheck
            // this match when bumping OV_ABI_VERSION.
            if !lang_str.is_empty() && detail.to_lowercase().contains("unknown language") {
                eprintln!(
                    "omnivoice-local: runtime rejected language '{lang_str}' \
                     ({detail}); retrying with auto-detection (GH #64)"
                );
                // lang = null → auto-detect (documented ABI default).
                params.lang = std::ptr::null();
                let retry_status =
                    unsafe { ov_synthesize(self.ctx, &raw const params, &raw mut audio) };
                if retry_status == OV_STATUS_CANCELLED && stream.is_some() {
                    unsafe { ov_audio_free(&raw mut audio) };
                    return Ok(Vec::new());
                }
                if retry_status == OV_STATUS_OK {
                    return Self::finish_streaming_or_buffered(stream, audio);
                }
                let retry_detail = Self::last_error();
                return Err(OmniVoiceError(format!(
                    "ov_synthesize failed after language fallback (status {retry_status}): {retry_detail}"
                )));
            }
            return Err(OmniVoiceError(format!(
                "ov_synthesize failed (status {status}): {detail}"
            )));
        }

        Self::finish_streaming_or_buffered(stream, audio)
    }

    /// Post-synthesis handling shared by the first attempt and the
    /// language-fallback retry: streaming mode validates delivery
    /// (the runtime leaves `out` unused when a chunk callback is
    /// registered); buffered mode copies out and frees.
    fn finish_streaming_or_buffered(
        stream: Option<&mut crate::omnivoice_stream::OvStreamState<'_>>,
        mut audio: ov_audio,
    ) -> Result<Vec<f32>, OmniVoiceError> {
        if let Some(state) = stream {
            if state.total_samples == 0 {
                // Nobody cancelled and nothing arrived — the pipeline
                // produced no audio (qwen3-local mirror).
                unsafe { ov_audio_free(&raw mut audio) };
                return Err(OmniVoiceError(
                    "streaming synthesis produced no audio".into(),
                ));
            }
            // `out` is unused in streaming mode; the free is a
            // documented no-op on the zeroed struct.
            unsafe { ov_audio_free(&raw mut audio) };
            return Ok(Vec::new());
        }
        Self::take_audio(audio)
    }
}

impl OmniVoiceLocalEngine {
    /// Copy out the synthesised samples and free the runtime buffer.
    ///
    /// # Errors
    /// When the runtime produced no samples.
    fn take_audio(mut audio: ov_audio) -> Result<Vec<f32>, OmniVoiceError> {
        let n = audio.n_samples as usize;
        if n == 0 || audio.samples.is_null() {
            // SAFETY: audio is an owned out-param; freeing a zeroed
            // buffer is a no-op in the runtime but keeps ownership clear.
            unsafe { ov_audio_free(&raw mut audio) };
            return Err(OmniVoiceError("empty audio output".into()));
        }

        // SAFETY: samples points to n f32 values owned by the runtime.
        let samples = unsafe { std::slice::from_raw_parts(audio.samples, n) }.to_vec();
        // SAFETY: audio is an owned out-param we have not freed yet.
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

        // Per-call voice→language + accent: the voice param carries a
        // BCP-47 code. The base subtag selects the language; the region
        // subtag maps to a voice-design instruct token (e.g. en-GB →
        // 'british accent') when the runtime's vocabulary supports it.
        // An explicitly-set params.instruct always wins over the derived
        // accent (the user chose it deliberately).
        // "default" (the installed-voice catalogue's id) means NO
        // language override — engine auto-detect, not a doomed native
        // call through the GH #64 retry (loop-1 review finding 6)
        let voice_sel = voice.filter(|v| !v.is_empty() && *v != "default");
        let lang_override = voice_sel.map(bcp47_to_omnivoice_lang);
        let lang_opt = lang_override.as_deref().filter(|s| !s.is_empty());
        let accent_instruct = voice_sel.and_then(bcp47_region_to_instruct);

        // Streaming route (#151 P5): with an on_audio sink, chunks are
        // converted + delivered as the runtime emits them.
        if let Some(cb) = on_audio {
            let gain = volume.clamp(0.0, 4.0);
            let mut state =
                crate::omnivoice_stream::OvStreamState::new(cb, gain, Some(&self.cancel_requested));
            self.synthesize_ov(
                &plain,
                lang_opt,
                rate,
                accent_instruct.as_deref(),
                Some(&mut state),
            )
            .map_err(|e| TtsError(format!("omnivoice-local: {e}")))?;
            // Duration-anchored word boundaries over the delivered
            // audio (estimated, as in the buffered path).
            if let Some(bcb) = on_boundary {
                let total_secs = state.total_samples as f32 / 24_000.0;
                if total_secs > 0.0 {
                    let boundaries = crate::engine::weighted_boundaries_scaled(&plain, total_secs);
                    for b in &boundaries {
                        let start = b.offset as f32 / 1000.0;
                        let end = (b.offset + b.duration) as f32 / 1000.0;
                        bcb(&b.text, start, end, -1, -1, true);
                    }
                }
            }
            return Ok(());
        }

        // Buffered route (no sink): full audio, then conversion.
        let pcm_f32 = self
            .synthesize_ov(&plain, lang_opt, rate, accent_instruct.as_deref(), None)
            .map_err(|e| TtsError(format!("omnivoice-local: {e}")))?;

        // Volume: linear PCM gain (clamped)
        let pcm_f32: Vec<f32> = if (volume - 1.0).abs() > f32::EPSILON {
            let g = volume.clamp(0.0, 4.0);
            pcm_f32.iter().map(|s| (s * g).clamp(-1.0, 1.0)).collect()
        } else {
            pcm_f32
        };

        // `on_audio` is always None on this route — every `Some` sink
        // took the streaming branch above and returned — so no PCM16
        // conversion or chunked delivery happens here (loop-review
        // finding: the old conversion + 7200-byte loop was unreachable
        // dead code).

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

    /// Model + codec paths were validated by `ov_init` at construction
    /// (GH #69): if the engine exists, the setup works — Verified.
    fn check_credentials_status(&self) -> crate::types::TtsResult<crate::engine::CredentialCheck> {
        Ok(crate::engine::CredentialCheck::Verified)
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
            .field("has_voice_ref", &self.voice_ref.is_some())
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
        // Free the cached reference codes BEFORE the runtime context:
        // drop order (ctx first below) must not leave codes dangling
        // behind a freed runtime.
        self.voice_ref = None;
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
        assert!(p.model_path.is_empty(), "default empty");
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
            voice_ref: None,
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

#[cfg(test)]
mod gh64_language_fallback {
    use super::*;

    /// Engine-level tests need `ov_init` to succeed, which requires
    /// loadable model files. Against the REAL runtime only real
    /// GGUFs work; against the dev stub (scripts/dev-stubs) any
    /// path does. Set both env vars to run:
    ///   OMNIVOICE_TEST_MODEL=/path/model.gguf
    ///   OMNIVOICE_TEST_CODEC=/path/codec.gguf
    /// (stub: point both at /dev/null). Skipped otherwise — the
    /// pure-logic GH #64 tests run unconditionally.
    fn maybe_engine_with_language(lang: &str) -> Option<OmniVoiceLocalEngine> {
        let model = std::env::var("OMNIVOICE_TEST_MODEL").ok()?;
        let codec = std::env::var("OMNIVOICE_TEST_CODEC").ok()?;
        OmniVoiceLocalEngine::new(OmniVoiceLocalParams {
            model_path: model,
            codec_path: codec,
            language: lang.into(),
            ..Default::default()
        })
        .ok()
    }

    /// GH #64: a language outside the runtime's accepted set must
    /// fall back to auto-detection instead of failing the whole
    /// utterance ("xx" is outside every table).
    #[test]
    fn rejected_language_falls_back_to_auto_detect() {
        let Some(engine) = maybe_engine_with_language("xx") else {
            eprintln!("skipping: OMNIVOICE_TEST_MODEL/CODEC not set");
            return;
        };
        let result = engine.speak("hello", None, 1.0, 1.0, 1.0, None, None, None);
        // The fallback makes the utterance succeed (wrong-language
        // audio beats a silent no-speak for AAC users).
        assert!(
            result.is_ok(),
            "auto-detect fallback must rescue an unknown language: {result:?}"
        );
    }

    /// Accepted languages synthesize without the fallback path.
    #[test]
    fn accepted_language_synthesizes_directly() {
        let Some(engine) = maybe_engine_with_language("en") else {
            eprintln!("skipping: OMNIVOICE_TEST_MODEL/CODEC not set");
            return;
        };
        assert!(engine
            .speak("hello", None, 1.0, 1.0, 1.0, None, None, None)
            .is_ok());
    }
}

#[cfg(test)]
mod voice_ref_tests {
    use super::*;

    /// Env-gated engine (stub or real GGUFs) — same pattern as the
    /// GH #64 module above.
    fn maybe_engine() -> Option<OmniVoiceLocalEngine> {
        let model = std::env::var("OMNIVOICE_TEST_MODEL").ok()?;
        let codec = std::env::var("OMNIVOICE_TEST_CODEC").ok()?;
        OmniVoiceLocalEngine::new(OmniVoiceLocalParams {
            model_path: model,
            codec_path: codec,
            ..Default::default()
        })
        .ok()
    }

    /// Issue #151 P2: setting a reference extracts codes ONCE and every
    /// synthesis sends `ref_audio_tokens` + `ref_T` — never the raw
    /// `ref_audio_24k` path. The dev stub's `ov_synthesize` contract
    /// guards fail the call (and therefore this test) if the engine
    /// ever wires the raw path or a mismatched pair back in.
    #[test]
    fn reference_sends_codes_not_samples() {
        let Some(mut engine) = maybe_engine() else {
            eprintln!("skipping: OMNIVOICE_TEST_MODEL/CODEC not set");
            return;
        };
        // ~2 seconds of dummy 24 kHz samples (stub: any non-empty input).
        let samples = vec![0.05_f32; 48_000];
        engine
            .set_reference_samples(&samples, "reference transcript")
            .expect("code extraction must succeed");
        assert!(engine.voice_ref.is_some(), "codes must be cached");
        // First and repeat synthesis both ride the codes path.
        for i in 0..2 {
            let out = engine.speak("hello again", None, 1.0, 1.0, 1.0, None, None, None);
            assert!(out.is_ok(), "synthesis {i} via cached codes: {out:?}");
        }
        // Clearing frees the codes and returns to the no-reference path.
        engine.clear_reference();
        assert!(engine.voice_ref.is_none());
        assert!(engine
            .speak("hello again", None, 1.0, 1.0, 1.0, None, None, None)
            .is_ok());
    }

    /// Empty reference samples are rejected before any FFI call.
    #[test]
    fn empty_reference_samples_error() {
        let Some(mut engine) = maybe_engine() else {
            eprintln!("skipping: OMNIVOICE_TEST_MODEL/CODEC not set");
            return;
        };
        assert!(engine.set_reference_samples(&[], "text").is_err());
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use std::cell::Cell;

    /// Env-gated engine (stub or real GGUFs) — same pattern as above.
    fn maybe_engine() -> Option<OmniVoiceLocalEngine> {
        let model = std::env::var("OMNIVOICE_TEST_MODEL").ok()?;
        let codec = std::env::var("OMNIVOICE_TEST_CODEC").ok()?;
        OmniVoiceLocalEngine::new(OmniVoiceLocalParams {
            model_path: model,
            codec_path: codec,
            ..Default::default()
        })
        .ok()
    }

    /// Issue #151 P5: with an on_audio sink, chunks arrive through the
    /// C `on_chunk` trampoline (the stub emits 4 x 0.25 s = 1 s of
    /// 24 kHz silence) and word boundaries fire over the delivered
    /// audio. The buffered path must NOT run (no full-buffer PCM).
    #[test]
    fn streaming_delivers_chunks_and_boundaries() {
        let Some(engine) = maybe_engine() else {
            eprintln!("skipping: OMNIVOICE_TEST_MODEL/CODEC not set");
            return;
        };
        let chunks = Cell::new(0usize);
        let bytes = Cell::new(0usize);
        let mut deliver = |b: &[u8]| {
            chunks.set(chunks.get() + 1);
            bytes.set(bytes.get() + b.len());
        };
        let words = Cell::new(0usize);
        let mut on_word = |_t: &str, _s: f32, _e: f32, _o: i32, _l: i32, _est: bool| {
            words.set(words.get() + 1);
        };
        engine
            .speak(
                "hello streaming world",
                None,
                1.0,
                1.0,
                1.0,
                Some(&mut deliver),
                Some(&mut on_word),
                None,
            )
            .expect("streaming speak");
        assert!(chunks.get() >= 1, "chunks delivered: {}", chunks.get());
        // stub: exactly 4 chunks x 6000 samples x 2 bytes — asserting
        // the COUNT makes this test self-sufficient against a
        // regression to buffered delivery (which would emit
        // 7 x 7200-byte chunks; loop-review finding 3)
        assert_eq!(chunks.get(), 4, "4 x 0.25 s streamed chunks");
        assert_eq!(bytes.get(), 48_000, "1 s of 24 kHz PCM16");
        assert!(words.get() >= 2, "boundaries over delivered audio");
    }

    /// A `stop()` mid-stream truncates the utterance: speak returns
    /// Ok and the chunks delivered before the stop stand (the
    /// qwen3-local precedent; the stub aborts on the callback's
    /// false, returning OV_STATUS_CANCELLED).
    #[test]
    fn stop_mid_stream_truncates_ok() {
        let Some(engine) = maybe_engine() else {
            eprintln!("skipping: OMNIVOICE_TEST_MODEL/CODEC not set");
            return;
        };
        let chunks = Cell::new(0usize);
        // The sink shares an immutable borrow of the engine's stop
        // flag with speak's `&self` — set it after the first chunk.
        let flag = &engine.cancel_requested;
        let mut deliver = |_b: &[u8]| {
            chunks.set(chunks.get() + 1);
            if chunks.get() == 1 {
                flag.store(true, Ordering::Relaxed);
            }
        };
        let result = engine.speak("hello", None, 1.0, 1.0, 1.0, Some(&mut deliver), None, None);
        assert!(result.is_ok(), "truncated stream is Ok: {result:?}");
        // Chunk 1 delivered, chunk 2's trampoline check aborts.
        assert_eq!(chunks.get(), 1, "delivered until the stop");
    }

    /// No on_audio sink: the buffered path is unchanged (full audio
    /// in one delivery).
    #[test]
    fn buffered_path_without_sink() {
        let Some(engine) = maybe_engine() else {
            eprintln!("skipping: OMNIVOICE_TEST_MODEL/CODEC not set");
            return;
        };
        // No sink: speak must still succeed via the buffered route.
        assert!(engine
            .speak("hello buffered", None, 1.0, 1.0, 1.0, None, None, None)
            .is_ok());
    }
}

#[cfg(test)]
mod nuget_build_tests {
    use super::*;

    /// GH #62: this test compiling at all proves the omnivoice-local
    /// engine is IN the build. Empty params must fail gracefully.
    #[test]
    fn omnivoice_local_compiled_in_and_fails_gracefully() {
        let err = OmniVoiceLocalEngine::new(OmniVoiceLocalParams::default())
            .expect_err("empty params must be a credential error");
        assert!(
            err.0.contains("required") || err.0.contains("model_path"),
            "expected a model/codec path error, got: {}",
            err.0
        );
    }
}
