//! floravox offline TTS engine: event-driven SSML synthesis for
//! piper/MMS VITS, Matcha (+vocoder), and Kokoro ONNX voices.
//!
//! What this adds over the sherpa-onnx engine:
//!
//! * **Native SSML** — `<break>`, `<prosody rate>`, `<mark>`,
//!   `<phoneme>`, `<sub>`, `<say-as>` are parsed locally
//!   (byte/char-exact spans), no cloud round-trip.
//! * **Measured word timings** — voices patched with floravox's
//!   duration-output surgery report boundaries from the model's own
//!   duration tensor (`estimated: false`). Unpatched voices fall back
//!   through the student tier (`.student` sidecar) to proportional
//!   estimates (`estimated: true`) — the flag is passed through
//!   untouched, never re-estimated here.
//! * **`<mark>` events** — surfaced both through the mark callback and
//!   as zero-duration measured boundaries (matching how cloud engines
//!   surface bookmark events), so consumers only need the boundary
//!   stream.
//!
//! G2P: misaki (the Kokoro phonemizer, a floravox-core default feature)
//! runs as the document pre-pass for phoneme voices; MMS-style
//! character-table voices (auto-detected, or forced via the `chars`
//! credential) use the CharFrontend instead. Lexicon/Phonetisaurus/ByT5
//! chains are intentionally out of scope for this engine.
//!
//! Audio is delivered as 16-bit little-endian mono PCM chunks, the same
//! shape as the sherpa-onnx engine. `pitch` is ignored (VITS-family
//! voices have no pitch control).
//!
//! # wasm32
//!
//! The feature compiles for native targets everywhere ort ships
//! prebuilt binaries. **wasm32 is currently blocked upstream of this
//! crate**: `ort-sys@2.0.0-rc.13` ships no wasm32 binaries ("no
//! prebuilt binaries available for target wasm32-unknown-unknown"), so
//! the onnx-enabled build cannot link there. The web-demo plan closes
//! this with a `floravox-wasm` crate bridging ORT to onnxruntime-web
//! (ort's `disable-linking` + JS glue); when that lands, the swap
//! happens in voice loading and this module's `std::fs` use — which is
//! already confined to [`FloravoxEngine::resolve_model`] and voice
//! discovery, i.e. caller-controlled paths — moves with it.

use crate::engine::TtsEngine;
use crate::types::{Gender, LanguageCode, TtsError, TtsResult, Voice, WordBoundary};
use floravox_core::synth::{CharFrontend, MisakiPrePass, StreamingSynthesis, Synthesizer};
use floravox_core::{SynthesisEvent, VoiceBackend};
use floravox_g2p::MisakiG2p;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Poll interval while interleaving audio chunks and events from the
/// synthesis channels (also caps `stop()` latency).
const POLL: Duration = Duration::from_millis(25);

/// Engine configuration from credentials JSON.
#[derive(Debug, Default)]
struct Config {
    models_dir: Option<PathBuf>,
    model_id: Option<String>,
    /// `"us"` (default) | `"gb"` — misaki dialect for the document
    /// pre-pass; `"off"` disables the pre-pass (per-token phonemizer
    /// only).
    misaki: Option<String>,
    /// Character-level frontend for MMS-style voices: `"true"` / `""`
    /// lowercases and feeds characters through the voice's own table;
    /// any other value is an ISO 639-3 code and input is romanized
    /// (uroman) first, e.g. `"hin"`.
    chars: Option<String>,
    /// Speaker id for multi-speaker voices (kokoro style slots, piper
    /// `sid`); single-speaker voices ignore it.
    speaker: Option<i64>,
}

impl Config {
    fn parse(credentials_json: &str) -> Self {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(credentials_json) else {
            return Self::default();
        };
        let get = |k: &str| {
            v.get(k)
                .and_then(serde_json::Value::as_str)
                .map(expand_tilde)
        };
        Self {
            models_dir: get("modelsDir"),
            model_id: v
                .get("modelId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            misaki: v
                .get("misaki")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            chars: v
                .get("chars")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            speaker: v.get("speaker").and_then(serde_json::Value::as_i64),
        }
    }
}

/// Expand a leading `~` to the user's home directory (the sherpa engine's
/// convention for directory credentials).
fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        // Unix HOME first, Windows USERPROFILE second ($HOME is almost
        // never set there).
        for key in ["HOME", "USERPROFILE"] {
            if let Some(home) = std::env::var_os(key) {
                if !home.is_empty() {
                    return PathBuf::from(home).join(rest);
                }
            }
        }
    }
    PathBuf::from(p)
}

