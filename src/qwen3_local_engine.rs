//! Local Qwen3-TTS engine via [qwen3-tts.cpp](https://github.com/predict-woo/qwen3-tts.cpp)
//! (GGML, MIT; Qwen3-TTS-12Hz models Apache-2.0). Feature `qwen3-local`;
//! the C++ library is built by the user (`scripts/build-qwen3-local.sh`)
//! and located via `QWEN3_TTS_LIB`.
//!
//! Third offline engine (after sherpa-onnx and floravox), and the only
//! local one with zero-shot voice cloning: any voice string passed to
//! `speak` is treated as a reference-audio WAV path — or an
//! `emb:<base64>` speaker embedding produced by the `qwen3-local`
//! cloner (see `crate::cloning`).
//!
//! Output: PCM16 LE mono 24 kHz (`on_audio`), word boundaries are
//! duration-scaled estimates (the C++ pipeline exposes no timestamps).
//!
//! # Cloning quality caveat (2026-10-10 finding)
//! Upstream implements **x-vector-only** cloning (ECAPA embedding):
//! timbre is cloned, but accent/prosody come from the model prior —
//! with `language_id=en` that prior is predominantly American-accented.
//! This is the official `x_vector_only_mode=true` "reduced quality"
//! mode; in-context (codec-prefix) cloning is unimplemented upstream.
//! Full analysis + the upstream-PR plan live in the project's
//! voice-cloning plan notes (kept out of the repository).

use crate::engine::{estimate_word_boundaries, strip_ssml_to_text, TtsEngine};
use crate::qwen3_local_support::{
    apply_gain, decode_embedding, id_to_iso639_3, language_id_for, supported_languages,
    EMBEDDING_SIZE,
};
use crate::types::{Gender, LanguageCode, TtsError, TtsResult, Voice, WordBoundary};
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
        }

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

        extern "C" {
            fn qwen3_tts_default_params(params: *mut Qwen3TtsParams);
            fn qwen3_tts_create(model_dir: *const c_char, n_threads: c_int) -> *mut Qwen3Tts;
            fn qwen3_tts_is_loaded(tts: *const Qwen3Tts) -> c_int;
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

    /// Run one synthesis and return PCM16 LE mono bytes. `voice`:
    /// `None` or `"default"` — no reference; a path — reference WAV;
    /// `emb:<base64>` — cached speaker embedding from the cloner.
    fn synthesize_to_pcm16(
        &self,
        text: &str,
        voice: Option<&str>,
        mut params: Qwen3TtsParams,
    ) -> TtsResult<(Vec<u8>, i32)> {
        // SSML-in: strip tags the pipeline would read aloud.
        let plain = if text.trim_start().starts_with('<') {
            strip_ssml_to_text(text)
        } else {
            text.to_string()
        };
        let c_text = CString::new(plain).map_err(|e| TtsError(format!("text: {e}")))?;
        // Upstream is not concurrent-safe: serialize at the C boundary.
        let _guard = CPP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A language voice selects the codec language token; reference
        // paths and emb: handles are cloning inputs instead.
        let voice_trim = voice.map(str::trim);
        if let Some(lang) = voice_trim.filter(|v| !v.is_empty() && *v != "default") {
            if let Some(lid) = language_id_for(lang) {
                params.language_id = lid;
            }
        }
        let voice_for_audio = voice_trim.filter(|v| !v.is_empty() && *v != "default");
        let is_language_voice = voice_for_audio.is_some_and(|v| language_id_for(v).is_some());
        let audio_ptr = match voice_for_audio.filter(|_| !is_language_voice) {
            None => unsafe {
                qwen3_tts_synthesize(self.tts, c_text.as_ptr(), std::ptr::addr_of!(params))
            },
            Some(v) if v.starts_with("emb:") => {
                let raw = decode_embedding(v)
                    .map_err(|e| TtsError(format!("qwen3-local: bad emb: voice: {e}")))?;
                unsafe {
                    qwen3_tts_synthesize_with_embedding(
                        self.tts,
                        c_text.as_ptr(),
                        raw.as_ptr(),
                        raw.len() as i32,
                        std::ptr::addr_of!(params),
                    )
                }
            }
            Some(path) => {
                let c_path =
                    CString::new(path).map_err(|e| TtsError(format!("reference path: {e}")))?;
                unsafe {
                    qwen3_tts_synthesize_with_voice_file(
                        self.tts,
                        c_text.as_ptr(),
                        c_path.as_ptr(),
                        std::ptr::addr_of!(params),
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
            qwen3_tts_free_audio(audio_ptr);
            if pcm.is_empty() {
                return Err(TtsError("qwen3-local: synthesis produced no audio".into()));
            }
            Ok((pcm, rate))
        }
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

impl TtsEngine for Qwen3LocalEngine {
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        _rate: f32,
        _pitch: f32,
        volume: f32,
        mut on_audio: Option<crate::engine::OnAudioCallback>,
        mut on_boundary: Option<crate::engine::OnBoundaryCallback>,
        _on_mark: Option<crate::engine::OnMarkCallback>,
    ) -> TtsResult<()> {
        // SSML-in: strip once here — both the C++ input and the word
        // boundary estimates use the plain text (tags are never words).
        let plain = if text.trim_start().starts_with('<') {
            strip_ssml_to_text(text)
        } else {
            text.to_string()
        };
        let params = self.params.clone();
        let (pcm, rate) = self.synthesize_to_pcm16(&plain, voice, params)?;
        // Volume is a real control here: linear PCM gain (clamped),
        // applied before delivery. Rate/pitch have no pipeline control
        // (see the engine docs).
        let pcm = if (volume - 1.0).abs() > f32::EPSILON {
            apply_gain(&pcm, volume.clamp(0.0, 4.0))
        } else {
            pcm
        };
        if let Some(cb) = on_audio.as_mut() {
            // Match the cloud engines' delivery granularity (~150 ms).
            for chunk in pcm.chunks(7200) {
                cb(chunk);
            }
        }
        if let Some(cb) = on_boundary.as_mut() {
            // No timestamps from the C++ pipeline: scaled estimates
            // anchored to the actual duration.
            let boundaries: Vec<WordBoundary> = estimate_word_boundaries(&plain);
            #[allow(clippy::cast_precision_loss)]
            let total_secs = pcm.len() as f32 / (rate as f32 * 2.0);
            #[allow(clippy::cast_precision_loss)]
            let words = boundaries.len().max(1) as f32;
            for (i, b) in boundaries.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let start = total_secs * (i as f32) / words;
                #[allow(clippy::cast_precision_loss)]
                let end = total_secs * ((i + 1) as f32) / words;
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
        Ok(()) // single-shot C++ calls; nothing to cancel mid-flight
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
}
