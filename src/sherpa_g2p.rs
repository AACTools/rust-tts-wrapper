//! Wrapper-side G2P front-end — the phonemize path (#82 Phase 3).
//!
//! sherpa-onnx 2.x offers two text front-ends for espeak-orphaned
//! models: the built-in lexicon (Phase 2's `lexiconCompat`) and
//! caller-side phonemization via `GenerationConfig::phoneme_codepoints`.
//! This module is the latter: dictionary lookup + Phonetisaurus OOV
//! from the voicegarden bundle, mapped through the Phase 2 vocabulary
//! ladder, handed to sherpa as per-sentence Unicode codepoints. OOV
//! words are SPOKEN (the lexicon front-end drops them — upstream
//! k2-fsa/sherpa-onnx#4139); numbers are normalized to German words.
//!
//! The codepoint contract (pinned from sherpa-onnx 2.0.1
//! `PiperPhonemesToIdsVits`): the caller supplies the BARE phoneme
//! stream per sentence — words joined by literal space codepoints,
//! punctuation as its own token (`,` short pause, `.` long pause,
//! `-` break, the piper convention [`punct_phonemes`] already
//! emits). sherpa adds BOS/EOS/pad itself and SKIPS unknown
//! codepoints with a log — which is why every symbol this module
//! emits is validated single-codepoint AND in the model's
//! `tokens.txt` before it can reach the stream.
//!
//! Costs are kept off the first utterance where possible: the bundle
//! is provisioned once (fetched with manifest sha256 verification, or
//! found pre-cached with its `.sha256` stamp), the dictionary is
//! mmap'd (~0 resident), and the Phonetisaurus WFST (~50 MB for deu)
//! opens LAZILY on the first dictionary miss.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use floravox_g2p::{
    ChainedFallback, LexiconPhonemizer, MmapLexicon, OovFallback, PhonetisaurusG2p, RuleFallback,
    TokenPhonemizer,
};
use unicode_normalization::UnicodeNormalization as _;

use crate::sherpa_lang::Archive;
use crate::sherpa_lexicon::{read_tokens_txt_public, VocabPhonemeMap};

/// The WFST, opened on the first dictionary miss. Opening it eagerly
/// would put ~50 MB resident on every voice load for the many
/// utterances that never meet an OOV word.
struct LazyWfst {
    path: PathBuf,
    state: OnceLock<WfstState>,
}

enum WfstState {
    Loaded(PhonetisaurusG2p),
    /// open failed (damaged/missing fst): fall through to the rule
    /// fallback rather than refusing to speak
    Failed(RuleFallback),
}

impl LazyWfst {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: OnceLock::new(),
        }
    }
}

impl OovFallback for LazyWfst {
    fn fallback(&mut self, word: &str) -> Vec<String> {
        if self.state.get().is_none() {
            let _ = self.state.set(match PhonetisaurusG2p::open(&self.path) {
                Ok(g) => WfstState::Loaded(g),
                Err(_) => WfstState::Failed(RuleFallback::default()),
            });
        }
        // &mut self ⇒ get_mut is lock-free (the Mutex is only needed
        // if the state were shared; it is not — this keeps OnceLock's
        // poisoning semantics out of the OOV path)
        match self.state.get_mut().expect("just initialised") {
            WfstState::Loaded(g) => g.fallback(word),
            WfstState::Failed(r) => r.fallback(word),
        }
    }
}

/// The front-end for one (model, bundle) pair: the phonemizer chain
/// plus the Phase 2 vocabulary mapper. Build once per engine, reuse
/// across utterances (the word cache makes repeats hash lookups).
pub struct G2pFrontend {
    chain: Box<dyn floravox_g2p::TokenPhonemizer + Send>,
    mapper: VocabPhonemeMap,
}

/// Why a front-end could not be built for a model. One-line reasons;
/// the engine logs each once and falls back to the plain-text path.
pub enum FrontendError {
    /// the model vocabulary contains multi-character tokens the
    /// codepoint path cannot address (sherpa maps one codepoint → one
    /// token id; a multi-char symbol is unreachable)
    MultiCharVocabulary(String),
    /// dictionary/vocabulary open failure (I/O, empty vocabulary)
    Open(String),
}

impl std::fmt::Debug for FrontendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

impl std::fmt::Display for FrontendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MultiCharVocabulary(m) => write!(
                f,
                "model vocabulary has multi-character token(s) {m} — the \
                 phoneme_codepoints path cannot address them; falling back"
            ),
            Self::Open(e) => write!(f, "opening G2P front-end: {e}"),
        }
    }
}

/// Word-level outcome of phonemizing one utterance.
pub struct PhonemizeResult {
    /// One codepoint vector per sentence (the bare phoneme stream;
    /// sherpa adds BOS/EOS/pad). Empty when every word was dropped —
    /// the caller must then fall back to the text path, because
    /// sherpa treats empty codepoints as "use the text argument".
    pub sentences: Vec<Vec<i32>>,
    /// words processed
    pub words: usize,
    /// words dropped (unmappable after dictionary AND OOV fallback)
    pub dropped: usize,
}