/// Offline TTS engine backed by [floravox](https://github.com/AACTools/floravox).
pub struct FloravoxEngine {
    models_dir: PathBuf,
    model_id: Mutex<String>,
    /// misaki dialect: `"us"` | `"gb"` | `"off"`.
    misaki: String,
    /// Character-frontend spec: `None` = misaki pre-pass; `Some(spec)`
    /// = CharFrontend (`""`/`"true"` plain, otherwise an ISO 639-3
    /// romanization code).
    chars: Option<String>,
    /// Speaker id applied to multi-speaker voices.
    speaker: i64,
    /// Set by `stop()`; the streaming pump drops its channels in response,
    /// which cancels the synthesis worker within one poll interval.
    cancel: Arc<AtomicBool>,
    /// Cached synthesizer, keyed by the voice + phonemizer options it was
    /// built with (rebuilding reloads the ONNX session, so it is worth
    /// caching).
    synth: Mutex<Option<(String, Arc<Synthesizer<MisakiG2p>>)>>,
}

impl fmt::Debug for FloravoxEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FloravoxEngine")
            .field("models_dir", &self.models_dir)
            .field("model_id", &self.model_id.lock().map(|g| g.clone()).ok())
            .field("misaki", &self.misaki)
            .field("chars", &self.chars)
            .field("speaker", &self.speaker)
            .finish_non_exhaustive()
    }
}

impl FloravoxEngine {
    /// Create a new floravox engine.
    ///
    /// Credentials JSON keys (all optional):
    /// - `modelsDir`: directory of voices (defaults to
    ///   `~/.rust-tts-wrapper/floravox`). A voice is a directory (or flat
    ///   pair) holding `X.onnx` + `X.onnx.json`; floravox-core
    ///   auto-detects the family (piper/MMS/Matcha/Kokoro) and loads any
    ///   sibling `.student` word-timing sidecar.
    /// - `modelId`: voice to load (directory or file stem). Voices are
    ///   also selectable per-call via `speak(voice = Some(...))`.
    /// - `misaki`: `"us"` (default) or `"gb"` — document-level misaki
    ///   pre-pass (heteronyms and numbers come out right); `"off"`
    ///   disables the pre-pass.
    /// - `chars`: character-level frontend for MMS-style voices.
    ///   `"true"` lowercases and feeds characters through the voice's
    ///   own table; any other string is an ISO 639-3 code and input is
    ///   romanized (uroman) first, e.g. `"hin"`. Overrides the misaki
    ///   pre-pass, and is forced on for auto-detected character-table
    ///   voices.
    /// - `speaker`: speaker id for multi-speaker voices (kokoro style
    ///   slots, piper `sid`).
    pub fn new(credentials_json: &str) -> Self {
        let cfg = Config::parse(credentials_json);
        Self {
            models_dir: cfg.models_dir.unwrap_or_else(default_models_dir),
            model_id: Mutex::new(cfg.model_id.unwrap_or_default()),
            misaki: cfg
                .misaki
                .unwrap_or_else(|| "us".to_string())
                .to_ascii_lowercase(),
            chars: cfg.chars,
            speaker: cfg.speaker.unwrap_or(0),
            cancel: Arc::new(AtomicBool::new(false)),
            synth: Mutex::new(None),
        }
    }

