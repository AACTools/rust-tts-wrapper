//! floravox offline TTS engine: event-driven SSML synthesis for
//! piper/MMS VITS and Matcha (+vocoder) ONNX voices.
//!
//! What this adds over the sherpa-onnx engine for piper voices:
//!
//! * **Measured word timings** — voices patched with
//!   `add_durations_output.py` (floravox's duration-graph surgery) report
//!   word boundaries derived from the acoustic model's own duration
//!   tensor, sample-accurate, instead of 150-wpm estimates. Unpatched
//!   voices still work; their boundaries are flagged estimates.
//! * **Native SSML** — `<break>`, `<prosody rate>`, `<mark>`, `<phoneme>`,
//!   `<sub>`, `<say-as>` are parsed locally (byte/char-exact spans), no
//!   round-trip to a cloud dialect.
//! * **Pluggable G2P** — lexicon (CMUDict/WikiPron FST) + Phonetisaurus
//!   WFST + ByT5 OOV fallbacks, all optional via credentials JSON, with a
//!   bounded LRU so repeated words cost one hash lookup.
//!
//! Audio is delivered as 16-bit little-endian mono PCM chunks, the same
//! shape as the sherpa-onnx engine. `pitch` is ignored (VITS-family
//! voices have no pitch control).

use crate::engine::TtsEngine;
use crate::types::{Gender, LanguageCode, TtsError, TtsResult, Voice, WordBoundary};
use floravox_core::synth::{CharFrontend, MisakiPrePass, StreamingSynthesis, Synthesizer};
use floravox_core::SynthesisEvent;
use floravox_core::VoiceBackend;
use floravox_g2p::{
    Byt5G2p, CachedPhonemizer, ChainedFallback, FstLexicon, LexiconPhonemizer, OovFallback,
    PhonetisaurusG2p, RuleFallback,
};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The concrete phonemizer stack the engine builds: lexicon (optional)
/// with a chained OOV fallback, behind a bounded LRU cache.
/// (`LexiconPhonemizer`'s `D` is the lexicon *storage* type.)
type Phon = CachedPhonemizer<LexiconPhonemizer<Vec<u8>, Box<dyn OovFallback + Send>>>;

/// Poll interval while interleaving audio chunks and events from the
/// synthesis channels (also caps stop() latency).
const POLL: Duration = Duration::from_millis(25);

/// Engine configuration from credentials JSON.
#[derive(Debug, Default)]
struct Config {
    models_dir: Option<PathBuf>,
    model_id: Option<String>,
    lexicon: Option<PathBuf>,
    phonetisaurus: Option<PathBuf>,
    byt5_encoder: Option<PathBuf>,
    byt5_decoder: Option<PathBuf>,
    /// "us" | "gb": document-level misaki pre-pass (English).
    misaki: Option<String>,
    /// Character-level frontend for MMS-style voices, with optional
    /// uroman romanization ("true" | an ISO 639-3 code).
    chars: Option<String>,
    /// ISO language code: with the floravox-lexicons feature, resolves
    /// the published lexicon bundle for the voice's language when no
    /// explicit lexicon is configured.
    lang: Option<String>,
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
            lexicon: get("lexicon"),
            phonetisaurus: get("phonetisaurus"),
            byt5_encoder: get("byt5Encoder"),
            byt5_decoder: get("byt5Decoder"),
            misaki: v
                .get("misaki")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            chars: v
                .get("chars")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            lang: v
                .get("lang")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
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
    lexicon: Option<PathBuf>,
    phonetisaurus: Option<PathBuf>,
    byt5_encoder: Option<PathBuf>,
    byt5_decoder: Option<PathBuf>,
    /// "us" | "gb" — document-level misaki pre-pass (English voices).
    misaki: Option<String>,
    /// Character-level frontend (MMS voices); the string is an optional
    /// ISO 639-3 code for uroman language-specific rules, "" for plain
    /// romanization, or the value "true" for plain lowercasing only.
    chars: Option<String>,
    /// Voice language (BCP-47 or ISO code); with `floravox-lexicons`,
    /// resolves the published bundle when no explicit lexicon is set.
    lang: Option<String>,
    /// Set by `stop()`; the streaming pump drops its channels in response,
    /// which cancels the synthesis worker within one poll interval.
    cancel: Arc<AtomicBool>,
    /// Cached synthesizer, keyed by the voice + g2p options it was built
    /// with (rebuilding reloads the ONNX session, so it is worth caching).
    synth: Mutex<Option<(String, Arc<Synthesizer<Phon>>)>>,
}
impl fmt::Debug for FloravoxEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FloravoxEngine")
            .field("models_dir", &self.models_dir)
            .field("model_id", &self.model_id.lock().map(|g| g.clone()))
            .field("lexicon", &self.lexicon)
            .field("phonetisaurus", &self.phonetisaurus)
            .finish_non_exhaustive()
    }
}