impl G2pFrontend {
    /// Build from a provisioned bundle directory (`<key>.fst` +
    /// `<key>.pho` + optional `phonetisaurus.fst`) and the model's
    /// `tokens.txt`.
    ///
    /// # Errors
    /// [`FrontendError::MultiCharVocabulary`] when the vocabulary has
    /// multi-character tokens (per-model refusal, plan Phase 3);
    /// [`FrontendError::Open`] for I/O or parse failures.
    pub fn open(bundle_dir: &Path, key: &str, tokens_path: &Path) -> Result<Self, FrontendError> {
        let lex = MmapLexicon::open(bundle_dir.join(format!("{key}.fst")))
            .map_err(|e| FrontendError::Open(format!("lexicon {key}.fst: {e}")))?;
        // to_mem(): LexiconPhonemizer needs D: AsRef<[u8]> and the
        // mmap-backed FstLexicon does not expose it (same call as
        // floravox_engine's chain); the de dictionary is ~2 MB
        let lex = lex.to_mem();
        let wfst = LazyWfst::new(bundle_dir.join("phonetisaurus.fst"));
        let rules = RuleFallback::default();
        let chain = floravox_g2p::CachedPhonemizer::new(
            LexiconPhonemizer::new(lex, ChainedFallback(wfst, rules)),
            1024,
        );
        let vocab = read_tokens_txt_public(tokens_path)
            .map_err(|e| FrontendError::Open(format!("{}: {e}", tokens_path.display())))?;
        if let Some(offender) = vocab.iter().find(|t| t.chars().count() > 1) {
            return Err(FrontendError::MultiCharVocabulary(offender.clone()));
        }
        let mapper = VocabPhonemeMap::for_language(tokens_path)
            .map_err(|e| FrontendError::Open(e.to_string()))?;
        Ok(Self {
            chain: Box::new(chain),
            mapper,
        })
    }

    /// Phonemize an utterance into per-sentence codepoint streams.
    /// Tokens that are all digits are first expanded to German words
    /// (minimal normalizer); each remaining token is looked up in the
    /// bundle dictionary (case-insensitive, NFC-normalized input),
    /// falling through to the Phonetisaurus WFST on a miss; every
    /// emitted symbol is mapped through the vocabulary ladder and
    /// must be a single in-vocabulary codepoint or the word is
    /// dropped (counted, never fatal).
    pub fn phonemize(&mut self, text: &str) -> PhonemizeResult {
        let mut sentences = Vec::new();
        let mut words = 0usize;
        let mut dropped = 0usize;
        for sentence in split_sentences(text) {
            let mut codepoints: Vec<i32> = Vec::new();
            for token in sentence.split_whitespace() {
                // Numeric tokens (#82 loop reviews, live-verified): a
                // token whose word CORE STARTS with a digit must expand
                // to German or DROP — never reach the fallback chain,
                // which spells alphanumeric junk in ENGLISH ("1e9" →
                // "wʌniːnaɪn"). Covered: plain/strict-thousands/
                // decimal numerals (digits_to_german) and
                // digits+alphabetic-suffix forms ("1920er" → the
                // German number + "er", a normal word for the
                // dictionary/WFST). Letter-leading mixes ("b2b") stay
                // ordinary words. Leading zeros collapse ("007" →
                // "sieben") — accepted for quantities, noted for codes.
                let expanded: String;
                let token = {
                    let core = floravox_g2p::word_core(token).unwrap_or_default();
                    if core.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        let Some(german) =
                            digits_to_german(core).or_else(|| digits_plus_suffix(core))
                        else {
                            // unhandled numeric ("1e9", ">999_999_999",
                            // malformed separators): drop, counted
                            dropped += 1;
                            words += 1;
                            continue;
                        };
                        expanded = reattach_punct(token, &german);
                        expanded.as_str()
                    } else {
                        token
                    }
                };
                words += 1;
                // NFC first (decomposed input composes to the key form),
                // then phonemize (dictionary keys are lowercase; the
                // FST lexicon lowercases its lookup)
                let nfc: String = token.nfc().collect();
                let phones = self.chain.phonemize_token(&nfc);
                let mut mapped: Vec<char> = Vec::with_capacity(phones.len());
                let mut ok = true;
                for sym in &phones {
                    let Some(parts) = self.mapper.map_symbol(sym) else {
                        ok = false;
                        break;
                    };
                    for p in parts {
                        // single codepoints only — the whole stream is
                        // address-by-codepoint
                        let mut chars = p.chars();
                        if let (Some(c), None) = (chars.next(), chars.next()) {
                            mapped.push(c);
                        } else {
                            ok = false;
                        }
                    }
                }
                if !ok || mapped.is_empty() {
                    dropped += 1;
                    continue;
                }
                if !codepoints.is_empty() {
                    codepoints.push(' ' as i32); // inter-word space token
                }
                codepoints.extend(mapped.iter().map(|c| *c as i32));
            }
            if !codepoints.is_empty() {
                sentences.push(codepoints);
            }
        }
        PhonemizeResult {
            sentences,
            words,
            dropped,
        }
    }
}

