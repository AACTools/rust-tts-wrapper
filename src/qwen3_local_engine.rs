//! Local Qwen3-TTS engine via [qwen3-tts.cpp](https://github.com/predict-woo/qwen3-tts.cpp)
//! (GGML, MIT; Qwen3-TTS-12Hz models Apache-2.0). Feature `qwen3-local`;
//! the C++ library is built by the user (`scripts/build-qwen3-local.sh`)
//! and located via `QWEN3_TTS_LIB`.
//!
//! Third offline engine (after sherpa-onnx and floravox), and the only
//! local one with zero-shot voice cloning: a voice string passed to
//! `speak` is a reference-audio WAV path, an `emb:<base64>` speaker
//! embedding produced by the `qwen3-local` cloner, or
//! `icl:<wav>|<transcript>` for in-context cloning (see
//! `crate::cloning`).
//!
//! Output: PCM16 LE mono 24 kHz (`on_audio`), word boundaries are
//! duration-scaled estimates (the C++ pipeline exposes no timestamps).
//!
//! # Cloning (2026-11 library: both routes)
//! `icl:<wav>|<transcript>` — in-context cloning (embedding +
//! reference codec tokens + transcript): accent and prosody are
//! preserved. `emb:<base64>` or a bare WAV path — x-vector-only
//! (ECAPA): timbre cloned, accent/prosody from the model prior (the
//! pre-2026-11 upstream mode; the 2026-10-10 "ICL unimplemented"
//! finding is retired). With an audio callback, plain and ICL
//! synthesis STREAM (first chunk during generation) and `stop()`
//! cancels mid-utterance; `emb:` clones have no upstream streaming
//! route and deliver batch. SSML-with-breaks segments synthesize
//! per-segment batch.

use crate::engine::{strip_ssml_to_text, weighted_boundaries_scaled, TtsEngine};
use crate::qwen3_local_support::{
    apply_gain, decode_embedding, id_to_iso639_3, language_id_for, supported_languages,
    EMBEDDING_SIZE,
};
use crate::ssml_expand::expand_ssml_to_text;
use crate::types::{Gender, LanguageCode, TtsError, TtsResult, Voice};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_float, c_int};

// ==== FFI (src/qwen3tts_c_api.h — hand-declared; the surface is tiny) ====

// Wrapped in a macro: cbindgen's synth parser cannot expand
// macro_rules!, keeping these external-library bindings out of the
// generated C header (rustc expands this normally).
/// Process-wide lock around every C++ entry point (moved rationale:
/// upstream is not concurrent-safe — see qwen3_local_support for the
/// testable pure helpers). Callers can create/destroy/speak from any
/// threads; this serializes the calls.
static CPP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