    /// Resolve the onnx path for a voice selector: a path under
    /// `models_dir` (dir with a single `.onnx`, or a direct `.onnx` file,
    /// or a bare stem). All `std::fs` use in this engine lives here and
    /// in voice discovery — the wasm seam, see the module docs.
    fn resolve_model(&self, voice: Option<&str>) -> TtsResult<PathBuf> {
        let requested = {
            let guard = self
                .model_id
                .lock()
                .map_err(|_| TtsError("model_id lock poisoned".into()))?;
            voice.map_or_else(|| guard.clone(), str::to_string)
        };
        if requested.is_empty() {
            return Err(TtsError(
                "No floravox modelId configured. Pass modelId in credentials JSON, \
                 or a voice selector per call."
                    .into(),
            ));
        }
        let direct = PathBuf::from(&requested);
        let candidates: Vec<PathBuf> = if direct.exists() {
            vec![direct]
        } else {
            let base = self.models_dir.join(&requested);
            vec![
                base.clone(),
                base.with_extension("onnx"),
                self.models_dir.join(format!("{requested}.onnx")),
            ]
        };
        for cand in candidates {
            if let Some(p) = find_onnx(&cand) {
                return Ok(p);
            }
        }
        Err(TtsError(format!(
            "floravox voice '{requested}' not found under {} (a voice is a directory \
             or pair holding X.onnx + X.onnx.json)",
            self.models_dir.display()
        )))
    }

    /// Get (building if needed) the cached synthesizer for a voice.
    fn synthesizer(&self, voice: Option<&str>) -> TtsResult<Arc<Synthesizer<MisakiG2p>>> {
        let onnx = self.resolve_model(voice)?;
        let key = format!(
            "{}|{}|{:?}|{}",
            onnx.display(),
            self.misaki,
            self.chars,
            self.speaker
        );
        let mut guard = self
            .synth
            .lock()
            .map_err(|_| TtsError("synth lock poisoned".into()))?;
        if let Some((k, s)) = guard.as_ref() {
            if *k == key {
                return Ok(Arc::clone(s));
            }
        }
        let model: Box<dyn VoiceBackend> = floravox_core::load_voice(&onnx)
            .map_err(|e| TtsError(format!("loading {}: {e:#}", onnx.display())))?;
        let auto_chars = model.config().is_char_table;
        let british = self.misaki.starts_with("gb");

        let mut synth = Synthesizer::new(model, MisakiG2p::new(british));

        // Document-level pre-passes, in order of specificity:
        //   explicit chars credential > auto-detected character table
        //   (MMS-style voices) > misaki pre-pass ("off" disables).
        if let Some(spec) = self.chars.as_deref() {
            let rom: Option<&'static str> = match spec {
                "" | "true" => None,
                code => Some(code.to_string().leak()),
            };
            synth = synth.with_document_phonemizer(Box::new(CharFrontend {
                lowercase: true,
                romanize: rom,
            }));
        } else if auto_chars {
            // Character-table voice with no explicit frontend: CharFrontend
            // is the only correct choice — phonemizing per-word would
            // spell everything out.
            synth = synth.with_document_phonemizer(Box::new(CharFrontend {
                lowercase: true,
                romanize: None,
            }));
        } else if self.misaki != "off" {
            synth =
                synth.with_document_phonemizer(Box::new(MisakiPrePass(MisakiG2p::new(british))));
        }

        if self.speaker > 0 {
            synth
                .set_speaker(self.speaker)
                .map_err(|e| TtsError(format!("set_speaker: {e:#}")))?;
        }