/// Rebuild a token around an expanded core: the non-alphanumeric edges
/// of the original (the punctuation `word_core` stripped) are reattached
/// so `punct_phonemes` still sees them ("42." → "zweiundvierzig.").
/// NOTE: a leading sign lands after the number ("-42" → "zweiundvierzig-")
/// — floravox appends pause symbols after the core, inverting the
/// intended break; accepted for now (negative numbers in TTS input are
/// rare), revisit if the Phase 4 corpus shows otherwise.
fn reattach_punct(original: &str, expanded_core: &str) -> String {
    let chars: Vec<char> = original.chars().collect();
    let first = chars.iter().position(|c| c.is_alphanumeric());
    let last = chars.iter().rposition(|c| c.is_alphanumeric());
    let mut out = String::with_capacity(expanded_core.len() + 4);
    if let (Some(f), Some(l)) = (first, last) {
        out.extend(chars[..f].iter());
        out.push_str(expanded_core);
        out.extend(chars[l + 1..].iter());
    } else {
        out.push_str(expanded_core);
    }
    out
}

/// Split text into sentences on the #4075 delimiter set (`. ! ? ; :`
/// plus newlines), with two refinements: a `.` BETWEEN digits is a
/// decimal/thousands separator, not a sentence end ("1.5", "3.500"),
/// and chunks with no alphanumeric character ("...", the newline
/// after a `?`) are skipped — with `max_num_sentences=1` each would
/// otherwise trigger a separate generate call for a lone pause token.
fn split_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    let mut it = text.chars().peekable();
    while let Some(ch) = it.next() {
        let next = it.peek().copied();
        let between_digits =
            prev.is_some_and(|p| p.is_ascii_digit()) && next.is_some_and(|n| n.is_ascii_digit());
        let is_delim = match ch {
            '.' if between_digits => false,
            '.' | '!' | '?' | ';' | ':' | '\n' | '\r' => true,
            _ => false,
        };
        cur.push(ch);
        if is_delim {
            if cur.chars().any(char::is_alphanumeric) {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
        prev = Some(ch);
    }
    if cur.chars().any(char::is_alphanumeric) {
        out.push(cur);
    }
    out
}

/// Minimal German digit normalizer. Handles:
/// - plain integers 0..=999_999_999 ("42" → "zweiundvierzig",
///   "1000000" → "einemillion", "2000000" → "zweimillionen")
/// - thousands-separated integers ("3.500" → "dreitausendfünfhundert")
/// - decimals ("1,5" → "einskommafünf" — German reads the digits after
///   the comma individually: "1,58" → "einskommafünfacht")
///
/// `None` when the token is not covered (beyond 999_999_999, malformed
/// separators) — the caller DROPS the word with a count rather than
/// letting the rule fallback spell English letter names into a German
/// stream (the loop-2 review caught exactly that, live).
fn digits_to_german(token: &str) -> Option<String> {
    // decimal form: integer part (optionally thousands-dotted, strict
    // groups — the canonical German money shape "3.500,50"), comma,
    // digits ("1,5" / "1,58" / "3.500,50")
    if let Some((int_part, frac)) = token.split_once(',') {
        let frac_ok = !frac.is_empty() && frac.chars().all(|c| c.is_ascii_digit());
        let int_ok = !int_part.is_empty()
            && int_part.chars().all(|c| c.is_ascii_digit() || c == '.')
            && digits_to_german(int_part).is_some();
        if !frac_ok || !int_ok {
            return None;
        }
        let mut out = digits_to_german(int_part)?;
        out.push_str("komma");
        for c in frac.chars() {
            out.push_str(&integer_to_german(&c.to_string())?);
        }
        return Some(out);
    }
    // integer form: digits with optional thousands dots in strict
    // groups of three ("3.500", "1.000.000"); a stray dot ("1.5",
    // "3.50.00") is rejected rather than guessed
    if token.is_empty() {
        return None;
    }
    let groups: Vec<&str> = token.split('.').collect();
    let all_digits = groups
        .iter()
        .all(|g| !g.is_empty() && g.chars().all(|c| c.is_ascii_digit()));
    if !all_digits {
        return None;
    }
    let strict_thousands = groups.len() == 1
        || groups.iter().enumerate().all(|(i, g)| {
            if i == 0 {
                (1..=3).contains(&g.len())
            } else {
                g.len() == 3
            }
        });
    if !strict_thousands {
        return None; // malformed separators: drop rather than guess
    }
    let plain: String = token.chars().filter(|c| *c != '.').collect();
    integer_to_german(&plain)
}

/// Digits + a short alphabetic suffix ("1920er", "50er", "1ste"):
/// expand the numeric part and keep the suffix, producing a normal
/// alphabetic word ("neunzehnhundertzwanziger") that the dictionary
/// or WFST pronounces German-ly. `None` when the shape does not match
/// (mixed alphanumerics like "1e9" — those drop).
fn digits_plus_suffix(core: &str) -> Option<String> {
    let idx = core
        .char_indices()
        .find(|(_, c)| c.is_ascii_alphabetic())
        .map(|(i, _)| i)
        .filter(|&i| i > 0)?;
    let (num, suffix) = core.split_at(idx);
    // suffix: short and purely alphabetic ("er", "ste"); the numeric
    // part must be a valid numeral in its own right. "1e9" fails here
    // (suffix "e9" is not alphabetic) and drops upstream.
    if suffix.len() > 4 || !suffix.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(format!(
        "{}{}",
        digits_to_german(num)?,
        suffix.to_lowercase()
    ))
}

/// 0..=999_999_999 in German words (thousands dots stripped by the
/// caller). `None` beyond range or for non-digit input.
fn integer_to_german(digits: &str) -> Option<String> {
    const UNITS: [&str; 10] = [
        "null", "eins", "zwei", "drei", "vier", "fünf", "sechs", "sieben", "acht", "neun",
    ];
    const TEENS: [&str; 10] = [
        "zehn",
        "elf",
        "zwölf",
        "dreizehn",
        "vierzehn",
        "fünfzehn",
        "sechzehn",
        "siebzehn",
        "achtzehn",
        "neunzehn",
    ];
    const TENS: [&str; 10] = [
        "", "", "zwanzig", "dreißig", "vierzig", "fünfzig", "sechzig", "siebzig", "achtzig",
        "neunzig",
    ];
    /// 1..=999: "eins" standalone, "ein" in compounds ("ein"hundert,
    /// hunder"ein", "ein"undzwanzig)
    fn under_hundred(n: u64) -> String {
        let hundreds = n / 100;
        let rest = n % 100;
        let mut out = String::new();
        if hundreds > 0 {
            out.push_str(if hundreds == 1 {
                "ein"
            } else {
                UNITS[hundreds as usize]
            });
            out.push_str("hundert");
        }
        match rest {
            0 => {}
            1..=9 => out.push_str(UNITS[rest as usize]),
            10..=19 => out.push_str(TEENS[(rest - 10) as usize]),
            _ => {
                let tens_idx = (rest / 10) as usize;
                let units_idx = (rest % 10) as usize;
                if units_idx == 0 {
                    out.push_str(TENS[tens_idx]);
                } else {
                    out.push_str(if units_idx == 1 {
                        "ein"
                    } else {
                        UNITS[units_idx]
                    });
                    out.push_str("und");
                    out.push_str(TENS[tens_idx]);
                }
            }
        }
        out
    }

    let n = digits.parse::<u64>().ok()?;
    if n > 999_999_999 {
        return None;
    }
    if n < 10 {
        return Some(UNITS[n as usize].to_string());
    }
    if (10..20).contains(&n) {
        return Some(TEENS[(n - 10) as usize].to_string());
    }
    let mut parts: Vec<String> = Vec::new();
    let millions = n / 1_000_000;
    let rest = n % 1_000_000;
    if millions > 0 {
        // "einemillion" / "zweimillionen" (compound form of eine/zwei)
        parts.push(if millions == 1 {
            "einemillion".to_string()
        } else {
            format!("{}millionen", under_hundred(millions))
        });
    }
    if rest > 0 {
        let thousands = rest / 1000;
        let tail = rest % 1000;
        if thousands > 0 {
            parts.push(format!(
                "{}tausend",
                if thousands == 1 {
                    "ein".to_string()
                } else {
                    under_hundred(thousands)
                }
            ));
        }
        if tail > 0 {
            parts.push(under_hundred(tail));
        }
    }
    Some(parts.join(""))
}

// ──────────────────────────────────────────────────────────────────
// Provisioning (model setup time; never inside the audio path)
// ──────────────────────────────────────────────────────────────────

/// Provision (fetch when stale or absent) the bundle named by an
/// allowlist row. The cache check mirrors the crate's own contract
/// EXACTLY: the `.sha256` stamp must match the manifest's sha (not
/// merely exist — a stale bundle from an older release must refresh),
/// plus both FST halves present under `<cache>/<key>/` (both archives
/// cache flat in the pinned crate). Constructs EXACTLY the row's
/// archive — the crate matches entries on `lang`/`bcp47`, so the
/// expanded archive asked for "de" would silently resolve `deu` (the
/// Phase 1 archive trap); with the pinned archive, a wrong key is an
/// error, not a wrong bundle.
///
/// The archive's manifest itself is network-fetched once and then
/// cached at `<cache>/lexicons.json` (offline installs fall back to
/// that file), so an offline machine with a provisioned bundle never
/// touches the network here.
///
/// # Errors
/// A one-line message (network, checksum, manifest); the engine logs
/// it once and retries on a later utterance (throttled).
pub fn provision_bundle(archive: Archive, key: &str) -> Result<PathBuf, String> {
    let pinned = match archive {
        Archive::Base => voicegarden_lexicons::LexiconArchive::default_archive(),
        Archive::Expanded => voicegarden_lexicons::LexiconArchive::default_expanded(),
    }
    .map_err(|e| format!("lexicon archive: {e}"))?;
    provision_from(&pinned, &voicegarden_lexicons::default_cache_dir(), key)
}

/// The cache-hit / fetch decision against an explicit archive + cache
/// (tests drive this with a local-dir base so the whole decision is
/// provable offline).
fn provision_from(
    pinned: &voicegarden_lexicons::LexiconArchive,
    cache: &std::path::Path,
    key: &str,
) -> Result<PathBuf, String> {
    let entry = pinned
        .entry(key)
        .ok_or_else(|| format!("no '{key}' bundle in the archive manifest"))?;
    let dir = cache.join(key);
    let stamp_matches =
        std::fs::read_to_string(dir.join(".sha256")).ok().as_deref() == Some(entry.sha256.as_str());
    if stamp_matches
        && dir.join(format!("{key}.fst")).exists()
        && dir.join(format!("{key}.pho")).exists()
    {
        return Ok(dir);
    }
    let bundle = pinned
        .fetch(key)
        .map_err(|e| format!("fetching bundle '{key}': {e}"))?;
    Ok(bundle.dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    /// A mini offline bundle: two dictionary rows via LexiconWriter,
    /// no WFST (so OOV falls through to the rule fallback and mostly
    /// drops — deterministic for tests).
    fn mini_bundle(dir: &Path) {
        floravox_g2p::LexiconWriter::new(dir.join("mini"))
            .write(vec![
                ("haus".into(), "h aʊ̯ s".into()),
                ("guten".into(), "ɡ uː t ə n".into()),
                ("tag".into(), "t aː k".into()),
                ("zweiundvierzig".into(), "ts v aɪ̯ ç ɪ ç".into()),
                ("einskommafünf".into(), "aɪ n s k o m a f ʏ n f".into()),
            ])
            .expect("write mini lexicon");
    }

    /// A thorsten-like vocabulary: single-char symbols, all the
    /// punctuation/pause tokens the contract needs, a space token.
    fn mini_tokens(dir: &Path) -> PathBuf {
        let syms = [
            "_", "^", "$", " ", ",", ".", "-", "!", "?", "\u{02C8}", "a", "e", "i", "o", "u", "ɪ",
            "ʊ", "ɐ", "ə", "ɛ", "ʏ", "ç", "ʁ", "b", "d", "f", "h", "k", "l", "m", "n", "p", "s",
            "t", "v", "x", "z", "\u{0261}", "ʃ", "ʒ", "ŋ", "\u{02D0}",
        ];
        let p = dir.join("tokens.txt");
        let mut s = String::new();
        for (i, sym) in syms.iter().enumerate() {
            writeln!(s, "{sym} {i}").expect("in-memory write");
        }
        std::fs::write(&p, s).unwrap();
        p
    }

    #[test]
    fn digits_expand_to_german_words() {
        let d = |t: &str| digits_to_german(t).expect("in range");
        assert_eq!(d("0"), "null");
        assert_eq!(d("3"), "drei");
        assert_eq!(d("16"), "sechzehn");
        assert_eq!(d("17"), "siebzehn"); // irregular
        assert_eq!(d("20"), "zwanzig");
        assert_eq!(d("21"), "einundzwanzig"); // ein, not eins
        assert_eq!(d("42"), "zweiundvierzig");
        assert_eq!(d("100"), "einhundert");
        assert_eq!(d("101"), "einhunderteins");
        assert_eq!(d("121"), "einhunderteinundzwanzig");
        assert_eq!(d("1000"), "eintausend");
        assert_eq!(d("1234"), "eintausendzweihundertvierunddreißig");
        // the loop-2 findings, closed:
        assert_eq!(d("1000000"), "einemillion"); // was English "one zero zero..."
        assert_eq!(d("2000000"), "zweimillionen");
        assert_eq!(
            d("1234567"),
            "einemillionzweihundertvierunddreißigtausendfünfhundertsiebenundsechzig"
        );
        assert_eq!(d("1,5"), "einskommafünf"); // was English "one five"
        assert_eq!(d("1,58"), "einskommafünfacht"); // post-comma digits, German style
        assert_eq!(d("3.500"), "dreitausendfünfhundert"); // strict thousands dots
                                                          // out of scope → None → the caller drops with a count
        assert!(digits_to_german("1000000000").is_none()); // 1e9
        assert!(digits_to_german("1.5").is_none()); // ambiguous separator
        assert!(digits_to_german("3.50.00").is_none()); // malformed groups
        assert!(digits_to_german("x1").is_none());
    }

    #[test]
    fn numeric_sentences_survive_splitting_and_expansion() {
        let dir = tempfile::tempdir().unwrap().keep();
        mini_bundle(&dir);
        let tokens = mini_tokens(&dir);
        let mut fe = G2pFrontend::open(&dir, "mini", &tokens).expect("frontend");

        // '.' between digits does not end a sentence; decimals speak German
        let r = fe.phonemize("Haus 1,5 Tag");
        assert_eq!(r.sentences.len(), 1);
        assert_eq!(r.dropped, 0);
        let sentence: String = r.sentences[0]
            .iter()
            .filter_map(|&cp| char::from_u32(cp.cast_unsigned()))
            .collect();
        // the STREAM carries the dictionary phones for "einskommafünf"
        assert!(
            sentence.contains("a\u{026a}nskomaf\u{028f}nf"),
            "decimal in German: {sentence}"
        );

        // unhandled numerics drop WITH a count, never English spelling
        let r = fe.phonemize("Haus 1000000000 Tag");
        assert_eq!(r.dropped, 1);
        assert_eq!(r.sentences.len(), 1);

        // pause-only sentences are skipped entirely
        assert_eq!(split_sentences("Haus...").len(), 1);
        assert_eq!(split_sentences("Haus... Tag").len(), 2);
    }

    #[test]
    fn sentences_split_on_the_4075_set() {
        let s = split_sentences("Guten Tag. Wie geht es dir; mir gut: wirklich?\nEnde");
        assert_eq!(s.len(), 5);
        assert_eq!(s[0], "Guten Tag.");
        assert_eq!(s[4], "Ende");
        assert_eq!(split_sentences("").len(), 0);
    }

    #[test]
    fn frontend_phonemizes_dictionary_words_with_spaces_and_pauses() {
        let dir = tempfile::tempdir().unwrap().keep();
        mini_bundle(&dir);
        let tokens = mini_tokens(&dir);
        let mut fe = G2pFrontend::open(&dir, "mini", &tokens).expect("frontend");

        let r = fe.phonemize("Guten Tag.");
        assert_eq!((r.words, r.dropped), (2, 0));
        assert_eq!(r.sentences.len(), 1);
        let sentence: String = r.sentences[0]
            .iter()
            .map(|&cp| char::from_u32(cp as u32).unwrap())
            .collect();
        // phones are consecutive codepoints (sherpa maps each codepoint
        // to one token); only WORDS are separated by the space token
        assert_eq!(sentence, "\u{0261}u\u{02D0}t\u{0259}n ta\u{02D0}k.");

        // two sentences, identical words → identical codepoint streams
        let r = fe.phonemize("Haus. Haus!");
        assert_eq!(r.sentences.len(), 2);
        assert_eq!(r.sentences[0], r.sentences[1]); // same word, same mapping
    }

    #[test]
    fn frontend_drops_unmappable_words_and_counts_them() {
        let dir = tempfile::tempdir().unwrap().keep();
        mini_bundle(&dir);
        let tokens = mini_tokens(&dir);
        let mut fe = G2pFrontend::open(&dir, "mini", &tokens).expect("frontend");

        // "xyzzy" is OOV: no WFST in the mini bundle → rule fallback
        // spells English letter names → unmappable → dropped, counted
        let r = fe.phonemize("Haus xyzzy Tag");
        assert_eq!(r.words, 3);
        assert_eq!(r.dropped, 1);
        assert_eq!(r.sentences.len(), 1); // the two good words survive

        // everything dropped → empty sentences (caller falls back to text)
        let r = fe.phonemize("qqqq");
        assert!(r.sentences.is_empty());
        assert_eq!(r.dropped, 1);
    }

    #[test]
    fn frontend_expands_digit_tokens_with_attached_punctuation() {
        let dir = tempfile::tempdir().unwrap().keep();
        mini_bundle(&dir);
        let tokens = mini_tokens(&dir);
        let mut fe = G2pFrontend::open(&dir, "mini", &tokens).expect("frontend");

        // 42 → zweiundvierzig (in the mini dictionary)
        let r = fe.phonemize("42");
        assert_eq!(r.dropped, 0);
        let sentence: String = r.sentences[0]
            .iter()
            .filter_map(|&cp| char::from_u32(cp.cast_unsigned()))
            .collect();
        assert!(sentence.contains('ç'), "zweiundvierzig phones: {sentence}");

        // "42." — the word CORE expands, the pause token survives, and
        // the sentence-final digits never reach the English rule
        // fallback (the bug the loop-1 review caught live). Mini-dict
        // words only ("Antwort" is not in the mini lexicon).
        let r = fe.phonemize("Haus 42.");
        assert_eq!(r.dropped, 0, "every word resolves");
        let sentence: String = r.sentences[0]
            .iter()
            .filter_map(|&cp| char::from_u32(cp.cast_unsigned()))
            .collect();
        assert!(
            sentence.contains('ç'),
            "digits spoken in German: {sentence}"
        );
        assert!(sentence.ends_with('.'), "pause token survives: {sentence}");
    }

    #[test]
    fn frontend_nfc_normalizes_input() {
        let dir = tempfile::tempdir().unwrap().keep();
        // a dictionary word stored precomposed: "schön"
        floravox_g2p::LexiconWriter::new(dir.join("mini"))
            .write(vec![("schön".into(), "ʃ eː n".into())])
            .expect("write mini lexicon");
        let tokens = mini_tokens(&dir);
        let mut fe = G2pFrontend::open(&dir, "mini", &tokens).expect("frontend");
        // decomposed input (s c h o + U+0308 n) composes to the key
        // form — assert the STREAM, not just the drop count (the rule
        // fallback also yields zero drops; the stream pins rule 1)
        let decomposed = "scho\u{0308}n";
        assert_ne!(decomposed, "schön"); // sanity: really decomposed
        let r = fe.phonemize(decomposed);
        let sentence: String = r.sentences[0]
            .iter()
            .filter_map(|&cp| char::from_u32(cp.cast_unsigned()))
            .collect();
        assert_eq!(sentence, "ʃe\u{02D0}n");
    }

    #[test]
    fn multi_char_vocabulary_refuses_the_model() {
        let dir = tempfile::tempdir().unwrap().keep();
        mini_bundle(&dir);
        // a multi-char token (a misaki-style diphthong)
        std::fs::write(dir.join("tokens2.txt"), "a 0\nA 1\nI 2\nai 3\n").unwrap();
        let Err(err) = G2pFrontend::open(&dir, "mini", &dir.join("tokens2.txt")) else {
            panic!("multi-char vocabulary must be refused");
        };
        assert!(matches!(err, FrontendError::MultiCharVocabulary(_)));
        assert!(err.to_string().contains("multi-character"));
    }

    #[test]
    fn numeric_boundary_never_reaches_the_english_fallback() {
        let d = |t: &str| digits_to_german(t).expect("in range");
        // the canonical German money format composes: thousands dots
        // + Komma decimals
        // post-comma digits read INDIVIDUALLY (German convention)
        assert_eq!(d("3.500,50"), "dreitausendfünfhundertkommafünfnull");
        assert_eq!(d("0,5"), "nullkommafünf");
        // digits + short alphabetic suffix → a normal German word
        assert_eq!(
            digits_plus_suffix("1920er").map(|w| w.contains("er")),
            Some(true)
        );
        // the FULL quantity form is used ("eintausendneunhundert…"),
        // not the idiomatic year form ("neunzehnhundert…") — never
        // wrong, just formal; year idioms are a Phase 4 corpus question
        assert_eq!(
            digits_plus_suffix("1920er"),
            Some("eintausendneunhundertzwanziger".to_string())
        );
        // mixed alphanumerics and out-of-range numerals are None → drop
        assert!(digits_to_german("1e9").is_none());
        assert!(digits_plus_suffix("1e9").is_none()); // "e9" is not a suffix
        assert!(digits_to_german("1000000000").is_none());
        // leading zeros collapse (documented: fine for quantities)
        assert_eq!(d("007"), "sieben");
    }

    #[test]
    fn phonemize_drops_unhandled_numerics_and_expands_suffix_forms() {
        let dir = tempfile::tempdir().unwrap().keep();
        // the mini dictionary gains the suffix-expanded word
        floravox_g2p::LexiconWriter::new(dir.join("mini"))
            .write(vec![
                ("haus".into(), "h aʊ̯ s".into()),
                ("tag".into(), "t aː k".into()),
                ("zweiundvierzig".into(), "ts v aɪ̯ ç ɪ ç".into()),
                ("eintausendneunhundertzwanziger".into(), "n o ʏ n".into()),
                (
                    "dreitausendfünfhundertkommafünfnull".into(),
                    "d ʁ aɪ".into(),
                ),
            ])
            .expect("write mini lexicon");
        let tokens = mini_tokens(&dir);
        let mut fe = G2pFrontend::open(&dir, "mini", &tokens).expect("frontend");

        // "1920er" expands then resolves via the dictionary
        let r = fe.phonemize("Haus 1920er Tag");
        assert_eq!((r.words, r.dropped), (3, 0));
        let sentence: String = r.sentences[0]
            .iter()
            .filter_map(|&cp| char::from_u32(cp.cast_unsigned()))
            .collect();
        assert!(
            sentence.contains('ʏ') || sentence.contains('ɔ'),
            "{sentence}"
        );

        // money format speaks
        let r = fe.phonemize("Haus 3.500,50 Tag");
        assert_eq!(r.dropped, 0, "money format must speak");

        // "1e9" drops with a count — never English phones
        let r = fe.phonemize("Haus 1e9 Tag");
        assert_eq!(r.dropped, 1);
        assert_eq!(r.sentences.len(), 1);

        // letter-leading digit mixes stay on the WORD path (never the
        // numeric-drop path): with the real bundle the WFST pronounces
        // them; in this WFST-less fixture the rule fallback may spell
        // them — either way the sentence survives
        let r = fe.phonemize("Haus b2b Tag");
        assert_eq!(r.sentences.len(), 1);
        assert!(!r.sentences[0].is_empty());
    }

    #[test]
    fn provisioning_stamp_must_match_the_manifest_sha() {
        // a local-dir archive base with a mini manifest: cache hits and
        // stale-stamp refreshes are provable offline (no tarball is
        // staged, so any fetch attempt fails loudly)
        let base = tempfile::tempdir().unwrap().keep();
        let cache = tempfile::tempdir().unwrap().keep();
        std::fs::write(
            base.join("lexicons.json"),
            r#"{"version":"t","languages":[{"lang":"de","name":"German","bcp47":"de",
                "entries":1,"license":"MIT","source":"t","phonetisaurus":null,
                "file":"de.tar.gz","sha256":"abc123"}]}"#,
        )
        .unwrap();
        let base_str = base.display().to_string();
        let archive = voicegarden_lexicons::LexiconArchive::new(base_str, &cache).expect("archive");

        // wrong sha → refresh attempt → fetch fails (no tarball) → Err
        let dir = cache.join("de");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("de.fst"), b"x").unwrap();
        std::fs::write(dir.join("de.pho"), b"x").unwrap();
        std::fs::write(dir.join(".sha256"), b"stale").unwrap();
        assert!(
            provision_from(&archive, &cache, "de").is_err(),
            "stale stamp must refresh"
        );

        // matching sha + halves → cache hit with NO tarball present
        std::fs::write(dir.join(".sha256"), b"abc123").unwrap();
        let got = provision_from(&archive, &cache, "de").expect("cache hit");
        assert_eq!(got, dir);
    }

    // ── real-bundle coverage (env-gated, like Phase 2's) ──
    //
    //   SHERPA_TEST_THORSTEN_TOKENS=.../tokens.txt \
    //   SHERPA_TEST_DE_DIR=.../voicegarden/lexicons/de \
    //   cargo test --features sherpaonnx,sherpa-g2p --lib sherpa_g2p -- --ignored

    fn env_val(var: &str) -> Option<PathBuf> {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.exists())
    }

    #[test]
    #[ignore = "needs the real thorsten tokens.txt + de bundle dir (SHERPA_TEST_THORSTEN_TOKENS, SHERPA_TEST_DE_DIR)"]
    fn real_bundle_phonemizes_a_german_sentence() {
        let (Some(tokens), Some(bundle)) = (
            env_val("SHERPA_TEST_THORSTEN_TOKENS"),
            env_val("SHERPA_TEST_DE_DIR"),
        ) else {
            eprintln!("skipping: env vars not set");
            return;
        };
        let mut fe = G2pFrontend::open(&bundle, "de", &tokens).expect("frontend");
        let r = fe.phonemize("Guten Morgen! Ich habe 42 Häuser in Donaudampfschifffahrt gesehen.");
        assert_eq!(r.dropped, 0, "every word must resolve");
        assert_eq!(r.sentences.len(), 2);
        // every codepoint must be a token (unknown ones would be
        // silently skipped inside sherpa — verify BEFORE synthesis)
        let vocab = read_tokens_txt_public(&tokens).unwrap();
        for sentence in &r.sentences {
            for &cp in sentence {
                let c = char::from_u32(cp as u32).unwrap();
                assert!(
                    vocab.contains(&c.to_string()),
                    "codepoint {c:?} not in vocab"
                );
            }
        }
    }

    #[test]
    #[ignore = "needs the real de bundle dir (SHERPA_TEST_DE_DIR)"]
    fn real_bundle_oov_word_goes_through_the_wfst() {
        let Some(bundle) = env_val("SHERPA_TEST_DE_DIR") else {
            eprintln!("skipping: env var not set");
            return;
        };
        // tokens from the bundle dir's sibling model? use thorsten env too
        let tokens = env_val("SHERPA_TEST_THORSTEN_TOKENS").expect("tokens env");
        let mut fe = G2pFrontend::open(&bundle, "de", &tokens).expect("frontend");
        // a nonce word: not in the dictionary, must still be spoken
        // (WFST guess) — dropped == 0 is the assertion that matters
        let r = fe.phonemize("Blorpinxel");
        assert_eq!(r.dropped, 0, "OOV must go through the WFST, not drop");
        assert_eq!(r.sentences.len(), 1);
    }
}