macro_rules! qwen3_ffi {
    () => {
        #[derive(Clone)]
        #[repr(C)]
        pub(crate) struct Qwen3TtsParams {
            max_audio_tokens: i32,
            temperature: f32,
            top_p: f32,
            top_k: i32,
            n_threads: i32,
            repetition_penalty: f32,
            language_id: i32,
            /// Optional pre-computed codec tokens file (ICL only).
            /// THE ABI-FIX FIELD (#145-class drift): upstream added it
            /// after our initial port; without it the struct is 12
            /// bytes short and `qwen3_tts_default_params` wrote past
            /// our allocation — stack corruption against any library
            /// built from 2026-11 master onward.
            ref_code_path: *const c_char,
        }

        /// Pin the ABI: size AND the ref_code_path offset (a
        /// same-width field reorder would pass a size-only pin and
        /// still corrupt — loop review). 28 bytes of scalars + 4
        /// alignment padding + the 8-byte pointer at offset 32.
        /// A change here means the hand-declared FFI needs re-reading
        /// against `src/qwen3tts_c_api.h`.
        const _: () = assert!(std::mem::size_of::<Qwen3TtsParams>() == 40);
        const _: () = assert!(std::mem::offset_of!(Qwen3TtsParams, ref_code_path) == 32);

        #[repr(C)]
        pub(crate) struct Qwen3TtsAudio {
            samples: *const c_float,
            n_samples: i32,
            sample_rate: i32,
        }

        // Opaque C++ handle. An extern-type-pattern struct with no fields is
        // FFI-safe for opaque pointers.
        #[repr(C)]
        pub(crate) struct Qwen3Tts {
            pub(crate) _private: [u8; 0],
        }

        /// Streaming callback: `(samples, n, user_data) -> 0 stops
        /// generation early` (the C++ contract).
        pub(crate) type Qwen3StreamCallback =
            extern "C" fn(*const c_float, i32, *mut std::ffi::c_void) -> c_int;

        extern "C" {
            fn qwen3_tts_default_params(params: *mut Qwen3TtsParams);
            fn qwen3_tts_create(model_dir: *const c_char, n_threads: c_int) -> *mut Qwen3Tts;
            fn qwen3_tts_is_loaded(tts: *const Qwen3Tts) -> c_int;
            fn qwen3_tts_sample_rate(tts: *const Qwen3Tts) -> c_int;
            fn qwen3_tts_synthesize(
                tts: *mut Qwen3Tts,
                text: *const c_char,
                params: *const Qwen3TtsParams,
            ) -> *mut Qwen3TtsAudio;
            fn qwen3_tts_free_audio(audio: *mut Qwen3TtsAudio);
            fn qwen3_tts_destroy(tts: *mut Qwen3Tts);
            fn qwen3_tts_synthesize_with_voice_file(
                tts: *mut Qwen3Tts,
                text: *const c_char,
                reference_audio_path: *const c_char,
                params: *const Qwen3TtsParams,
            ) -> *mut Qwen3TtsAudio;
            /// Clone from RAW f32 samples (no temp WAV on disk).
            /// (Declared for parity; no call site yet — the cloner's
            /// emb: handles and file paths cover current routes.)
            fn qwen3_tts_synthesize_with_voice_samples(
                tts: *mut Qwen3Tts,
                text: *const c_char,
                ref_samples: *const c_float,
                n_ref_samples: i32,
                params: *const Qwen3TtsParams,
            ) -> *mut Qwen3TtsAudio;
            fn qwen3_tts_extract_embedding_file(
                tts: *mut Qwen3Tts,
                reference_audio_path: *const c_char,
                embedding_out: *mut c_float,
                max_size: i32,
            ) -> c_int;
            fn qwen3_tts_synthesize_with_embedding(
                tts: *mut Qwen3Tts,
                text: *const c_char,
                embedding: *const c_float,
                embedding_size: i32,
                params: *const Qwen3TtsParams,
            ) -> *mut Qwen3TtsAudio;
            fn qwen3_tts_get_error(tts: *const Qwen3Tts) -> *const c_char;
            /// In-context cloning (embedding + codec tokens + transcript)
            // (upstream quirk: the ICL batch entry drops top_p/
            // n_threads and reports failures to stderr, not via
            // get_error — the STREAMING ICL entry is the well-behaved
            // one; we route streaming first when a callback exists)
            fn qwen3_tts_synthesize_in_context(
                tts: *mut Qwen3Tts,
                text: *const c_char,
                reference_audio_path: *const c_char,
                reference_text: *const c_char,
                params: *const Qwen3TtsParams,
            ) -> *mut Qwen3TtsAudio;
            /// Streaming: plain synthesis with progressive audio
            /// (first chunk after ~2 s of generated audio).
            fn qwen3_tts_synthesize_streaming(
                tts: *mut Qwen3Tts,
                text: *const c_char,
                params: *const Qwen3TtsParams,
                callback: Qwen3StreamCallback,
                user_data: *mut std::ffi::c_void,
            ) -> *mut Qwen3TtsAudio;
            /// Streaming ICL: the reference is decode warmup (never
            /// delivered); the first delivered chunk additionally
            /// waits for the echo plus a transition margin.
            fn qwen3_tts_synthesize_in_context_streaming(
                tts: *mut Qwen3Tts,
                text: *const c_char,
                reference_audio_path: *const c_char,
                reference_text: *const c_char,
                params: *const Qwen3TtsParams,
                callback: Qwen3StreamCallback,
                user_data: *mut std::ffi::c_void,
            ) -> *mut Qwen3TtsAudio;
        }
    };
}

qwen3_ffi!();

/// The local engine. One `Qwen3Tts` per model directory; synthesis is
/// CPU-bound and single-utterance (the C++ pipeline is not re-entrant).
pub struct Qwen3LocalEngine {
    tts: *mut Qwen3Tts,
    params: Qwen3TtsParams,
}