impl FloravoxEngine {
    /// Create a new floravox engine.
    ///
    /// Credentials JSON keys (all optional):
    /// - `modelsDir`: directory of piper voices (defaults to
    ///   `~/.rust-tts-wrapper/floravox`). A voice is any directory (or flat
    ///   pair) holding `X.onnx` + `X.onnx.json`.
    /// - `modelId`: voice to load (directory or file stem). Voices are also
    ///   selectable per-call via `speak(voice = Some(...))`.
    /// - `lexicon`: compiled FST lexicon stem (`stem.fst` + `stem.pho`;
    ///   from `floravox-fst-compile` or a voicegarden-lexicons bundle).
    /// - `phonetisaurus`: Phonetisaurus WFST model path (`.fst`, tables
    ///   embedded or beside).
    /// - `byt5Encoder` / `byt5Decoder`: ByT5 ONNX pair for OOV words.
    /// - `lang`: language code; with the `floravox-lexicons` feature,
    ///   fetches the published bundle for that language (lexicon +
    ///   trained OOV WFST) when `lexicon`/`phonetisaurus` are not set.
    /// - `misaki`: `"us"` or `"gb"` — document-level English pre-pass
    ///   (the phonemizer Kokoro was trained with; heteronyms and
    ///   numbers come out right).
    /// - `chars`: character-level frontend for MMS-style voices.
    ///   `"true"` lowercases and feeds characters through the voice's
    ///   own table; any other string is an ISO 639-3 code and input is
    ///   romanized (uroman) first, e.g. `"hin"`.
    ///
    /// OOV chain order: lexicon → Phonetisaurus → ByT5 → letter spelling.
    pub fn new(credentials_json: &str) -> Self {
        let cfg = Config::parse(credentials_json);
        Self {
            models_dir: cfg.models_dir.unwrap_or_else(default_models_dir),
            model_id: Mutex::new(cfg.model_id.unwrap_or_default()),
            lexicon: cfg.lexicon,
            phonetisaurus: cfg.phonetisaurus,
            byt5_encoder: cfg.byt5_encoder,
            byt5_decoder: cfg.byt5_decoder,
            misaki: cfg.misaki,
            chars: cfg.chars,
            lang: cfg.lang,
            cancel: Arc::new(AtomicBool::new(false)),
            synth: Mutex::new(None),
        }
    }

    /// Resolve the onnx path for a voice selector: a path under
    /// `models_dir` (dir with a single `.onnx`, or a direct `.onnx` file,
    /// or a bare stem).
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
    fn synthesizer(&self, voice: Option<&str>) -> TtsResult<Arc<Synthesizer<Phon>>> {
        self.synthesizer_for(voice, None)
    }