        let synth = Arc::new(synth);
        *guard = Some((key, Arc::clone(&synth)));
        Ok(synth)
    }

    /// Shared pump: streams audio + events from a synthesis, feeding the
    /// callbacks. Returns collected `(pcm bytes, boundaries)`.
    #[allow(clippy::too_many_arguments)]
    fn pump(
        &self,
        stream: floravox_core::synth::StreamingSynthesis,
        volume: f32,
        mut on_audio: Option<crate::engine::OnAudioCallback<'_>>,
        mut on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
        mut on_mark: Option<crate::engine::OnMarkCallback<'_>>,
        collect: bool,
    ) -> TtsResult<(Vec<u8>, Vec<WordBoundary>)> {
        let StreamingSynthesis {
            audio,
            events,
            result,
        } = stream;
        let mut bytes = Vec::new();
        let mut boundaries = Vec::new();
        self.cancel.store(false, Ordering::SeqCst);

        let drain_event = |ev: SynthesisEvent,
                           on_boundary: &mut Option<crate::engine::OnBoundaryCallback<'_>>,
                           on_mark: &mut Option<crate::engine::OnMarkCallback<'_>>,
                           boundaries: &mut Vec<WordBoundary>| {
            match ev {
                SynthesisEvent::WordBoundary(w) => {
                    if let Some(cb) = on_boundary.as_mut() {
                        #[allow(clippy::cast_precision_loss)]
                        let (s, e) = (w.ms_start as f32 / 1000.0, w.ms_end as f32 / 1000.0);
                        #[allow(clippy::cast_possible_wrap)]
                        cb(
                            &w.text,
                            s,
                            e,
                            w.byte_offset as i32,
                            w.byte_len as i32,
                            w.estimated,
                        );
                    }
                    boundaries.push(WordBoundary {
                        text: w.text,
                        offset: w.ms_start,
                        duration: w.ms_end.saturating_sub(w.ms_start),
                        estimated: w.estimated,
                    });
                }
                SynthesisEvent::MarkReached {
                    name,
                    ms,
                    char_offset,
                    ..
                } => {
                    // Sample-accurate → measured. Also a zero-duration
                    // boundary so boundary-only consumers see marks.
                    if let Some(cb) = on_mark.as_mut() {
                        #[allow(clippy::cast_precision_loss)]
                        let s = ms as f32 / 1000.0;
                        #[allow(clippy::cast_possible_wrap)]
                        cb(&name, s, s, char_offset as i32);
                    }
                    if let Some(cb) = on_boundary.as_mut() {
                        #[allow(clippy::cast_precision_loss)]
                        let s = ms as f32 / 1000.0;
                        cb(&name, s, s, char_offset as i32, 0, false);
                    }
                    boundaries.push(WordBoundary {
                        text: name,
                        offset: ms,
                        duration: 0,
                        estimated: false,
                    });
                }
                _ => {}
            }
        };

        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Ok((bytes, boundaries)); // dropping the receivers cancels the worker
            }
            // Drain pending events first so boundaries precede the audio
            // they time.
            while let Ok(ev) = events.try_recv() {
                drain_event(ev, &mut on_boundary, &mut on_mark, &mut boundaries);
            }
            match audio.recv_timeout(POLL) {
                Ok(chunk) => {
                    let scaled = apply_volume(&chunk.samples, volume);
                    let pcm = samples_to_le_bytes(&scaled);
                    if collect {
                        bytes.extend_from_slice(&pcm);
                    }
                    if let Some(cb) = on_audio.as_mut() {
                        cb(&pcm);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // Audio done; drain the remaining events, then surface
                    // the worker outcome — an inference failure must not
                    // masquerade as success with empty audio (floravox
                    // issue #9 semantics).
                    for ev in events {
                        drain_event(ev, &mut on_boundary, &mut on_mark, &mut boundaries);
                    }
                    if let Ok(Err(err)) = result.recv() {
                        return Err(TtsError(format!("floravox synthesis: {err:#}")));
                    }
                    return Ok((bytes, boundaries));
                }
            }
        }
    }
}

/// Default models dir: `~/.rust-tts-wrapper/floravox`.
fn default_models_dir() -> PathBuf {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|k| std::env::var_os(k).map(|h| (!h.is_empty()).then(|| PathBuf::from(h))))
        .flatten()
        .map_or_else(
            || PathBuf::from(".floravox"),
            |h| h.join(".rust-tts-wrapper").join("floravox"),
        )
}

/// Scale f32 samples by a volume factor (clamped).
fn apply_volume(samples: &[f32], volume: f32) -> Vec<f32> {
    if (volume - 1.0).abs() < f32::EPSILON {
        return samples.to_vec();
    }
    samples
        .iter()
        .map(|&s| (s * volume.clamp(0.0, 4.0)).clamp(-1.0, 1.0))
        .collect()
}