impl std::fmt::Debug for Qwen3LocalEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Qwen3LocalEngine")
            .field("loaded", {
                let _guard = CPP_LOCK
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                &(unsafe { qwen3_tts_is_loaded(self.tts) != 0 })
            })
            .finish_non_exhaustive()
    }
}

// The C++ object owns GGML backends with per-call scheduling; the
// upstream API is single-threaded-per-handle and we never share it
// across threads within one engine instance.
unsafe impl Send for Qwen3LocalEngine {}
unsafe impl Sync for Qwen3LocalEngine {}

/// Test seam: set the cancellation flag (mirrors stop()).
#[doc(hidden)]
pub fn stop_for_tests() {
    QWEN_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Test seam: the params struct's size (the ABI pin, by name).
#[doc(hidden)]
#[must_use]
pub fn params_size_probe() -> usize {
    std::mem::size_of::<Qwen3TtsParams>()
}

/// Test seam: clear the cancellation flag (the engine resets per
/// speak; parallel tests need an explicit reset).
#[doc(hidden)]
pub fn reset_cancel_for_tests() {
    QWEN_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Read by the stream trampoline (crate-visible for it).
pub(crate) fn qwen_cancel_requested() -> bool {
    QWEN_CANCEL.load(std::sync::atomic::Ordering::SeqCst)
}

/// Cancellation for streaming synthesis: `stop()` sets it, the
/// stream callback returns 0 and the C++ pipeline stops generating
/// (the upstream contract). Reset at each speak, like sherpa's flag.
static QWEN_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The parsed voice convention for one utterance.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceSpec {
    /// No reference audio (the default timbre; a language voice also
    /// lands here after setting `language_id`)
    Plain,
    /// `emb:<base64>` — cached x-vector embedding (timbre-only clone)
    Embedding(Vec<f32>),
    /// A WAV path — x-vector clone from file
    ReferenceFile(String),
    /// `icl:<wav-path>|<transcript>` — in-context cloning: accent and
    /// prosody are cloned too, not just timbre
    InContext {
        /// the reference WAV path
        path: String,
        /// the reference's transcript (what was said)
        transcript: String,
    },
}

/// Parse the engine's voice string into a [`VoiceSpec`]. `None`,
/// `""`, `"default"`, and supported language codes are Plain (the
/// language sets `language_id` separately); `emb:` and `icl:` carry
/// their payloads; anything else is a reference file path.
///
/// # Errors
/// Malformed `emb:` base64, or an `icl:` voice missing its
/// `<path>|<transcript>` pair — both caller fixable.
pub fn parse_voice_spec(voice: Option<&str>) -> TtsResult<VoiceSpec> {
    let v = voice
        .map(str::trim)
        .filter(|v| !v.is_empty() && *v != "default");
    let Some(v) = v else {
        return Ok(VoiceSpec::Plain);
    };
    if let Some(rest) = v.strip_prefix("emb:") {
        let raw = decode_embedding(rest)
            .map_err(|e| TtsError(format!("qwen3-local: bad emb: voice: {e}")))?;
        return Ok(VoiceSpec::Embedding(raw));
    }
    if let Some(rest) = v.strip_prefix("icl:") {
        // `icl:<wav-path>|<transcript>` — the transcript is REQUIRED
        // (accent/prosody cloning reads it); split on the FIRST pipe
        // so transcripts may contain pipes
        let (path, transcript) = rest.split_once('|').ok_or_else(|| {
            TtsError(
                "qwen3-local: icl: voice needs '<wav-path>|<transcript>' — the transcript \
                 is what preserves accent and prosody"
                    .into(),
            )
        })?;
        if path.is_empty() || transcript.is_empty() {
            return Err(TtsError(
                "qwen3-local: icl: voice path and transcript must both be non-empty".into(),
            ));
        }
        return Ok(VoiceSpec::InContext {
            path: path.to_string(),
            transcript: transcript.to_string(),
        });
    }
    if language_id_for(v).is_some() {
        return Ok(VoiceSpec::Plain);
    }
    Ok(VoiceSpec::ReferenceFile(v.to_string()))
}

impl Qwen3LocalEngine {
    /// Create from credentials: `modelsDir` (required — holds the two
    /// GGUFs), `threads`, `temperature`, `topK`, `maxTokens`.
    ///
    /// # Errors
    /// When `modelsDir` is missing/invalid or the C++ engine fails to
    /// load the GGUF models.
    pub fn new(credentials: &HashMap<String, String>) -> TtsResult<Self> {
        let models_dir = credentials
            .get("modelsDir")
            .filter(|d| !d.is_empty())
            .ok_or_else(|| {
                TtsError("qwen3-local: credentials need modelsDir (dir with qwen3-tts-0.6b-f16.gguf + qwen3-tts-tokenizer-f16.gguf)".into())
            })?;
        let threads: i32 = credentials
            .get("threads")
            .and_then(|t| t.parse().ok())
            .unwrap_or(4);
        let c_dir =
            CString::new(models_dir.as_str()).map_err(|e| TtsError(format!("modelsDir: {e}")))?;
        let _guard = CPP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tts = unsafe { qwen3_tts_create(c_dir.as_ptr(), threads) };
        if tts.is_null() {
            return Err(TtsError("qwen3-local: qwen3_tts_create failed".into()));
        }
        let mut params = Qwen3TtsParams {
            max_audio_tokens: 4096,
            temperature: 0.9,
            top_p: 1.0,
            top_k: 50,
            n_threads: threads,
            repetition_penalty: 1.05,
            language_id: 2050, // en
            ref_code_path: std::ptr::null(),
        };
        unsafe { qwen3_tts_default_params(std::ptr::addr_of_mut!(params)) };
        // Credential overrides after defaults.
        if let Some(t) = credentials.get("temperature").and_then(|v| v.parse().ok()) {
            params.temperature = t;
        }
        if let Some(k) = credentials.get("topK").and_then(|v| v.parse().ok()) {
            params.top_k = k;
        }
        if let Some(m) = credentials.get("maxTokens").and_then(|v| v.parse().ok()) {
            params.max_audio_tokens = m;
        }
        Ok(Self { tts, params })
    }

    fn last_error(&self) -> String {
        unsafe {
            let err = qwen3_tts_get_error(self.tts);
            if err.is_null() {
                String::new()
            } else {
                CStr::from_ptr(err).to_string_lossy().into_owned()
            }
        }
    }

    /// Run one BATCH synthesis and return PCM16 LE mono bytes plus the
    /// total sample count (for boundary estimation). `voice`: the
    /// engine convention (`None`/language/`emb:`/path/`icl:path|text`).
    fn synthesize_to_pcm16(
        &self,
        text: &str,
        voice: Option<&str>,
        mut params: Qwen3TtsParams,
    ) -> TtsResult<(Vec<u8>, i32, usize)> {
        let plain = plain_text_for_qwen(text);
        let spec = parse_voice_spec(voice)?;
        // a language voice selects the codec language token
        if let Some(lid) = voice.and_then(language_id_for) {
            params.language_id = lid;
        }
        let (pcm, rate, samples) = self.run_batch(&plain, &spec, &params)?;
        Ok((pcm, rate, samples))
    }

    /// The FFI dispatch for one batch utterance.
    fn run_batch(
        &self,
        plain: &str,
        spec: &VoiceSpec,
        params: &Qwen3TtsParams,
    ) -> TtsResult<(Vec<u8>, i32, usize)> {
        let c_text = CString::new(plain).map_err(|e| TtsError(format!("text: {e}")))?;
        // Upstream is not concurrent-safe: serialize at the C boundary.
        let _guard = CPP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let audio_ptr = match spec {
            VoiceSpec::Plain => unsafe { qwen3_tts_synthesize(self.tts, c_text.as_ptr(), params) },
            VoiceSpec::Embedding(raw) => unsafe {
                qwen3_tts_synthesize_with_embedding(
                    self.tts,
                    c_text.as_ptr(),
                    raw.as_ptr(),
                    raw.len() as i32,
                    params,
                )
            },
            VoiceSpec::ReferenceFile(path) => {
                let c_path = CString::new(path.as_str())
                    .map_err(|e| TtsError(format!("reference path: {e}")))?;
                unsafe {
                    qwen3_tts_synthesize_with_voice_file(
                        self.tts,
                        c_text.as_ptr(),
                        c_path.as_ptr(),
                        params,
                    )
                }
            }
            VoiceSpec::InContext { path, transcript } => {
                let c_path = CString::new(path.as_str())
                    .map_err(|e| TtsError(format!("reference path: {e}")))?;
                let c_text_ref = CString::new(transcript.as_str())
                    .map_err(|e| TtsError(format!("reference transcript: {e}")))?;
                unsafe {
                    qwen3_tts_synthesize_in_context(
                        self.tts,
                        c_text.as_ptr(),
                        c_path.as_ptr(),
                        c_text_ref.as_ptr(),
                        params,
                    )
                }
            }
        };
        if audio_ptr.is_null() {
            return Err(TtsError(format!(
                "qwen3-local: synthesis failed: {}",
                self.last_error()
            )));
        }
        // Convert f32 → PCM16 and free the C++ buffer even on early return.
        let mut pcm = Vec::new();
        unsafe {
            let audio = &*audio_ptr;
            #[allow(clippy::cast_possible_truncation)]
            let n = audio.n_samples.max(0) as usize;
            if !audio.samples.is_null() && n > 0 {
                pcm.reserve(n * 2);
                for i in 0..n {
                    let s = *audio.samples.add(i);
                    #[allow(clippy::cast_possible_truncation)]
                    let v = (s.clamp(-1.0, 1.0) * 32_767.0) as i16;
                    pcm.extend_from_slice(&v.to_le_bytes());
                }
            }
            let rate = audio.sample_rate;
            let n_samples = n;
            qwen3_tts_free_audio(audio_ptr);
            if pcm.is_empty() {
                return Err(TtsError("qwen3-local: synthesis produced no audio".into()));
            }
            Ok((pcm, rate, n_samples))
        }
    }

    /// STREAMING synthesis: delivers PCM16 chunks through `deliver`
    /// as the C++ pipeline produces them (first chunk after ~2 s of
    /// generated audio; in ICL the reference is decode WARMUP —
    /// it delays the first chunk and is never delivered as audio).
    /// Returns
    /// (sample_rate, total_f32_samples) — or None when the voice spec
    /// has no streaming route (`emb:` clones) and the caller should
    /// take the batch path.
    fn run_streaming(
        &self,
        plain: &str,
        spec: &VoiceSpec,
        params: &Qwen3TtsParams,
        gain: f32,
        deliver: &mut dyn FnMut(&[u8]),
    ) -> TtsResult<Option<(i32, usize)>> {
        // only Plain and ICL have streaming entry points; embeddings
        // (no streaming FFI) and plain reference files fall back
        let (use_icl, c_path, c_ref_text) = match spec {
            VoiceSpec::Plain => (false, None, None),
            VoiceSpec::InContext { path, transcript } => {
                let c_path = CString::new(path.as_str())
                    .map_err(|e| TtsError(format!("reference path: {e}")))?;
                let c_ref = CString::new(transcript.as_str())
                    .map_err(|e| TtsError(format!("reference transcript: {e}")))?;
                (true, Some(c_path), Some(c_ref))
            }
            VoiceSpec::Embedding(_) | VoiceSpec::ReferenceFile(_) => return Ok(None),
        };
        let c_text = CString::new(plain).map_err(|e| TtsError(format!("text: {e}")))?;
        let _guard = CPP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = crate::qwen3_stream::StreamState::new(deliver, gain);
        let audio_ptr = unsafe {
            if use_icl {
                qwen3_tts_synthesize_in_context_streaming(
                    self.tts,
                    c_text.as_ptr(),
                    c_path.as_ref().unwrap().as_ptr(),
                    c_ref_text.as_ref().unwrap().as_ptr(),
                    params,
                    crate::qwen3_stream::stream_trampoline,
                    std::ptr::addr_of_mut!(state).cast::<std::ffi::c_void>(),
                )
            } else {
                qwen3_tts_synthesize_streaming(
                    self.tts,
                    c_text.as_ptr(),
                    params,
                    crate::qwen3_stream::stream_trampoline,
                    std::ptr::addr_of_mut!(state).cast::<std::ffi::c_void>(),
                )
            }
        };
        let total = state.total_samples;
        let cancelled = state.cancelled;
        if audio_ptr.is_null() {
            return Err(TtsError(format!(
                "qwen3-local: synthesis failed: {}",
                self.last_error()
            )));
        }
        // the returned audio also contains the FULL buffer (contract)
        let rate = unsafe { (*audio_ptr).sample_rate };
        unsafe { qwen3_tts_free_audio(audio_ptr) };
        if total == 0 && !cancelled {
            // streaming delivered nothing and nobody cancelled: the
            // pipeline probably produced no audio
            return Err(TtsError("qwen3-local: synthesis produced no audio".into()));
        }
        Ok(Some((rate, total)))
    }

    /// Extract a speaker embedding from a reference WAV (for the
    /// cloner). Returns the f32 embedding.
    ///
    /// # Errors
    /// When the path is invalid or extraction fails in the C++ pipeline.
    pub(crate) fn extract_embedding(&self, reference_wav: &str) -> TtsResult<Vec<f32>> {
        let c_path =
            CString::new(reference_wav).map_err(|e| TtsError(format!("reference path: {e}")))?;
        let mut out = vec![0.0f32; EMBEDDING_SIZE];
        let _guard = CPP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = unsafe {
            qwen3_tts_extract_embedding_file(
                self.tts,
                c_path.as_ptr(),
                out.as_mut_ptr(),
                EMBEDDING_SIZE as i32,
            )
        };
        if n <= 0 {
            return Err(TtsError(format!(
                "qwen3-local: embedding extraction failed: {}",
                self.last_error()
            )));
        }
        out.truncate(n as usize);
        Ok(out)
    }
}

impl Drop for Qwen3LocalEngine {
    fn drop(&mut self) {
        if !self.tts.is_null() {
            let _guard = CPP_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            unsafe { qwen3_tts_destroy(self.tts) };
        }
    }
}

/// SSML-in for the qwen pipeline: the C++ side reads tags aloud, so
/// interpret the document locally first — `<say-as>` variants expand to
/// spoken words, `<sub alias>` substitutes, `<phoneme>` keeps its display
/// text. Falls back to plain tag-stripping for malformed documents.
#[cfg(feature = "ssml-expand")]
fn plain_text_for_qwen(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.to_ascii_lowercase().starts_with("<speak") {
        if let Some(expanded) = expand_ssml_to_text(trimmed) {
            return expanded;
        }
        return strip_ssml_to_text(trimmed);
    }
    text.to_string()
}

/// Parse SSML into break-separated segments; returns `None` for plain text.
#[cfg(feature = "ssml-expand")]
fn ssml_segments_for_qwen(text: &str) -> Option<Vec<crate::ssml_expand::SsmlSegment>> {
    let trimmed = text.trim();
    if trimmed.to_ascii_lowercase().starts_with("<speak") {
        return crate::ssml_expand::expand_ssml_to_segments(trimmed);
    }
    None
}

/// Synthesise a list of segments with silence gaps between them,
/// concatenating the PCM and firing marks at segment boundaries.
#[allow(clippy::too_many_arguments, clippy::cast_precision_loss)]
#[cfg(feature = "ssml-expand")]
fn speak_segments(
    engine: &Qwen3LocalEngine,
    segments: &[crate::ssml_expand::SsmlSegment],
    voice: Option<&str>,
    rate: f32,
    volume: f32,
    mut on_audio: Option<crate::engine::OnAudioCallback<'_>>,
    mut on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
    mut on_mark: Option<crate::engine::OnMarkCallback<'_>>,
) -> TtsResult<()> {
    let sr = 24_000u32;
    let mut all_pcm: Vec<i16> = Vec::new();
    let mut offset_secs: f32 = 0.0;

    for seg in segments {
        // fire marks at this segment's start
        if let Some(cb) = on_mark.as_mut() {
            for name in &seg.marks {
                cb(name, offset_secs, offset_secs, -1);
            }
        }

        if seg.text.is_empty() {
            // pure break segment — just add silence
            let silence_samples = (seg.pause_after_ms as f32 / 1000.0 * sr as f32) as usize;
            all_pcm.extend(std::iter::repeat_n(0i16, silence_samples));
            offset_secs += silence_samples as f32 / sr as f32;
            continue;
        }

        // synthesise this segment
        let seg_text = seg.text.as_str();
        let (pcm_bytes, _pcm_rate, _n) =
            engine.synthesize_to_pcm16(seg_text, voice, engine.params.clone())?;
        let seg_dur = pcm_bytes.len() as f32 / (sr as f32 * 2.0);

        // deliver audio chunk (pcm_bytes are already LE i16 bytes)
        if let Some(cb) = on_audio.as_mut() {
            for chunk in pcm_bytes.chunks(7200) {
                cb(chunk);
            }
        }

        // fire boundaries for this segment, offset by accumulated time
        if let Some(cb) = on_boundary.as_mut() {
            let boundaries = crate::engine::weighted_boundaries_scaled(seg_text, seg_dur);
            for b in &boundaries {
                let start = offset_secs + b.offset as f32 / 1000.0;
                let end = offset_secs + (b.offset + b.duration) as f32 / 1000.0;
                cb(&b.text, start, end, -1, -1, true);
            }
        }

        // append the i16 samples (from LE bytes)
        for pair in pcm_bytes.as_chunks::<2>().0 {
            let lo = u16::from(pair[0]);
            let hi = u16::from(pair[1]);
            all_pcm.push((lo | (hi << 8)) as i16);
        }
        offset_secs += seg_dur;

        // insert silence for the break
        if seg.pause_after_ms > 0 {
            let silence_samples = (seg.pause_after_ms as f32 / 1000.0 * sr as f32) as usize;
            all_pcm.extend(std::iter::repeat_n(0i16, silence_samples));
            offset_secs += silence_samples as f32 / sr as f32;
        }
    }

    let _ = (rate, volume); // rate/volume handled per-segment by the engine
    Ok(())
}

impl TtsEngine for Qwen3LocalEngine {
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        volume: f32,
        mut on_audio: Option<crate::engine::OnAudioCallback>,
        mut on_boundary: Option<crate::engine::OnBoundaryCallback>,
        on_mark: Option<crate::engine::OnMarkCallback>,
    ) -> TtsResult<()> {
        // SSML with <break> tags: synthesise each segment independently,
        // insert silence gaps, fire marks at segment boundaries (#50)
        #[cfg(feature = "ssml-expand")]
        if let Some(segments) = ssml_segments_for_qwen(text) {
            if segments.len() > 1 || segments.first().is_some_and(|s| s.pause_after_ms > 0) {
                return speak_segments(
                    self,
                    &segments,
                    voice,
                    rate,
                    volume,
                    on_audio,
                    on_boundary,
                    on_mark,
                );
            }
        }

        // SSML-in: strip once here — both the C++ input and the word
        // boundary estimates use the plain text (tags are never words).
        let plain = plain_text_for_qwen(text);
        let mut params = self.params.clone();
        let spec = parse_voice_spec(voice)?;
        // A language voice selects the codec language token. This MUST
        // happen on the plain path too — the SSML-segments path does
        // it inside synthesize_to_pcm16, and round-1's refactor
        // dropped it here (loop review, FFI-boundary-proven: fr/de
        // plain text reached the C++ entry with the default en token)
        if let Some(lid) = voice.and_then(language_id_for) {
            params.language_id = lid;
        }
        // per-utterance cancel reset (same shape as sherpa's flag)
        QWEN_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
        let gain = volume.clamp(0.0, 4.0);
        // STREAMING FIRST (when a callback wants audio and the voice
        // has a streaming route): chunks arrive while the model still
        // generates — first audio after ~2 s instead of after the
        // whole utterance. The volume gain rides each chunk (linear,
        // so per-chunk == whole-buffer).
        let (pcm, rate, n_samples) = if let Some(cb) = on_audio.as_mut() {
            match self.run_streaming(&plain, &spec, &params, gain, cb) {
                Ok(Some((rate, n))) => (Vec::new(), rate, n),
                // no streaming route (emb:): batch below
                Ok(None) => {
                    let (pcm, rate, n) = self.run_batch(&plain, &spec, &params)?;
                    let pcm = if (gain - 1.0).abs() > f32::EPSILON {
                        apply_gain(&pcm, gain)
                    } else {
                        pcm
                    };
                    for chunk in pcm.chunks(7200) {
                        cb(chunk);
                    }
                    (pcm, rate, n)
                }
                Err(e) => return Err(e),
            }
        } else {
            let (pcm, rate, n) = self.run_batch(&plain, &spec, &params)?;
            let pcm = if (gain - 1.0).abs() > f32::EPSILON {
                apply_gain(&pcm, gain)
            } else {
                pcm
            };
            (pcm, rate, n)
        };
        if let Some(cb) = on_boundary.as_mut() {
            // No timestamps from the C++ pipeline (investigated: the
            // talker exposes no word-alignment signal — see the
            // attention-dump research in qwen3-tts.cpp). Word shape from
            // syllable/pause weights, rescaled onto the exact duration:
            // long words get proportionally more time, punctuation beats
            // pause, and the last word ends at the audio end.
            // streamed utterances carry the sample count (pcm is
            // empty); batch utterances derive it from the buffer
            #[allow(clippy::cast_precision_loss)]
            let total_secs = if pcm.is_empty() {
                n_samples as f32 / rate.max(1) as f32
            } else {
                pcm.len() as f32 / (rate as f32 * 2.0)
            };
            let boundaries = weighted_boundaries_scaled(&plain, total_secs);
            for b in &boundaries {
                #[allow(clippy::cast_precision_loss)]
                let start = b.offset as f32 / 1000.0;
                #[allow(clippy::cast_precision_loss)]
                let end = (b.offset + b.duration) as f32 / 1000.0;
                // No source-offset mapping exists (no timestamps):
                // -1 per the callback contract for unknown offsets.
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

    fn stop(&self) -> TtsResult<()> {
        // Streaming synthesis checks this from the stream callback
        // and returns 0 — the C++ pipeline stops generating mid-
        // utterance (the batch path has no interrupt, same as before).
        QWEN_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        // The default timbre plus one voice per supported language;
        // beyond the catalogue, ANY reference WAV works as a "voice"
        // (zero-shot cloning — pass its path, or an emb: handle).
        let mut voices = vec![Voice {
            id: "default".into(),
            name: "Qwen3-TTS local (default)".into(),
            gender: Gender::Unknown,
            provider: "qwen3-local".into(),
            language_codes: vec![LanguageCode {
                bcp47: "en".into(),
                iso639_3: "eng".into(),
                display: "English".into(),
            }],
        }];
        for (id, bcp47, display, _) in supported_languages() {
            voices.push(Voice {
                id: (*id).into(),
                name: format!("Qwen3-TTS local ({display})"),
                gender: Gender::Unknown,
                provider: "qwen3-local".into(),
                language_codes: vec![LanguageCode {
                    bcp47: (*bcp47).into(),
                    iso639_3: id_to_iso639_3(id),
                    display: (*display).into(),
                }],
            });
        }
        Ok(voices)
    }

    fn engine_id(&self) -> &'static str {
        "qwen3-local"
    }

    /// Models actually loaded (mirrors the cloud engines' credential
    /// check — for a local engine this is "are the GGUFs valid").
    fn check_credentials(&self) -> TtsResult<bool> {
        let _guard = CPP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(unsafe { qwen3_tts_is_loaded(self.tts) != 0 })
    }

    /// The GGUFs were loaded at construction (GH #69) — a live engine
    /// means a working setup, so report Verified. (A live engine whose
    /// models somehow unloaded reads Unverified — "setup state unknown"
    /// — rather than a rejection.)
    fn check_credentials_status(&self) -> TtsResult<crate::engine::CredentialCheck> {
        use crate::engine::CredentialCheck;
        if self.check_credentials()? {
            Ok(CredentialCheck::Verified)
        } else {
            Ok(CredentialCheck::Unverified)
        }
    }
}

#[cfg(test)]
mod nuget_build_tests {
    use super::*;

    /// GH #62: this test compiling at all proves the qwen3-local engine
    /// is IN the build (the factory's "not enabled" path is cfg'd out).
    /// With empty credentials the engine must fail gracefully — a
    /// credential error, never a panic or a "not enabled" message.
    #[test]
    fn qwen3_local_compiled_in_and_fails_gracefully() {
        let creds: HashMap<String, String> = HashMap::new();
        let err = Qwen3LocalEngine::new(&creds)
            .expect_err("empty credentials must be a credential error");
        assert!(
            err.0.contains("modelsDir"),
            "expected a modelsDir credential error, got: {}",
            err.0
        );
    }
}