    /// Resolve the synthesizer, letting an SSML `<speak xml:lang="...">`
    /// stand in for a `lang` credential (per-utterance language routing;
    /// the cache key covers it, so a language switch reuses the model
    /// path but rebuilds the phonemizer only when the key differs).
    fn synthesizer_for(
        &self,
        voice: Option<&str>,
        doc_lang: Option<&str>,
    ) -> TtsResult<Arc<Synthesizer<Phon>>> {
        #[cfg_attr(not(feature = "floravox-lexicons"), allow(unused_variables))]
        let effective_lang = self.lang.as_deref().or(doc_lang);
        let onnx = self.resolve_model(voice)?;
        let key = format!(
            "{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            onnx.display(),
            self.lexicon,
            self.phonetisaurus,
            self.byt5_encoder,
            self.byt5_decoder,
            self.misaki,
            self.chars,
            effective_lang,
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
        let mut synth = Synthesizer::new(model, build_phonemizer(self, effective_lang));

        // Document-level pre-passes, in order of specificity:
        //   explicit chars credential > auto-detected character table
        //   (MMS-style voices) > misaki (English)
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
        } else if let Some(dialect) = self.misaki.as_deref() {
            let british = dialect.eq_ignore_ascii_case("gb");
            synth = synth.with_document_phonemizer(Box::new(MisakiPrePass(
                floravox_g2p::MisakiG2p::new(british),
            )));
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
        stream: StreamingSynthesis,
        volume: f32,
        mut on_audio: Option<crate::engine::OnAudioCallback<'_>>,
        mut on_boundary: Option<crate::engine::OnBoundaryCallback<'_>>,
        mut on_mark: Option<crate::engine::OnMarkCallback<'_>>,
        collect: bool,
    ) -> TtsResult<(Vec<u8>, Vec<WordBoundary>)> {
        let StreamingSynthesis { audio, events } = stream;
        let mut bytes = Vec::new();
        let mut boundaries = Vec::new();
        self.cancel.store(false, Ordering::SeqCst);
        let fire_mark =
            |name: &str,
             ms: u64,
             char_offset: i64,
             on_mark: &mut Option<crate::engine::OnMarkCallback<'_>>| {
                if let Some(cb) = on_mark.as_mut() {
                    #[allow(clippy::cast_precision_loss)]
                    let s = ms as f32 / 1000.0;
                    #[allow(clippy::cast_possible_wrap)]
                    cb(name, s, s, char_offset as i32);
                }
            };
        let fire_boundary = |w: &floravox_core::WordTiming,
                             on_boundary: &mut Option<crate::engine::OnBoundaryCallback<'_>>,
                             boundaries: &mut Vec<WordBoundary>| {
            if let Some(cb) = on_boundary.as_mut() {
                #[allow(clippy::cast_precision_loss)]
                let (s, e) = (w.ms_start as f32 / 1000.0, w.ms_end as f32 / 1000.0);
                #[allow(clippy::cast_possible_wrap)]
                cb(
                    &w.text,
                    s,
                    e,
                    w.char_offset as i32,
                    w.char_len as i32,
                    w.estimated,
                );
            }
            boundaries.push(WordBoundary {
                text: w.text.clone(),
                offset: w.ms_start,
                duration: w.ms_end.saturating_sub(w.ms_start),
                estimated: w.estimated,
            });
        };
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Ok((bytes, boundaries)); // dropping the receivers cancels the worker
            }
            // Drain pending events first so boundaries precede the audio
            // they time.
            while let Ok(ev) = events.try_recv() {
                match ev {
                    SynthesisEvent::WordBoundary(w) => {
                        fire_boundary(&w, &mut on_boundary, &mut boundaries);
                    }
                    SynthesisEvent::MarkReached {
                        name,
                        ms,
                        char_offset,
                        ..
                    } => {
                        fire_mark(&name, ms, char_offset, &mut on_mark);
                    }
                    _ => {}
                }
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
                    // Audio done; drain the remaining events.
                    for ev in events {
                        match ev {
                            SynthesisEvent::WordBoundary(w) => {
                                fire_boundary(&w, &mut on_boundary, &mut boundaries);
                            }
                            SynthesisEvent::MarkReached {
                                name,
                                ms,
                                char_offset,
                                ..
                            } => {
                                fire_mark(&name, ms, char_offset, &mut on_mark);
                            }
                            _ => {}
                        }
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
        .find_map(|k| {
            let h = std::env::var_os(k)?;
            (!h.is_empty()).then(|| PathBuf::from(h).join(".rust-tts-wrapper").join("floravox"))
        })
        .unwrap_or_else(|| PathBuf::from(".floravox"))
}

/// Build the phonemizer stack from the engine's g2p options.
/// OOV chain: Phonetisaurus → ByT5 → letter spelling (first hit wins).
#[cfg_attr(not(feature = "floravox-lexicons"), allow(unused_variables))]
fn build_phonemizer(engine: &FloravoxEngine, effective_lang: Option<&str>) -> Phon {
    // Resolve the lexicon stem: explicit `lexicon` config wins; with the
    // floravox-lexicons feature, the published bundle for the voice's
    // language (which also carries a trained Phonetisaurus WFST) is
    // fetched and cached on first use.
    let lexicon_stem = engine.lexicon.clone();
    #[allow(unused_mut)]
    let mut phonetisaurus = engine.phonetisaurus.clone();
    #[allow(unused_mut)]
    let mut lexicon_stem = lexicon_stem;
    #[cfg(feature = "floravox-lexicons")]
    if let Some(lang) = effective_lang {
        if lexicon_stem.is_none() && phonetisaurus.is_none() {
            match voicegarden_lexicons::LexiconArchive::default_archive()
                .and_then(|a| a.fetch(lang))
            {
                Ok(bundle) => {
                    let dir = bundle.dir.clone();
                    let lang_id = bundle.entry.lang.clone();
                    let has_wfst = bundle.phonetisaurus.is_some();
                    drop(bundle);
                    lexicon_stem = Some(dir.join(&lang_id));
                    if has_wfst {
                        phonetisaurus = Some(dir.join("phonetisaurus.fst"));
                    }
                }
                Err(e) => {
                    eprintln!("floravox: lexicon bundle for {lang:?} unavailable: {e:#}");
                }
            }
        }
    }

    // OOV chain: Phonetisaurus -> ByT5 -> letter spelling.
    let mut fallback: Box<dyn OovFallback + Send> = Box::new(RuleFallback::default());
    if let (Some(enc), Some(dec)) = (&engine.byt5_encoder, &engine.byt5_decoder) {
        if let Ok(byt5) = Byt5G2p::load(enc, dec) {
            fallback = Box::new(ChainedFallback(byt5, fallback));
        }
    }
    if let Some(model) = &phonetisaurus {
        if let Ok(ph) = PhonetisaurusG2p::open(model) {
            fallback = Box::new(ChainedFallback(ph, fallback));
        }
    }
    let lexicon = lexicon_stem
        .as_deref()
        .and_then(|stem| floravox_g2p::MmapLexicon::open(stem).ok())
        .map_or_else(
            || FstLexicon::from_rows(Vec::new()).expect("empty lexicon"),
            |m| m.to_mem(),
        );
    CachedPhonemizer::new(LexiconPhonemizer::new(lexicon, fallback), 1024)
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

/// Build the input floravox receives: SpeechMarkdown (when enabled) is
/// converted to the generic SSML dialect and normalized for floravox;
/// rate wraps an outer prosody. Exposed for tests.
fn prepare_input(text: &str, rate: f32) -> String {
    #[cfg(feature = "speechmarkdown")]
    let text = {
        let (processed, _is_ssml) = crate::engine::preprocess_speech_markdown(text, "floravox");
        normalize_ssml_for_floravox(&processed)
    };
    #[cfg(not(feature = "speechmarkdown"))]
    let text = text.to_string();
    wrap_rate(&text, rate)
}

/// Document language: `<speak xml:lang="...">` when present. Used for
/// lexicon-bundle routing when no explicit `lang` credential is set.
fn document_lang(text: &str) -> Option<String> {
    let doc = floravox_ssml::parse(text).ok()?;
    doc.lang
}

/// Map vendor-specific SSML elements from the generic SpeechMarkdown
/// dialect onto floravox-supported equivalents. floravox's parser treats
/// unknown tags as transparent containers (their text still renders), so
/// this is fidelity polish, not correctness: today only whisper has a
/// better mapping than speak-normally.
fn normalize_ssml_for_floravox(ssml: &str) -> String {
    ssml.replace(
        "<amazon:effect name=\"whispered\">",
        "<prosody volume=\"soft\" rate=\"0.85\">",
    )
    .replace("</amazon:effect>", "</prosody>")
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

/// One discovered voice on disk.
struct DiscoveredVoice {
    id: String,
    name: String,
    bcp47: String,
    iso639_3: String,
    sample_rate: u32,
}

/// Scan `dir` for piper voices: subdirectories (or the flat dir itself)
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
    let sample_rate = raw
        .pointer("/audio/sample_rate")
        .and_then(serde_json::Value::as_u64)
        .map_or_else(|| 16_000, |v| u32::try_from(v).unwrap_or(16_000));
    let (bcp47, iso) = bcp47_from(espeak_voice, dataset);
    Some(DiscoveredVoice {
        name: id.replace(['_', '-'], " "),
        id: id.to_string(),
        bcp47,
        iso639_3: iso.to_string(),
        sample_rate,
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
        let synth = self.synthesizer_for(voice, document_lang(text).as_deref())?;
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

    fn engine_id(&self) -> &'static str {
        "floravox"
    }

    /// floravox reports *measured* word timings from the model's duration
    /// tensor (patched voices) — not the default length-based estimates.
    fn synth_with_boundaries(
        &self,
        text: &str,
        voice: Option<&str>,
        rate: f32,
        _pitch: f32,
        volume: f32,
    ) -> TtsResult<(Vec<u8>, Vec<WordBoundary>)> {
        let input = prepare_input(text, rate);
        let synth = self.synthesizer_for(voice, document_lang(text).as_deref())?;
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
        let ssml = wrap_rate("<speak><break time=\"100ms\"/>ok</speak>", 0.8);
        assert!(ssml.starts_with("<speak><prosody rate=\"0.800\"><break"));
        assert!(ssml.ends_with("</prosody></speak>"));
    }

    #[test]
    fn bcp47_mapping() {
        assert_eq!(bcp47_from("en-us", ""), ("en-US".to_string(), "eng"));
        assert_eq!(
            bcp47_from("", "de_DE-thorsten-high"),
            ("de".to_string(), "deu")
        );
        assert_eq!(bcp47_from("fr-fr", ""), ("fr-FR".to_string(), "fra"));
        assert_eq!(bcp47_from("", "xx_unknown"), ("xx".to_string(), ""));
    }

    #[test]
    fn voice_scanning_discovers_piper_layout() {
        let dir = tempfile::tempdir().unwrap();
        let voice = dir.path().join("en_US-lessac-low");
        std::fs::create_dir(&voice).unwrap();
        std::fs::write(voice.join("en_US-lessac-low.onnx"), b"fake").unwrap();
        std::fs::write(
            voice.join("en_US-lessac-low.onnx.json"),
            r#"{"espeak":{"voice":"en-us"},"audio":{"sample_rate":16000},
                "phoneme_id_map":{"a":[1]},"num_speakers":1}"#,
        )
        .unwrap();
        let voices = scan_voices(dir.path());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "en_US-lessac-low");
        assert_eq!(voices[0].bcp47, "en-US");
        assert_eq!(voices[0].iso639_3, "eng");
        assert_eq!(voices[0].sample_rate, 16_000);

        // get_voices through the engine
        let engine =
            FloravoxEngine::new(&format!("{{\"modelsDir\":\"{}\"}}", dir.path().display()));
        let list = engine.get_voices().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].provider, "floravox");
        assert_eq!(list[0].language_codes[0].bcp47, "en-US");
        assert_eq!(engine.engine_id(), "floravox");
    }