/// f32 mono samples → 16-bit little-endian PCM bytes.
#[allow(clippy::cast_possible_truncation)]
fn samples_to_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Wrap input with a prosody rate when the caller asked for non-default.
/// Plain text is XML-escaped and wrapped; SSML input keeps its own markup
/// with an outer prosody inserted inside `<speak>`.
fn wrap_rate(text: &str, rate: f32) -> String {
    if (rate - 1.0).abs() < f32::EPSILON {
        return text.to_string();
    }
    let rate = rate.clamp(0.1, 10.0);
    let is_ssml = text.trim_start().to_ascii_lowercase().starts_with("<speak");
    if is_ssml {
        // Insert an outer prosody around the inner content.
        match text.find('>') {
            Some(end_open) if text[..end_open].contains("<speak") => {
                let inner = &text[end_open + 1..];
                let close = inner.rfind("</speak>").unwrap_or(inner.len());
                format!(
                    "{}<prosody rate=\"{rate:.3}\">{}</prosody>{}",
                    &text[..=end_open],
                    &inner[..close],
                    &inner[close..]
                )
            }
            _ => text.to_string(),
        }
    } else {
        format!(
            "<speak><prosody rate=\"{rate:.3}\">{}</prosody></speak>",
            escape_text(text)
        )
    }
}

/// Build the input floravox receives: SpeechMarkdown (the `floravox`
/// feature always enables it) is converted to the generic SSML dialect —
/// which floravox parses natively — and normalized; rate wraps an outer
/// prosody. Exposed for tests.
fn prepare_input(text: &str, rate: f32) -> String {
    let (processed, _is_ssml) = crate::engine::preprocess_speech_markdown(text, "floravox");
    let normalized = processed.replace(
        "<amazon:effect name=\"whispered\">",
        "<prosody volume=\"soft\" rate=\"0.85\">",
    );
    let normalized = normalized.replace("</amazon:effect>", "</prosody>");
    wrap_rate(&normalized, rate)
}

/// XML-escape plain text being wrapped into SSML.
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// True for vocoder-style file names (excluded when picking the acoustic
/// model out of a voice directory — matcha voices pair the two).
fn is_vocoder_name(p: &Path) -> bool {
    p.file_name().is_some_and(|n| {
        let n = n.to_string_lossy().to_ascii_lowercase();
        n.contains("hifigan") || n.contains("vocoder") || n.contains("vocos")
    })
}

/// Find the acoustic `.onnx` file for a candidate path: the path itself
/// when it is one, or the single non-vocoder `*.onnx` inside a directory.
fn find_onnx(cand: &Path) -> Option<PathBuf> {
    if cand.extension().and_then(|e| e.to_str()) == Some("onnx")
        && cand.is_file()
        && !is_vocoder_name(cand)
    {
        return Some(cand.to_path_buf());
    }
    if cand.is_dir() {
        let mut onnx: Vec<PathBuf> = std::fs::read_dir(cand)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()) == Some("onnx") && !is_vocoder_name(p)
            })
            .collect();
        if onnx.len() == 1 {
            return onnx.pop();
        }
    }
    None
}

/// One discovered voice on disk.
struct DiscoveredVoice {
    id: String,
    name: String,
    bcp47: String,
    iso639_3: String,
}

/// Scan `dir` for voices: subdirectories (or the flat dir itself)
/// holding `X.onnx` + `X.onnx.json`.
fn scan_voices(dir: &Path) -> Vec<DiscoveredVoice> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(onnx) = find_onnx(&path) {
            if let Some(v) = describe_voice(&entry.file_name().to_string_lossy(), &onnx) {
                out.push(v);
            }
        }
    }
    // Flat layout: X.onnx + X.onnx.json directly in dir.
    if out.is_empty() {
        if let Some(onnx) = find_onnx(dir) {
            if let Some(v) = describe_voice(
                &dir.file_name().unwrap_or_default().to_string_lossy(),
                &onnx,
            ) {
                out.push(v);
            }
        }
    }
    out
}

/// Read a voice's `X.onnx.json` for language + name metadata.
fn describe_voice(id: &str, onnx: &Path) -> Option<DiscoveredVoice> {
    let json = onnx.with_extension("onnx.json");
    let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(json).ok()?).ok()?;
    let espeak_voice = raw
        .pointer("/espeak/voice")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let dataset = raw
        .get("dataset")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let (bcp47, iso) = bcp47_from(espeak_voice, dataset);
    Some(DiscoveredVoice {
        name: id.replace(['_', '-'], " "),
        id: id.to_string(),
        bcp47,
        iso639_3: iso.to_string(),
    })
}