    #[test]
    #[cfg(feature = "speechmarkdown")]
    fn speechmarkdown_flows_into_floravox_ssml() {
        // SpeechMarkdown inline modifiers → generic-dialect SSML that
        // floravox parses natively (break, prosody rate, sub, say-as).
        let input = prepare_input(
            "[250ms] Hello (fast)[rate:2] (WWW)[sub:\"World Wide Web\"] (hi)[chars]",
            1.0,
        );
        assert!(input.contains("<break"), "{input}");
        assert!(input.contains("prosody"), "{input}");
        assert!(input.contains("<sub alias="), "{input}");
        assert!(input.contains("say-as"), "{input}");
    }

    #[test]
    #[cfg(feature = "speechmarkdown")]
    fn whisper_maps_to_floravox_prosody() {
        let input = prepare_input("(be very quiet)[whisper]", 1.0);
        assert!(
            input.contains("<prosody volume=\"soft\" rate=\"0.85\">"),
            "{input}"
        );
        assert!(!input.contains("amazon:effect"), "{input}");
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        assert_eq!(prepare_input("plain words", 1.0), "plain words");
        // rate wrapping still applies
        assert!(prepare_input("plain words", 1.5).contains("prosody"));
    }

    #[test]
    fn document_lang_is_extracted() {
        assert_eq!(
            document_lang(r#"<speak xml:lang="de-DE">Guten Tag</speak>"#).as_deref(),
            Some("de-DE")
        );
        assert!(document_lang("plain text").is_none());
    }

    #[test]
    fn missing_model_errors_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let engine =
            FloravoxEngine::new(&format!("{{\"modelsDir\":\"{}\"}}", dir.path().display()));
        let Err(err) = engine.synthesizer(None) else {
            panic!("expected an error for a missing modelId");
        };
        assert!(err.0.contains("modelId"));
    }
}