/// Derive a BCP-47 tag from the piper config: prefer `espeak.voice`
/// (`"en-us"`), else the dataset prefix (`"en_US-lessac-low"`).
fn bcp47_from(espeak_voice: &str, dataset: &str) -> (String, &'static str) {
    let raw = if espeak_voice.is_empty() {
        dataset.split(['-', '_']).next().unwrap_or("").to_string()
    } else {
        espeak_voice.to_string()
    };
    let raw = raw.replace('_', "-");
    // Normalise "en-us" → "en-US" (region uppercase when 2 letters).
    let mut parts = raw.split('-');
    let lang = parts.next().unwrap_or("").to_ascii_lowercase();
    let region = parts.next().map(str::to_ascii_uppercase);
    let bcp47 = match &region {
        Some(r) if r.len() == 2 => format!("{lang}-{r}"),
        _ => lang.clone(),
    };
    let iso = match lang.as_str() {
        "en" => "eng",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "nl" => "nld",
        "pl" => "pol",
        "ru" => "rus",
        "sv" => "swe",
        "da" => "dan",
        "nb" | "no" => "nor",
        "fi" => "fin",
        "cs" => "ces",
        "sk" => "slk",
        "hu" => "hun",
        "ro" => "ron",
        "el" => "ell",
        "tr" => "tur",
        "ar" => "ara",
        "hi" => "hin",
        "zh" | "cmn" | "yue" => "zho",
        "ja" => "jpn",
        "ko" => "kor",
        "vi" => "vie",
        "th" => "tha",
        _ => "",
    };
    (bcp47, iso)
}

impl TtsEngine for FloravoxEngine {
    #[allow(clippy::too_many_arguments)]
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        volume: f32,
        on_audio: Option<crate::engine::OnAudioCallback<'_>>,
        on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
        on_mark: Option<crate::engine::OnMarkCallback<'_>>,
    ) -> TtsResult<()> {
        let input = prepare_input(text, rate);
        let synth = self.synthesizer(voice)?;
        let stream = synth
            .synthesize_stream(&input)
            .map_err(|e| TtsError(format!("floravox synthesis: {e:#}")))?;
        self.pump(stream, volume, on_audio, on_boundary, on_mark, false)
            .map(|_| ())
    }

    #[allow(clippy::too_many_arguments)]
    fn speak_sync(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        pitch: f32,
        volume: f32,
        on_audio: Option<crate::engine::OnAudioCallback<'_>>,
        on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
        on_mark: Option<crate::engine::OnMarkCallback<'_>>,
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
        self.cancel.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn get_voices(&self) -> TtsResult<Vec<Voice>> {
        Ok(scan_voices(&self.models_dir)
            .into_iter()
            .map(|v| Voice {
                name: v.name,
                id: v.id,
                gender: Gender::Unknown,
                provider: "floravox".to_string(),
                language_codes: vec![LanguageCode {
                    display: crate::types::locale_display_name(&v.bcp47),
                    bcp47: v.bcp47,
                    iso639_3: v.iso639_3,
                }],
            })
            .collect())
    }

    fn check_credentials(&self) -> TtsResult<bool> {
        // A usable configuration = the models dir holds at least one
        // voice, or an explicit modelId is configured (and will resolve
        // on first synthesis). No ONNX session is opened here.
        Ok(find_onnx(&self.models_dir).is_some()
            || !self
                .model_id
                .lock()
                .map_err(|_| TtsError("model_id lock poisoned".into()))?
                .is_empty())
    }

    fn engine_id(&self) -> &'static str {
        "floravox"
    }

    /// floravox reports *measured* word timings from the model's duration
    /// tensor (patched voices) — never the default length-based estimates.
    fn synth_with_boundaries(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        volume: f32,
    ) -> TtsResult<(Vec<u8>, Vec<WordBoundary>)> {
        let input = prepare_input(text, rate);
        let synth = self.synthesizer(voice)?;
        let stream = synth
            .synthesize_stream(&input)
            .map_err(|e| TtsError(format!("floravox synthesis: {e:#}")))?;
        self.pump(stream, volume, None, None, None, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_to_bytes_roundtrip() {
        let bytes = samples_to_le_bytes(&[0.0, 1.0, -1.0, 0.5]);
        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[0..2], &[0, 0]);
        assert_eq!(&bytes[2..4], &[0xFF, 0x7F]); // i16::MAX LE
        assert_eq!(&bytes[4..6], &[0x01, 0x80]); // i16::MIN LE
    }

    #[test]
    fn volume_scaling_clamps() {
        assert!((apply_volume(&[0.5], 2.0)[0] - 1.0).abs() < 1e-6);
        assert!(apply_volume(&[0.5], 0.0)[0].abs() < 1e-6);
        assert!((apply_volume(&[0.5], 1.0)[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn rate_wrapping_plain_and_ssml() {
        assert_eq!(wrap_rate("hi", 1.0), "hi");
        let wrapped = wrap_rate("hi & bye", 1.5);
        assert!(wrapped.contains("<prosody rate=\"1.500\">"));
        assert!(wrapped.contains("hi &amp; bye"));
        // SSML input keeps its own markup, gets an outer prosody.
        let ssml = wrap_rate("<speak>hello</speak>", 0.8);
        assert!(ssml.starts_with("<speak>"));
        assert!(ssml.contains("<prosody rate=\"0.800\">hello</prosody>"));
    }

    #[test]
    fn speechmarkdown_routes_through_ssml() {
        // SpeechMarkdown input is expanded to the generic SSML dialect,
        // which floravox parses natively.
        let out = prepare_input("Wait [500ms] then ++speak++ up", 1.0);
        assert!(out.contains("<break"), "break survives: {out}");
        assert!(out.contains("<emphasis"), "emphasis survives: {out}");
    }

    #[test]
    fn config_parses_credentials() {
        let cfg = Config::parse(
            r#"{"modelsDir": "~/voices", "modelId": "en-lessac", "misaki": "gb", "chars": "hin", "speaker": 3}"#,
        );
        // `~` expands at parse time (HOME on unix, USERPROFILE on Windows).
        let expanded = cfg
            .models_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        assert!(
            expanded.as_deref().is_some_and(|p| p.ends_with("voices")),
            "expanded: {expanded:?}"
        );
        assert_eq!(cfg.model_id.as_deref(), Some("en-lessac"));
        assert_eq!(cfg.misaki.as_deref(), Some("gb"));
        assert_eq!(cfg.chars.as_deref(), Some("hin"));
        assert_eq!(cfg.speaker, Some(3));
        // Garbage JSON → all defaults.
        let cfg = Config::parse("not json");
        assert!(cfg.models_dir.is_none());
        assert_eq!(cfg.speaker, None);
    }

    #[test]
    fn event_mapping_measured_vs_estimated() {
        // WordTiming maps to WordBoundary with the estimated flag passed
        // through untouched — the contract floravox depends on.
        let w = floravox_core::WordTiming {
            text: "hello".into(),
            byte_offset: 0,
            byte_len: 5,
            char_offset: 0,
            char_len: 5,
            sample_start: 0,
            sample_end: 2400,
            ms_start: 0,
            ms_end: 100,
            estimated: false,
        };
        let ev = SynthesisEvent::WordBoundary(w);
        assert_eq!(ev.sample(), 0);
        assert_eq!(ev.ms(), 0);
    }

    #[test]
    fn engine_debug_and_id() {
        let engine = FloravoxEngine::new(r#"{"modelId": "x"}"#);
        assert_eq!(engine.engine_id(), "floravox");
        assert!(format!("{engine:?}").contains("FloravoxEngine"));
    }

    #[test]
    fn misaki_credential_normalizes() {
        let engine = FloravoxEngine::new(r#"{"misaki": "GB"}"#);
        assert_eq!(engine.misaki, "gb");
        let engine = FloravoxEngine::new("{}");
        assert_eq!(engine.misaki, "us");
    }
}
