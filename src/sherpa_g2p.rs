//! Wrapper-side G2P front-end — the phonemize path (#82 Phase 3).
//!
//! sherpa-onnx 2.x offers two text front-ends for espeak-orphaned
//! models: the built-in lexicon (Phase 2's `lexiconCompat`) and
//! caller-side phonemization via `GenerationConfig::phoneme_codepoints`.
//! This module is the latter: dictionary lookup + Phonetisaurus OOV
//! from the voicegarden bundle, mapped through the Phase 2 vocabulary
//! ladder, handed to sherpa as per-sentence Unicode codepoints. OOV
//! words are SPOKEN (the lexicon front-end drops them — upstream
//! k2-fsa/sherpa-onnx#4139); numbers are normalized to words in the
//! bundle's language (per-language `LangNormalizer`s: German since
//! #82, English since the #92 codepoints migration, fr/es/pt/it/ru
//! since the #92 v0.4.0 locale flips, Russian included).
//!
//! The codepoint contract (pinned from sherpa-onnx 2.0.1
//! `PiperPhonemesToIdsVits`): the caller supplies the BARE phoneme
//! stream per sentence — words joined by literal space codepoints,
//! punctuation as its own token (`,` short pause, `.` long pause,
//! `-` break, the piper convention `punct_phonemes` already
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

use floravox_g2p_fst::{
    ChainedFallback, LexiconPhonemizer, MmapLexicon, OovFallback, PhonetisaurusG2p, RuleFallback,
    TokenPhonemizer,
};
use unicode_normalization::UnicodeNormalization as _;

use crate::sherpa_lang::Archive;
use crate::sherpa_lexicon::{read_tokens_txt_public, VocabPhonemeMap};
use voicegarden_lexicons::{LexiconArchive, Manifest};

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
    chain: Box<dyn floravox_g2p_fst::TokenPhonemizer + Send>,
    mapper: VocabPhonemeMap,
    normalizer: Box<dyn LangNormalizer>,
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

/// Per-language text normalization for the phonemize path (#92 work
/// item A): how a token containing digits becomes words the
/// dictionary can answer. The first version hard-wired German rules
/// for every language — French `1,5` and Spanish `3.500` were read
/// with German decimal/thousands conventions. One implementation per
/// language; unhandled languages get an identity normalizer whose
/// digit tokens DROP (counted), never English letter-spelling.
pub trait LangNormalizer: Send {
    /// The BCP-47 primary subtag this normalizer handles ("de").
    fn lang(&self) -> &'static str;
    /// Expand a digit-bearing word core to words the dictionary
    /// answers, or None when the numeric shape is unhandled (the
    /// caller drops the token with a count). `core` is the
    /// edge-punctuation-stripped core of the original token.
    fn normalize_numeric(&self, core: &str) -> Option<String>;
}

/// German: decimals with Komma, strict thousands dots, decade
/// suffixes, mixed alphanumeric run-splitting (#82 loop reviews).
struct GermanNormalizer;

impl LangNormalizer for GermanNormalizer {
    fn lang(&self) -> &'static str {
        "de"
    }
    fn normalize_numeric(&self, core: &str) -> Option<String> {
        digits_to_german(core)
            .or_else(|| digits_plus_suffix(core))
            .or_else(|| split_digit_runs(core))
    }
}

/// The identity normalizer for languages without number rules yet:
/// digit-bearing tokens are UNHANDLED (dropped with a count by the
/// caller) — speaking them via the English rule fallback is the bug
/// class the #82 loop reviews eliminated, so no expansion beats
/// wrong-language expansion.
struct IdentityNormalizer;

impl LangNormalizer for IdentityNormalizer {
    fn lang(&self) -> &'static str {
        ""
    }
    fn normalize_numeric(&self, _core: &str) -> Option<String> {
        None
    }
}

/// The normalizer for a bundle language. Known languages get their
/// own; everything else gets the identity (drop-count) behavior.
#[must_use]
pub fn normalizer_for(lang: &str) -> Box<dyn LangNormalizer> {
    match iso639_3_to_primary(&primary_subtag(lang)).as_str() {
        "de" => Box::new(GermanNormalizer),
        "fr" => Box::new(FrenchNormalizer),
        "es" => Box::new(SpanishNormalizer),
        "pt" => Box::new(PortugueseNormalizer),
        "it" => Box::new(ItalianNormalizer),
        "en" => Box::new(EnglishNormalizer),
        "ru" => Box::new(RussianNormalizer),
        "ca" => Box::new(CatalanNormalizer),
        "cs" => Box::new(CzechNormalizer),
        "da" => Box::new(DanishNormalizer),
        "nl" => Box::new(DutchNormalizer),
        "pl" => Box::new(PolishNormalizer),
        "sk" => Box::new(SlovakNormalizer),
        "sl" => Box::new(SlovenianNormalizer),
        "sv" => Box::new(SwedishNormalizer),
        // batch-2 campaign (#92 wave 2)
        "af" => Box::new(AfrikaansNormalizer),
        "cy" => Box::new(WelshNormalizer),
        "eu" => Box::new(BasqueNormalizer),
        "hu" => Box::new(HungarianNormalizer),
        "is" => Box::new(IcelandicNormalizer),
        "ka" => Box::new(GeorgianNormalizer),
        "kk" => Box::new(KazakhNormalizer),
        "ko" => Box::new(KoreanNormalizer),
        "lb" => Box::new(LuxembourgishNormalizer),
        "lv" => Box::new(LatvianNormalizer),
        "ml" => Box::new(MalayalamNormalizer),
        "ne" => Box::new(NepaliNormalizer),
        // nb is the Bokmål alias (nob routing, #108)
        "no" | "nb" => Box::new(NorwegianNormalizer),
        "sq" => Box::new(AlbanianNormalizer),
        "sr" => Box::new(SerbianNormalizer),
        "uk" => Box::new(UkrainianNormalizer),
        _ => Box::new(IdentityNormalizer),
    }
}

/// Fold an ISO 639-3 corpus tag to its 639-1 primary (deu→de, fra→fr,
/// spa→es, …) so bundle keys route once regardless of which manifest
/// they come from. Sync when voicegarden-lexicons adds languages.
fn iso639_3_to_primary(tag: &str) -> String {
    const T3_TO_T2: &[(&str, &str)] = &[
        ("ara", "ar"),
        ("ben", "bn"),
        ("cat", "ca"),
        ("ces", "cs"),
        ("dan", "da"),
        ("deu", "de"),
        ("ell", "el"),
        ("eng", "en"),
        ("fas", "fa"),
        ("fin", "fi"),
        ("fra", "fr"),
        ("heb", "he"),
        ("hin", "hi"),
        ("ind", "id"),
        ("isl", "is"),
        ("ita", "it"),
        ("nld", "nl"),
        ("nob", "nb"),
        ("pol", "pl"),
        ("por", "pt"),
        ("ron", "ro"),
        ("rus", "ru"),
        ("slk", "sk"),
        ("slv", "sl"),
        ("spa", "es"),
        ("swe", "sv"),
        ("tam", "ta"),
        ("tur", "tr"),
        ("urd", "ur"),
    ];
    T3_TO_T2
        .iter()
        .find(|(t3, _)| *t3 == tag)
        .map_or_else(|| tag.to_string(), |(_, t2)| (*t2).to_string())
}

// ──────────────────────────────────────────────────────────────────
// French
// ──────────────────────────────────────────────────────────────────

/// French digits: decimal comma, space/dot thousands, vigesimal
/// cardinals (quatre-vingt-dix), mille not "un mille".
struct FrenchNormalizer;

impl LangNormalizer for FrenchNormalizer {
    fn lang(&self) -> &'static str {
        "fr"
    }
    fn normalize_numeric(&self, core: &str) -> Option<String> {
        fr_normalize(core)
    }
}

fn fr_normalize(core: &str) -> Option<String> {
    if let Some((int_part, frac)) = core.split_once(',') {
        let ok = !frac.is_empty()
            && frac.chars().all(|c| c.is_ascii_digit())
            && !int_part.is_empty()
            && fr_int_ok(int_part);
        if !ok {
            return None;
        }
        let mut out = fr_integer(&int_part.replace(['.', ' ', '\u{00A0}'], ""))?;
        out.push_str("virgule");
        for c in frac.chars() {
            out.push_str(fr_d(c));
        }
        return Some(out);
    }
    if !fr_int_ok(core) {
        return None;
    }
    fr_integer(&core.replace(['.', ' ', '\u{00A0}'], ""))
}

fn fr_int_ok(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // digits with optional dot/space/NBSP thousands separators
    let mut prev_digit = false;
    for c in s.chars() {
        match c {
            '0'..='9' => prev_digit = true,
            '.' | ' ' | '\u{00A0}' if prev_digit => prev_digit = false,
            _ => return false,
        }
    }
    prev_digit
}

fn fr_d(c: char) -> &'static str {
    const D: [&str; 10] = [
        "zéro", "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf",
    ];
    D[c.to_digit(10).unwrap_or(0) as usize]
}

fn fr_integer(digits: &str) -> Option<String> {
    let n: u64 = digits.parse().ok()?;
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("zéro".into());
    }
    let mut out = String::new();
    let m = n / 1_000_000;
    let r = n % 1_000_000;
    if m == 1 {
        out.push_str("unmillion");
    } else if m > 1 {
        out.push_str(&fr_m(m));
        out.push_str("millions");
    }
    if r > 0 {
        out.push_str(&fr_m(r));
    }
    Some(out)
}

fn fr_m(n: u64) -> String {
    let t = n / 1000;
    let rest = n % 1000;
    let mut out = String::new();
    if t == 1 {
        out.push_str("mille");
    } else if t > 1 {
        out.push_str(&fr_1000(t));
        out.push_str("mille");
    }
    if rest > 0 {
        out.push_str(&fr_1000(rest));
    }
    out
}

fn fr_1000(n: u64) -> String {
    let h = n / 100;
    let r = n % 100;
    let mut out = String::new();
    if h == 1 {
        out.push_str("cent");
    } else if h > 1 {
        out.push_str(&fr_100(r_1(h))); // need fr_100 for hundreds digit too
        out.push_str("cent");
        if r == 0 {
            out.push('s');
        } // deuxcents
    }
    if r > 0 {
        out.push_str(&fr_100(r));
    }
    out
}

fn r_1(n: u64) -> u64 {
    n
} // passthrough for now — fr_100 is recursive

fn fr_100(n: u64) -> String {
    const U: [&str; 20] = [
        "", "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf", "dix", "onze",
        "douze", "treize", "quatorze", "quinze", "seize", "dix-sept", "dix-huit", "dix-neuf",
    ];
    if n < 20 {
        return U[n as usize].into();
    }
    match n {
        20 => "vingt".into(),
        21 => "vingt et un".into(),
        22..=29 => format!("vingt-{}", U[(n - 20) as usize]),
        30 => "trente".into(),
        31 => "trente et un".into(),
        32..=39 => format!("trente-{}", U[(n - 30) as usize]),
        40 => "quarante".into(),
        41 => "quarante et un".into(),
        42..=49 => format!("quarante-{}", U[(n - 40) as usize]),
        50 => "cinquante".into(),
        51 => "cinquante et un".into(),
        52..=59 => format!("cinquante-{}", U[(n - 50) as usize]),
        60 => "soixante".into(),
        61 => "soixante et un".into(),
        62..=79 => format!("soixante-{}", U[(n - 60) as usize]),
        // 80-89 = quatre-vingt + 0-9
        80 => "quatre-vingts".into(),
        81..=99 => format!("quatre-vingt-{}", U[(n - 80) as usize]),
        _ => String::new(),
    }
}

// ──────────────────────────────────────────────────────────────────
// Spanish
// ──────────────────────────────────────────────────────────────────

struct SpanishNormalizer;

impl LangNormalizer for SpanishNormalizer {
    fn lang(&self) -> &'static str {
        "es"
    }
    fn normalize_numeric(&self, core: &str) -> Option<String> {
        es_normalize(core)
    }
}

fn es_normalize(core: &str) -> Option<String> {
    if let Some((int_part, frac)) = core.split_once(',') {
        let ok = !frac.is_empty()
            && frac.chars().all(|c| c.is_ascii_digit())
            && !int_part.is_empty()
            && int_part.chars().all(|c| c.is_ascii_digit() || c == '.');
        if !ok {
            return None;
        }
        let mut out = es_integer(&int_part.replace('.', ""))?;
        out.push_str("coma");
        for c in frac.chars() {
            out.push_str(es_d(c));
        }
        return Some(out);
    }
    if core.is_empty() || core.chars().any(|c| !c.is_ascii_digit() && c != '.') {
        return None;
    }
    let groups: Vec<&str> = core.split('.').collect();
    if groups.len() > 1
        && !groups.iter().enumerate().all(|(i, g)| {
            !g.is_empty() && g.chars().all(|c| c.is_ascii_digit()) && (i == 0 || g.len() == 3)
        })
    {
        return None;
    }
    es_integer(&core.replace('.', ""))
}

fn es_d(c: char) -> &'static str {
    const D: [&str; 10] = [
        "cero", "uno", "dos", "tres", "cuatro", "cinco", "seis", "siete", "ocho", "nueve",
    ];
    D[c.to_digit(10).unwrap_or(0) as usize]
}

fn es_integer(digits: &str) -> Option<String> {
    let n: u64 = digits.parse().ok()?;
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("cero".into());
    }
    let mut out = String::new();
    let m = n / 1_000_000;
    let r = n % 1_000_000;
    if m == 1 {
        out.push_str("unmillón");
    } else if m > 1 {
        out.push_str(&es_m(m));
        out.push_str("millones");
    }
    if r > 0 {
        out.push_str(&es_m(r));
    }
    Some(out)
}

fn es_m(n: u64) -> String {
    let t = n / 1000;
    let rest = n % 1000;
    let mut out = String::new();
    if t > 1 {
        out.push_str(&es_1000(t));
    }
    if t > 0 {
        out.push_str("mil");
    }
    if rest > 0 {
        out.push_str(&es_1000(rest));
    }
    out
}

fn es_1000(n: u64) -> String {
    let h = n / 100;
    let r = n % 100;
    if h == 1 && r == 0 {
        return "cien".into(); // exact 100
    }
    let mut out = String::new();
    if h == 1 {
        out.push_str("ciento");
    } else if h > 1 {
        out.push_str(&es_100(h));
        out.push_str("cientos");
    }
    if r > 0 {
        out.push_str(&es_100(r));
    }
    out
}

fn es_100(n: u64) -> String {
    const U: [&str; 30] = [
        "cero",
        "uno",
        "dos",
        "tres",
        "cuatro",
        "cinco",
        "seis",
        "siete",
        "ocho",
        "nueve",
        "diez",
        "once",
        "doce",
        "trece",
        "catorce",
        "quince",
        "dieciséis",
        "diecisiete",
        "dieciocho",
        "diecinueve",
        "veinte",
        "veintiuno",
        "veintidós",
        "veintitrés",
        "veinticuatro",
        "veinticinco",
        "veintiséis",
        "veintisiete",
        "veintiocho",
        "veintinueve",
    ];
    const T: [&str; 10] = [
        "",
        "",
        "",
        "treinta",
        "cuarenta",
        "cincuenta",
        "sesenta",
        "setenta",
        "ochenta",
        "noventa",
    ];
    if n < 30 {
        return U[n as usize].into();
    }
    if n == 100 {
        return "cien".into();
    }
    let t = (n / 10) as usize;
    let u = (n % 10) as usize;
    if u == 0 {
        return T[t].into();
    }
    format!("{} y {}", T[t], U[u])
}

// ──────────────────────────────────────────────────────────────────
// Portuguese
// ──────────────────────────────────────────────────────────────────

struct PortugueseNormalizer;

impl LangNormalizer for PortugueseNormalizer {
    fn lang(&self) -> &'static str {
        "pt"
    }
    fn normalize_numeric(&self, core: &str) -> Option<String> {
        pt_normalize(core)
    }
}

fn pt_normalize(core: &str) -> Option<String> {
    // Portuguese uses comma decimals, dot thousands (like Spanish)
    if let Some((int_part, frac)) = core.split_once(',') {
        let ok = !frac.is_empty()
            && frac.chars().all(|c| c.is_ascii_digit())
            && !int_part.is_empty()
            && int_part.chars().all(|c| c.is_ascii_digit() || c == '.');
        if !ok {
            return None;
        }
        let mut out = pt_integer(&int_part.replace('.', ""))?;
        out.push_str("vírgula");
        for c in frac.chars() {
            out.push_str(pt_d(c));
        }
        return Some(out);
    }
    if core.is_empty() || core.chars().any(|c| !c.is_ascii_digit() && c != '.') {
        return None;
    }
    let groups: Vec<&str> = core.split('.').collect();
    if groups.len() > 1
        && !groups.iter().enumerate().all(|(i, g)| {
            !g.is_empty() && g.chars().all(|c| c.is_ascii_digit()) && (i == 0 || g.len() == 3)
        })
    {
        return None;
    }
    pt_integer(&core.replace('.', ""))
}

fn pt_d(c: char) -> &'static str {
    const D: [&str; 10] = [
        "zero", "um", "dois", "três", "quatro", "cinco", "seis", "sete", "oito", "nove",
    ];
    D[c.to_digit(10).unwrap_or(0) as usize]
}

fn pt_integer(digits: &str) -> Option<String> {
    let n: u64 = digits.parse().ok()?;
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("zero".into());
    }
    let mut out = String::new();
    let m = n / 1_000_000;
    let r = n % 1_000_000;
    if m == 1 {
        out.push_str("ummilhão");
    } else if m > 1 {
        out.push_str(&pt_m(m));
        out.push_str("milhões");
    }
    if r > 0 {
        out.push_str(&pt_m(r));
    }
    Some(out)
}

fn pt_m(n: u64) -> String {
    let t = n / 1000;
    let rest = n % 1000;
    let mut out = String::new();
    if t > 1 {
        out.push_str(&pt_1000(t));
    }
    if t > 0 {
        out.push_str("mil");
    }
    if rest > 0 {
        out.push_str(&pt_1000(rest));
    }
    out
}

fn pt_1000(n: u64) -> String {
    let h = n / 100;
    let r = n % 100;
    if h == 1 && r == 0 {
        return "cem".into(); // exact 100
    }
    let mut out = String::new();
    if h == 1 {
        out.push_str("cento");
    } else if h > 1 {
        out.push_str(&pt_100(h));
        out.push_str("centos");
    }
    if r > 0 {
        out.push_str(&pt_100(r));
    }
    out
}

fn pt_100(n: u64) -> String {
    const U: [&str; 20] = [
        "",
        "um",
        "dois",
        "três",
        "quatro",
        "cinco",
        "seis",
        "sete",
        "oito",
        "nove",
        "dez",
        "onze",
        "doze",
        "treze",
        "quatorze",
        "quinze",
        "dezesseis",
        "dezessete",
        "dezoito",
        "dezenove",
    ];
    const T: [&str; 10] = [
        "",
        "",
        "vinte",
        "trinta",
        "quarenta",
        "cinquenta",
        "sessenta",
        "setenta",
        "oitenta",
        "noventa",
    ];
    if n < 20 {
        return U[n as usize].into();
    }
    if n == 100 {
        return "cem".into();
    }
    let t = (n / 10) as usize;
    let u = (n % 10) as usize;
    if u == 0 {
        return T[t].into();
    }
    if u == 1 && n != 21 && n != 31 {
        // "e um" for 31, 61, etc but "vinte e um"
        return format!("{} e um", T[t]);
    }
    format!("{} e {}", T[t], U[u])
}

// ──────────────────────────────────────────────────────────────────
// Italian
// ──────────────────────────────────────────────────────────────────

struct ItalianNormalizer;

impl LangNormalizer for ItalianNormalizer {
    fn lang(&self) -> &'static str {
        "it"
    }
    fn normalize_numeric(&self, core: &str) -> Option<String> {
        it_normalize(core)
    }
}

fn it_normalize(core: &str) -> Option<String> {
    // Italian uses comma decimals, dot thousands
    if let Some((int_part, frac)) = core.split_once(',') {
        let ok = !frac.is_empty()
            && frac.chars().all(|c| c.is_ascii_digit())
            && !int_part.is_empty()
            && int_part.chars().all(|c| c.is_ascii_digit() || c == '.');
        if !ok {
            return None;
        }
        let mut out = it_integer(&int_part.replace('.', ""))?;
        out.push_str("virgola");
        for c in frac.chars() {
            out.push_str(it_d(c));
        }
        return Some(out);
    }
    if core.is_empty() || core.chars().any(|c| !c.is_ascii_digit() && c != '.') {
        return None;
    }
    let groups: Vec<&str> = core.split('.').collect();
    if groups.len() > 1
        && !groups.iter().enumerate().all(|(i, g)| {
            !g.is_empty() && g.chars().all(|c| c.is_ascii_digit()) && (i == 0 || g.len() == 3)
        })
    {
        return None;
    }
    it_integer(&core.replace('.', ""))
}

fn it_d(c: char) -> &'static str {
    const D: [&str; 10] = [
        "zero", "uno", "due", "tre", "quattro", "cinque", "sei", "sette", "otto", "nove",
    ];
    D[c.to_digit(10).unwrap_or(0) as usize]
}

fn it_integer(digits: &str) -> Option<String> {
    let n: u64 = digits.parse().ok()?;
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("zero".into());
    }
    let mut out = String::new();
    let m = n / 1_000_000;
    let r = n % 1_000_000;
    if m == 1 {
        out.push_str("unmilione");
    } else if m > 1 {
        out.push_str(&it_m(m));
        out.push_str("milioni");
    }
    if r > 0 {
        out.push_str(&it_m(r));
    }
    Some(out)
}

fn it_m(n: u64) -> String {
    let t = n / 1000;
    let rest = n % 1000;
    let mut out = String::new();
    if t > 1 {
        out.push_str(&it_1000(t));
    }
    if t > 0 {
        out.push_str("mila");
    } // "mila" plural
    if rest > 0 {
        out.push_str(&it_1000(rest));
    }
    out
}

fn it_1000(n: u64) -> String {
    let h = n / 100;
    let r = n % 100;
    let mut out = String::new();
    if h == 1 {
        out.push_str("cento");
    } else if h > 1 {
        out.push_str(&it_100(h));
        // vowel elision: trecento not "trecento"
        if out.ends_with('e') && r > 0 {
            out.pop();
        }
        out.push_str("cento");
    }
    if r > 0 {
        out.push_str(&it_100(r));
    }
    out
}

// ──────────────────────────────────────────────────────────────────
// English
// ──────────────────────────────────────────────────────────────────

/// English digits: point decimals (1.5), comma thousands in some
/// locales but dot is standard; no vigesimal; straightforward
/// cardinals. Hyphenated compounds (forty-two).
struct EnglishNormalizer;

impl LangNormalizer for EnglishNormalizer {
    fn lang(&self) -> &'static str {
        "en"
    }
    fn normalize_numeric(&self, core: &str) -> Option<String> {
        en_normalize(core).or_else(|| en_split_digit_runs(core))
    }
}

fn en_normalize(core: &str) -> Option<String> {
    // decimal: dot (1.5 → "one point five"). "3.500" reads as a
    // decimal in en-US convention ("three point five zero zero") —
    // NOT European thousands (English strict thousands use commas);
    // pinned as such below.
    if let Some((int_part, frac)) = core.split_once('.') {
        let ok = !frac.is_empty()
            && frac.chars().all(|c| c.is_ascii_digit())
            && !int_part.is_empty()
            && int_part.chars().all(|c| c.is_ascii_digit() || c == ',')
            && en_groups_ok(int_part);
        if ok {
            let mut out = en_integer(&int_part.replace(',', ""))?;
            out.push_str(" point");
            for c in frac.chars() {
                out.push(' ');
                out.push_str(en_d(c));
            }
            return Some(out);
        }
        return None; // malformed separators — drop, not guess
    }
    // comma thousands (1,000) or plain integer
    let plain = core.replace(',', "");
    if plain.is_empty() || plain.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    // strict comma groups if commas present (matches the German
    // policy: first group 1..=3 digits, every later group exactly 3)
    if core.contains(',') && !en_groups_ok(core) {
        return None;
    }
    en_integer(&plain)
}

/// Comma-group shape for English numerals: every group all-digits,
/// first 1..=3 long, every later exactly 3 ("1,000,000" ✓,
/// "12,34" ✗, "1," ✗).
fn en_groups_ok(int_part: &str) -> bool {
    int_part.split(',').enumerate().all(|(i, g)| {
        !g.is_empty()
            && g.chars().all(|c| c.is_ascii_digit())
            && (if i == 0 {
                (1..=3).contains(&g.len())
            } else {
                g.len() == 3
            })
    })
}

/// Shared alpha/digit run-splitting for mixed-alphanumeric numerals
/// (`COVID-19` → covid + nineteen, `MP3` → mp + three). One
/// core for every language — this was a near-verbatim duplicate between
/// the German and English normalizers before the unification; the two
/// per-language behaviors are flags, pinned byte-for-byte by both
/// languages' existing tests:
/// - `require_alpha`: English rejects pure-digit cores (the strict
///   numeral shapes already failed `en_normalize`); German's chained
///   try-order never routes pure digits here
/// - `drop_ordinals`: English drops the digit-first/short-alpha-suffix
///   shape ("48th" — a cardinal + letter names would be wrong); German
///   has no such shape
fn split_runs_with(
    core: &str,
    require_alpha: bool,
    drop_ordinals: bool,
    cardinal: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if require_alpha && !core.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut runs: Vec<String> = Vec::new();
    let mut run = String::new();
    let mut run_is_digits = false;
    for ch in core.chars() {
        let is_digit = ch.is_ascii_digit();
        if !is_digit && !ch.is_ascii_alphabetic() {
            // inner separators (the hyphen in "COVID-19") end the run
            if !run.is_empty() {
                runs.push(std::mem::take(&mut run));
            }
            continue;
        }
        if !run.is_empty() && is_digit != run_is_digits {
            runs.push(std::mem::take(&mut run));
        }
        run_is_digits = is_digit;
        run.push(ch);
    }
    if !run.is_empty() {
        runs.push(run);
    }
    // ordinal shape: digit run first, short alphabetic suffix last
    // ("48th", "1920s") — a cardinal + letter names would be wrong
    if drop_ordinals
        && runs.len() >= 2
        && runs[0].chars().next().is_some_and(|c| c.is_ascii_digit())
        && runs
            .last()
            .is_some_and(|r| r.chars().all(|c| c.is_ascii_alphabetic()) && r.len() <= 2)
    {
        return None;
    }
    let mut joined: Vec<String> = Vec::with_capacity(runs.len());
    for part in runs {
        if part.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            joined.push(cardinal(&part)?);
        } else {
            joined.push(part.to_lowercase());
        }
    }
    (!joined.is_empty()).then(|| joined.join(" "))
}

/// Mixed alphanumerics split into alphabetic runs (words) + digit runs
/// (expanded): "COVID-19" → "covid nineteen", "A4" → "a four", "MP3"
/// → "mp three", "b2b" → "b zwei b". Mirrors the #82 loop reviews; the
/// alphabetic runs are words the dictionary/WFST answers (or
/// letter-spells — never a wrong number). Ordinal-shaped tokens
/// (digits first, short alpha suffix last: "48th", "1920s") DROP
/// rather than speak a wrong cardinal + letter names — proper
/// ordinals need their own rules.
fn en_split_digit_runs(core: &str) -> Option<String> {
    split_runs_with(core, true, true, en_integer)
}

fn en_d(c: char) -> &'static str {
    const D: [&str; 10] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ];
    D[c.to_digit(10).unwrap_or(0) as usize]
}

/// English cardinals are MULTI-WORD ("one hundred fifteen") — the
/// bundle lexicon answers individual words, never compounds, so the
/// parts are space-separated here (token_parts_with splits them into
/// separate dictionary lookups). Only the tens-units compound keeps
/// its standard hyphen ("forty-two"), which token_parts_with also
/// splits. US convention: no "and" after hundreds.
fn en_integer(digits: &str) -> Option<String> {
    let n: u64 = digits.parse().ok()?;
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("zero".into());
    }
    let mut out = String::new();
    let m = n / 1_000_000;
    let r = n % 1_000_000;
    if m > 0 {
        out.push_str(&en_m(m));
        out.push_str(" million");
    }
    if r > 0 {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&en_m(r));
    }
    Some(out)
}

/// 1..=999_999
fn en_m(n: u64) -> String {
    let t = n / 1000;
    let rest = n % 1000;
    let mut out = String::new();
    if t > 0 {
        out.push_str(&en_1000(t));
        out.push_str(" thousand");
    }
    if rest > 0 {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&en_1000(rest));
    }
    out
}

/// 1..=999
fn en_1000(n: u64) -> String {
    let h = n / 100;
    let r = n % 100;
    let mut out = String::new();
    if h > 0 {
        out.push_str(en_d(char::from_digit(h as u32, 10).unwrap()));
        out.push_str(" hundred");
    }
    if r > 0 {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&en_100(r));
    }
    out
}

fn en_100(n: u64) -> String {
    const U: [&str; 20] = [
        "",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    const T: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    if n < 20 {
        return U[n as usize].into();
    }
    let t = (n / 10) as usize;
    let u = (n % 10) as usize;
    if u == 0 {
        return T[t].into();
    }
    format!("{}-{}", T[t], U[u])
}

// ──────────────────────────────────────────────────────────────────
// Russian
// ──────────────────────────────────────────────────────────────────

/// Russian digits: comma decimals ("3,5" → "три запятая пять", the
/// Komma convention — post-comma digits read individually), no dot
/// separator (a dot is not a Russian numeral separator → drop, never
/// guess); space thousands cannot survive whitespace tokenization, so
/// plain integers carry it. Cardinals are SPACE-SEPARATED words
/// ("сорок два") — Russian orthography, and the lexicon answers
/// individual words. Feminine одна/две in the thousands position
/// ("двадцать одна тысяча"); thousand/million agreement follows the
/// 1/2-4/5+ paucal rules.
struct RussianNormalizer;

impl LangNormalizer for RussianNormalizer {
    fn lang(&self) -> &'static str {
        "ru"
    }
    fn normalize_numeric(&self, core: &str) -> Option<String> {
        ru_normalize(core)
    }
}

fn ru_normalize(core: &str) -> Option<String> {
    // decimal: comma ("3,5" → "три запятая пять")
    if let Some((int_part, frac)) = core.split_once(',') {
        let ok = !int_part.is_empty()
            && int_part.chars().all(|c| c.is_ascii_digit())
            && !frac.is_empty()
            && frac.chars().all(|c| c.is_ascii_digit());
        if !ok {
            return None; // malformed separators — drop, not guess
        }
        let mut out = ru_integer(int_part)?;
        out.push_str(" запятая");
        for c in frac.chars() {
            out.push(' ');
            out.push_str(ru_d(c));
        }
        return Some(out);
    }
    // a dot is not a Russian numeral separator (space thousands
    // cannot survive whitespace tokenization; dot decimals are not
    // standard) — drop rather than guess which was meant
    if core.contains('.') {
        return None;
    }
    if core.is_empty() || core.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    ru_integer(core)
}

fn ru_d(c: char) -> &'static str {
    const D: [&str; 10] = [
        "ноль",
        "один",
        "два",
        "три",
        "четыре",
        "пять",
        "шесть",
        "семь",
        "восемь",
        "девять",
    ];
    D[c.to_digit(10).unwrap_or(0) as usize]
}

/// Russian plural agreement: 1/2-4/5+ (paucal), with the 11-14
/// exception. 0 → form 3 ("пятью и более").
fn ru_plural(n: u64) -> usize {
    if n % 100 / 10 == 1 {
        return 2;
    }
    match n % 10 {
        1 => 0,
        2..=4 => 1,
        _ => 2,
    }
}

/// 0..=999_999_999 as space-separated Russian words. `None` out of
/// range or non-digit input.
const RU_U: [&str; 10] = [
    "ноль",
    "один",
    "два",
    "три",
    "четыре",
    "пять",
    "шесть",
    "семь",
    "восемь",
    "девять",
];
/// feminine units (the thousands position: одна/две)
const RU_UF: [&str; 10] = [
    "ноль",
    "одна",
    "две",
    "три",
    "четыре",
    "пять",
    "шесть",
    "семь",
    "восемь",
    "девять",
];
const RU_TEEN: [&str; 10] = [
    "десять",
    "одиннадцать",
    "двенадцать",
    "тринадцать",
    "четырнадцать",
    "пятнадцать",
    "шестнадцать",
    "семнадцать",
    "восемнадцать",
    "девятнадцать",
];
const RU_TENS: [&str; 10] = [
    "",
    "",
    "двадцать",
    "тридцать",
    "сорок",
    "пятьдесят",
    "шестьдесят",
    "семьдесят",
    "восемьдесят",
    "девяносто",
];
const RU_HUND: [&str; 10] = [
    "",
    "сто",
    "двести",
    "триста",
    "четыреста",
    "пятьсот",
    "шестьсот",
    "семьсот",
    "восемьсот",
    "девятьсот",
];
const RU_THOUSAND: [&str; 3] = ["тысяча", "тысячи", "тысяч"];
const RU_MILLION: [&str; 3] = ["миллион", "миллиона", "миллионов"];

/// 1..=999 as words; `fem` switches the final unit 1/2 to одна/две
/// (the thousands position takes feminine agreement)
fn ru_group(n: u64, fem: bool) -> String {
    let mut words: Vec<&str> = Vec::new();
    let h = (n / 100) as usize;
    if h > 0 {
        words.push(RU_HUND[h]);
    }
    let r = n % 100;
    match r {
        0 => {}
        1..=9 => words.push(if fem {
            RU_UF[r as usize]
        } else {
            RU_U[r as usize]
        }),
        10..=19 => words.push(RU_TEEN[(r - 10) as usize]),
        _ => {
            words.push(RU_TENS[(r / 10) as usize]);
            let uidx = (r % 10) as usize;
            if uidx > 0 {
                words.push(if fem { RU_UF[uidx] } else { RU_U[uidx] });
            }
        }
    }
    words.join(" ")
}

/// 0..=999_999_999 as space-separated Russian words. `None` out of
/// range or non-digit input.
fn ru_integer(digits: &str) -> Option<String> {
    let n: u64 = digits.parse().ok()?;
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some(RU_U[0].into());
    }
    let mut words: Vec<String> = Vec::new();
    let millions = n / 1_000_000;
    let rest = n % 1_000_000;
    if millions > 0 {
        words.push(ru_group(millions, false));
        words.push(RU_MILLION[ru_plural(millions)].to_string());
    }
    let thousands = rest / 1000;
    let tail = rest % 1000;
    if thousands > 0 {
        words.push(ru_group(thousands, true));
        words.push(RU_THOUSAND[ru_plural(thousands)].to_string());
    }
    if tail > 0 {
        words.push(ru_group(tail, false));
    }
    Some(words.join(" "))
}

fn it_100(n: u64) -> String {
    const U: [&str; 20] = [
        "",
        "uno",
        "due",
        "tre",
        "quattro",
        "cinque",
        "sei",
        "sette",
        "otto",
        "nove",
        "dieci",
        "undici",
        "dodici",
        "tredici",
        "quattordici",
        "quindici",
        "sedici",
        "diciassette",
        "diciotto",
        "diciannove",
    ];
    const T: [&str; 10] = [
        "",
        "",
        "venti",
        "trenta",
        "quaranta",
        "cinquanta",
        "sessanta",
        "settanta",
        "ottanta",
        "novanta",
    ];
    if n < 20 {
        return U[n as usize].into();
    }
    let t = (n / 10) as usize;
    let u = (n % 10) as usize;
    if u == 0 {
        return T[t].into();
    }
    // vowel elision: ventuno not "ventiuno", ventotto not "ventiotto"
    let tens = T[t];
    if (u == 1 || u == 8) && tens.ends_with('i') {
        return format!("{}{}", &tens[..tens.len() - 1], U[u]);
    }
    format!("{}{}", T[t], U[u])
}

fn primary_subtag(lang_code: &str) -> String {
    lang_code
        .trim()
        .split(['-', '_'])
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_ascii_lowercase()
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
        let chain = floravox_g2p_fst::CachedPhonemizer::new(
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
            // the bundle key is the language tag the dictionary
            // answers in (deu/de, fra/fr, …): the normalizer follows
            // the bundle, not the model registry spelling
            normalizer: normalizer_for(key),
        })
    }

    /// Phonemize an utterance into per-sentence codepoint streams.
    /// Tokens with digits are first expanded to words in the bundle's
    /// language (`LangNormalizer`); each remaining token is looked up
    /// in the
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
                // token whose word core CONTAINS a digit must expand to
                // German or DROP — never reach the fallback chain,
                // which spells alphanumeric junk in ENGLISH ("1e9" →
                // "wʌniːnaɪn", "MP3" → "ɛmpiːθriː"; the German WFST
                // cannot process digit characters at all). Covered, in
                // order: plain/strict-thousands/decimal numerals
                // (digits_to_german), digits+alphabetic-suffix forms
                // ("1920er" → the German number + "er", one word for
                // the dictionary/WFST), and mixed alphanumerics split
                // into alphabetic runs (words) + digit runs (expanded):
                // "COVID-19" → "covid neunzehn", "A4" → "a vier",
                // "b2b" → "b zwei b". Leading zeros collapse ("007" →
                // "sieben") — accepted for quantities, noted for codes.
                let Some(parts_owned) = token_parts_with(self.normalizer.as_ref(), token) else {
                    // unhandled numeric (">999_999_999", malformed
                    // separators): drop, counted
                    dropped += 1;
                    words += 1;
                    continue;
                };
                let parts: Vec<&str> = parts_owned.iter().map(String::as_str).collect();
                words += 1;
                let mut mapped: Vec<char> = Vec::new();
                let mut ok = true;
                for (i, part) in parts.iter().enumerate() {
                    // runs within one original token are separate
                    // words: a space token between them, same as
                    // between original tokens
                    if i > 0 && !mapped.is_empty() {
                        mapped.push(' ');
                    }
                    // NFC first (decomposed input composes to the key
                    // form; dictionary keys are lowercase, the FST
                    // lexicon lowercases its lookup)
                    let nfc: String = part.nfc().collect();
                    let phones = self.chain.phonemize_token(&nfc);
                    for sym in &phones {
                        let Some(tokens) = self.mapper.map_symbol(sym) else {
                            ok = false;
                            break;
                        };
                        for p in tokens {
                            // single codepoints only — the whole stream
                            // is address-by-codepoint
                            let mut chars = p.chars();
                            if let (Some(c), None) = (chars.next(), chars.next()) {
                                mapped.push(c);
                            } else {
                                ok = false;
                            }
                        }
                        if !ok {
                            break;
                        }
                    }
                    if !ok {
                        break;
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

/// The words the dictionary/WFST will actually be asked for, after
/// the digit normalization: digit-bearing cores expand (`42.` →
/// `["zweiundvierzig."]`, `COVID-19` → `["covid", "neunzehn"]` with
/// punctuation reattached), everything else passes through as the
/// token itself. `None` = an unhandled numeric the caller drops.
/// Public so the bake-off attributes dictionary-vs-fallback tiers on
/// the SAME lookup words the runtime uses (Mac-team review: looking
/// up the raw `42.` miscounted a normalized dictionary hit as
/// fallback).
#[must_use]
pub fn token_parts_with(norm: &dyn LangNormalizer, token: &str) -> Option<Vec<String>> {
    let core = floravox_g2p_fst::word_core(token).unwrap_or_default();
    if core.chars().any(|c| c.is_ascii_digit()) {
        let expanded = norm.normalize_numeric(core)?;
        Some(
            reattach_punct(token, &expanded)
                .split_whitespace()
                .flat_map(split_hyphenated)
                .collect(),
        )
    } else {
        Some(split_hyphenated(token))
    }
}

/// Split a token on interior hyphen runs. Audited: neither bundle
/// lexicon carries a hyphenated entry (eng-US 0/133,637, deu
/// 0/333,945 — Wiktionary headwords are single words), so
/// `forty-two` or `Kfz-Kennzeichen` as ONE lookup key misses the
/// dictionary and the WFST fallback letter-spells the compound
/// (`f-o-r-t-y t-w-o`). The parts are separate dictionary words; the
/// caller inserts the inter-word space token between parts, exactly
/// as between original tokens. Punctuation-only tokens (`--`) pass
/// through unchanged.
fn split_hyphenated(token: &str) -> Vec<String> {
    // guard must cover every split character — an ASCII-only `contains('-')`
    // would let U+2010/2011/en/em dashes pass through unsplit
    if !token.contains(['-', '‐', '‑', '–', '—']) {
        return vec![token.to_string()];
    }
    let parts: Vec<String> = token
        .split(['-', '‐', '‑', '–', '—'])
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    if parts.is_empty() {
        vec![token.to_string()]
    } else {
        parts
    }
}

/// [`token_parts_with`] with the GERMAN normalizer — the bake-off and
/// existing call sites are German-only; kept so tooling does not need
/// to know the language.
#[must_use]
pub fn token_parts(token: &str) -> Option<Vec<String>> {
    token_parts_with(&GermanNormalizer, token)
}

/// Text-level digit normalization for front-ends that read raw
/// characters (MMS): expand digit-bearing tokens to words in the
/// model's language ("42" → "forty two"), because MMS token files
/// carry `0`..`9` — raw digits articulate ONE BY ONE ("four two"),
/// and the models were trained on Bible audio, never on digit words.
/// Unhandled numerics (malformed separators, out-of-range, no rules
/// for the locale yet) drop with a count — never wrong-language
/// expansion. Non-digit tokens pass through (hyphen compounds split,
/// as everywhere). Returns the rewritten text and the dropped count.
#[must_use]
pub fn normalize_digits_in_text(text: &str, lang: &str) -> (String, usize) {
    let norm = normalizer_for(lang);
    let mut out = String::with_capacity(text.len());
    let mut dropped = 0usize;
    for token in text.split_whitespace() {
        match token_parts_with(norm.as_ref(), token) {
            Some(parts) => {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&parts.join(" "));
            }
            None => dropped += 1,
        }
    }
    (out, dropped)
}

/// The dictionary lookup key for one [`token_parts`] part: NFC-
/// normalized, edge punctuation stripped — exactly what the runtime
/// hands the lexicon (`phonemize` NFC-normalizes each part, then
/// floravox's `phonemize_token` applies `word_core`; the FST lexicon
/// lowercases its lookup itself). Public so the bake-off attributes
/// tiers on the same key the runtime looks up — `token_parts`
/// reattaches punctuation (`"42."` → `"zweiundvierzig."`), and
/// looking THAT up raw miscounts a dictionary hit as fallback
/// (Mac-team review #3).
#[must_use]
pub fn lookup_key(part: &str) -> Option<String> {
    let nfc: String = part.nfc().collect();
    floravox_g2p_fst::word_core(&nfc).map(str::to_string)
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

/// Mixed alphanumerics ("COVID-19", "A4", "b2b", "MP3") split into
/// runs: alphabetic runs become words (dictionary/WFST — German
/// phones), digit runs expand to German words. Returns the
/// space-joined replacement, or `None` when a digit run is not a
/// covered numeral (then the caller drops the token).
///
/// The German WFST cannot process digit characters (not in its input
/// alphabet) — without this split such tokens fall through to the
/// English letter-name speller (live-verified: "MP3" → "ɛmpiːθriː").
fn split_digit_runs(core: &str) -> Option<String> {
    // German: no alpha gate, no ordinal shape (see split_runs_with)
    split_runs_with(core, false, false, digits_to_german)
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
// Batch-1 locales (#92 campaign): ca cs da nl pl sk sl sv
//
// Validated per the fr/es/pt/it/ru playbook: probe 0-drop, whisper
// round-trip, number-vocabulary audit against the v0.4.0 bundles.
// Conventions:
// - ca/pl: comma decimal, strict dot thousands (ca/pl style)
// - cs/sk: comma decimal ("celá"/"celá"), strict space thousands are
//   unreachable (whitespace tokenizes) — plain digits only
// - da/nl/sv: comma decimal ("komma"), dot rejected outright
// - sl: comma decimal ("cela"); one-and-twenty CLOSED compounds are
//   dictionary hits (validated: dvainštirideset)
// - nl: one-and-twenty closed compounds with the standard ligatures
//   (tweeënveertig) — dictionary hits (validated)
// - da/sv: compounds SPLIT (to og fyrre / fyrtio två) — the closed
//   forms miss the dictionaries and the swe WFST cannot spell å
// - ca 31-99 emit SPACED tens (quaranta dos, not the normative
//   quaranta-dos): pronunciation-neutral, ear-validated, and both
//   forms split identically at the token layer
// - Icelandic deliberately NOT here: the isl dictionary lacks the
//   compound tens (þrjátíu…) and its WFST drops them — needs bundle
//   work first (issue #92 tracking)
// ──────────────────────────────────────────────────────────────────

/// Build tens+units ("dvacet jedna") from per-language tables.
fn tens_units(n: u64, units: &[&str], teens: &[&str], decades: &[&str]) -> String {
    match n {
        0..=9 => units[n as usize].to_string(),
        10..=19 => teens[(n - 10) as usize].to_string(),
        20..=99 => {
            let t = decades[((n / 10) - 2) as usize];
            let u = (n % 10) as usize;
            if u == 0 {
                t.to_string()
            } else {
                format!("{t} {}", units[u])
            }
        }
        _ => unreachable!("tens_units takes 0..=99"),
    }
}

/// under1000 from a hundreds table (index 0 empty or unused).
fn with_hundreds(n: u64, hundreds: &[&str], tail: &dyn Fn(u64) -> String) -> String {
    let h = (n / 100) as usize;
    let r = n % 100;
    let mut out = String::new();
    if h > 0 {
        out.push_str(hundreds[h]);
    }
    if r > 0 {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&tail(r));
    }
    out
}

fn ca_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "zero", "un", "dos", "tres", "quatre", "cinc", "sis", "set", "vuit", "nou",
    ];
    const T: [&str; 10] = [
        "deu", "onze", "dotze", "tretze", "catorze", "quinze", "setze", "disset", "divuit", "dinou",
    ];
    const X: [&str; 8] = [
        "vint",
        "trenta",
        "quaranta",
        "cinquanta",
        "seixanta",
        "setanta",
        "vuitanta",
        "noranta",
    ];
    const H: [&str; 10] = [
        "",
        "cent",
        "dos-cents",
        "tres-cents",
        "quatre-cents",
        "cinc-cents",
        "sis-cents",
        "set-cents",
        "vuit-cents",
        "nou-cents",
    ];
    if n > 999_999_999 || n == 0 {
        return if n == 0 { Some("zero".into()) } else { None };
    }
    let tail = |x: u64| -> String {
        if (21..=29).contains(&x) {
            format!("vint-i-{}", U[(x % 20) as usize])
        } else {
            tens_units(x, &U, &T, &X)
        }
    };
    let under1000 = |x: u64| with_hundreds(x, &H, &tail);
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut words: Vec<String> = Vec::new();
    if mil == 1 {
        words.push("un milió".into());
    } else if mil > 1 {
        words.push(under1000(mil));
        words.push("milions".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            words.push("mil".into());
        } else {
            words.push(under1000(t));
            words.push("mil".into());
        }
    }
    let tail_n = rest % 1_000;
    if tail_n > 0 {
        words.push(under1000(tail_n));
    }
    Some(words.join(" "))
}

/// cs/sk share the shape: paucal thousands/millions, spaced compounds.
fn cs_sk_integer(
    n: u64,
    u: &[&str],
    teens: &[&str],
    decades: &[&str],
    hundreds: &[&str],
    tys: (&str, &str, &str),
    mil: (&str, &str, &str),
) -> Option<String> {
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some(u[0].to_string());
    }
    let tail = |x: u64| tens_units(x, u, teens, decades);
    let under1000 = |x: u64| with_hundreds(x, hundreds, &tail);
    let paucal = |count: u64, forms: (&str, &str, &str)| -> String {
        let c10 = count % 10;
        let c100 = count % 100;
        if count == 1 {
            forms.0.into()
        } else if (2..=4).contains(&c10) && !(12..=14).contains(&c100) {
            forms.1.into()
        } else {
            forms.2.into()
        }
    };
    let m = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut words: Vec<String> = Vec::new();
    if m == 1 {
        words.push(mil.0.into());
    } else if m > 1 {
        words.push(under1000(m));
        words.push(paucal(m, mil));
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            words.push(tys.0.into());
        } else {
            words.push(under1000(t));
            words.push(paucal(t, tys));
        }
    }
    let tail_n = rest % 1_000;
    if tail_n > 0 {
        words.push(under1000(tail_n));
    }
    Some(words.join(" "))
}

// NOTE: cs standalone 21 is "dvacet jeden" (masculine) rather than
// the more idiomatic "dvacet jedna" — self-consistent with the
// compound agreement (dvacet jeden tisíc) and dictionary-clean
fn cs_integer(n: u64) -> Option<String> {
    cs_sk_integer(
        n,
        &[
            "nula", "jeden", "dva", "tři", "čtyři", "pět", "šest", "sedm", "osm", "devět",
        ],
        &[
            "deset",
            "jedenáct",
            "dvanáct",
            "třináct",
            "čtrnáct",
            "patnáct",
            "šestnáct",
            "sedmnáct",
            "osmnáct",
            "devatenáct",
        ],
        &[
            "dvacet",
            "třicet",
            "čtyřicet",
            "padesát",
            "šedesát",
            "sedmdesát",
            "osmdesát",
            "devadesát",
        ],
        &[
            "",
            "sto",
            "dvě stě",
            "tři sta",
            "čtyři sta",
            "pět set",
            "šest set",
            "sedm set",
            "osm set",
            "devět set",
        ],
        ("tisíc", "tisíce", "tisíc"),
        ("milion", "miliony", "milionů"),
    )
}

fn sk_integer(n: u64) -> Option<String> {
    cs_sk_integer(
        n,
        &[
            "nula", "jeden", "dva", "tri", "štyri", "päť", "šesť", "sedem", "osem", "deväť",
        ],
        &[
            "desať",
            "jedenásť",
            "dvanásť",
            "trinásť",
            "štrnásť",
            "pätnásť",
            "šestnásť",
            "sedemnásť",
            "osemnásť",
            "devätnásť",
        ],
        &[
            "dvadsať",
            "tridsať",
            "štyridsať",
            "päťdesiat",
            "šesťdesiat",
            "sedemdesiat",
            "osemdesiat",
            "deväťdesiat",
        ],
        &[
            "",
            "sto",
            "dvesto",
            "tristo",
            "štyristo",
            "päťsto",
            "šesťsto",
            "sedemsto",
            "osemsto",
            "deväťsto",
        ],
        ("tisíc", "tisíce", "tisíc"),
        ("milión", "milióny", "miliónov"),
    )
}

fn da_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "nul", "en", "to", "tre", "fire", "fem", "seks", "syv", "otte", "ni",
    ];
    const T: [&str; 10] = [
        "ti", "elleve", "tolv", "tretten", "fjorten", "femten", "seksten", "sytten", "atten",
        "nitten",
    ];
    const X: [&str; 8] = [
        "tyve",
        "tredive",
        "fyrre",
        "halvtreds",
        "tres",
        "halvfjerds",
        "firs",
        "halvfems",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("nul".into());
    }
    // one-and-twenty, SPLIT ("to og fyrre"): the dan dictionary lacks
    // the closed forms; the split IS the spoken order
    let tail = |x: u64| -> String {
        if x <= 19 {
            return tens_units(x, &U, &T, &["", ""]);
        }
        let t = X[((x / 10) - 2) as usize];
        let unit = (x % 10) as usize;
        if unit == 0 {
            t.to_string()
        } else {
            format!("{} og {t}", U[unit])
        }
    };
    let under1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut out = String::new();
        if h == 1 {
            out.push_str("hundrede");
        } else if h > 1 {
            out.push_str(&tail(h));
            out.push_str(" hundrede");
        }
        if r > 0 {
            if !out.is_empty() {
                // "og" between the hundreds word and the tail
                // (to hundrede og halvtreds) — Danish convention
                out.push_str(" og ");
            }
            out.push_str(&tail(r));
        }
        out
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut words: Vec<String> = Vec::new();
    if mil == 1 {
        words.push("en million".into());
    } else if mil > 1 {
        words.push(under1000(mil));
        words.push("millioner".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            words.push("tusinde".into());
        } else {
            words.push(under1000(t));
            words.push("tusinde".into());
        }
    }
    let tail_n = rest % 1_000;
    if tail_n > 0 {
        // a sub-100 remainder after a scale word takes "og" (Danish:
        // to tusinde og fem)
        if tail_n < 100 && !words.is_empty() {
            words.push(format!("og {}", under1000(tail_n)));
        } else {
            words.push(under1000(tail_n));
        }
    }
    Some(words.join(" "))
}

fn nl_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "nul", "een", "twee", "drie", "vier", "vijf", "zes", "zeven", "acht", "negen",
    ];
    const T: [&str; 10] = [
        "tien",
        "elf",
        "twaalf",
        "dertien",
        "veertien",
        "vijftien",
        "zestien",
        "zeventien",
        "achttien",
        "negentien",
    ];
    const X: [&str; 8] = [
        "twintig", "dertig", "veertig", "vijftig", "zestig", "zeventig", "tachtig", "negentig",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("nul".into());
    }
    // one-and-twenty CLOSED compounds with the standard ligatures —
    // dictionary hits (validated: "tweeënveertig")
    let tail = |x: u64| -> String {
        if x <= 19 {
            return tens_units(x, &U, &T, &["", ""]);
        }
        let t = X[((x / 10) - 2) as usize];
        let unit = (x % 10) as usize;
        if unit == 0 {
            return t.to_string();
        }
        match unit {
            1 => format!("eenen{t}"),
            2 => format!("tweeën{t}"),
            3 => format!("drieën{t}"),
            4 => format!("vieren{t}"),
            5 => format!("vijfen{t}"),
            6 => format!("zesen{t}"),
            7 => format!("zevenen{t}"),
            8 => format!("achten{t}"),
            _ => format!("negenen{t}"),
        }
    };
    let under1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut out = String::new();
        if h == 1 {
            out.push_str("honderd");
        } else if h > 1 {
            out.push_str(U[h as usize]);
            out.push_str("honderd");
        }
        if r > 0 {
            if !out.is_empty() && h > 1 {
                out.push(' ');
            }
            out.push_str(&tail(r));
        }
        out
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut words: Vec<String> = Vec::new();
    if mil == 1 {
        words.push("een miljoen".into());
    } else if mil > 1 {
        words.push(under1000(mil));
        words.push("miljoen".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            words.push("duizend".into());
        } else {
            words.push(under1000(t));
            words.push("duizend".into());
        }
    }
    let tail_n = rest % 1_000;
    if tail_n > 0 {
        words.push(under1000(tail_n));
    }
    Some(words.join(" "))
}

fn pl_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "zero",
        "jeden",
        "dwa",
        "trzy",
        "cztery",
        "pięć",
        "sześć",
        "siedem",
        "osiem",
        "dziewięć",
    ];
    const T: [&str; 10] = [
        "dziesięć",
        "jedenaście",
        "dwanaście",
        "trzynaście",
        "czternaście",
        "piętnaście",
        "szesnaście",
        "siedemnaście",
        "osiemnaście",
        "dziewiętnaście",
    ];
    const X: [&str; 8] = [
        "dwadzieścia",
        "trzydzieści",
        "czterdzieści",
        "pięćdziesiąt",
        "sześćdziesiąt",
        "siedemdziesiąt",
        "osiemdziesiąt",
        "dziewięćdziesiąt",
    ];
    const H: [&str; 10] = [
        "",
        "sto",
        "dwieście",
        "trzysta",
        "czterysta",
        "pięćset",
        "sześćset",
        "siedemset",
        "osiemset",
        "dziewięćset",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("zero".into());
    }
    let tail = |x: u64| tens_units(x, &U, &T, &X);
    let under1000 = |x: u64| with_hundreds(x, &H, &tail);
    let paucal = |count: u64, one: &str, few: &str, many: &str| -> String {
        let c10 = count % 10;
        let c100 = count % 100;
        if count == 1 {
            one.into()
        } else if (2..=4).contains(&c10) && !(12..=14).contains(&c100) {
            few.into()
        } else {
            many.into()
        }
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut words: Vec<String> = Vec::new();
    if mil == 1 {
        words.push("milion".into());
    } else if mil > 1 {
        words.push(under1000(mil));
        words.push(paucal(mil, "milion", "miliony", "milionów"));
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            words.push("tysiąc".into());
        } else {
            words.push(under1000(t));
            words.push(paucal(t, "tysiąc", "tysiące", "tysięcy"));
        }
    }
    let tail_n = rest % 1_000;
    if tail_n > 0 {
        words.push(under1000(tail_n));
    }
    Some(words.join(" "))
}

fn sv_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "noll", "ett", "två", "tre", "fyra", "fem", "sex", "sju", "åtta", "nio",
    ];
    const T: [&str; 10] = [
        "tio", "elva", "tolv", "tretton", "fjorton", "femton", "sexton", "sjutton", "arton",
        "nitton",
    ];
    const X: [&str; 8] = [
        "tjugo", "trettio", "fyrtio", "femtio", "sextio", "sjuttio", "åttio", "nittio",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("noll".into());
    }
    // closed compounds (trettioett) MISS the swe dictionary and the
    // WFST cannot spell å — SPLIT (fyrtio två): intelligible + clean
    let tail = |x: u64| tens_units(x, &U, &T, &X);
    let under1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut out = String::new();
        if h == 1 {
            out.push_str("hundra");
        } else if h > 1 {
            out.push_str(U[h as usize]);
            out.push_str("hundra");
        }
        if r > 0 {
            if !out.is_empty() && h > 1 {
                out.push(' ');
            }
            out.push_str(&tail(r));
        }
        out
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut words: Vec<String> = Vec::new();
    if mil == 1 {
        words.push("en miljon".into());
    } else if mil > 1 {
        words.push(under1000(mil));
        words.push("miljoner".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            words.push("ettusen".into());
        } else {
            words.push(under1000(t));
            words.push("tusen".into());
        }
    }
    let tail_n = rest % 1_000;
    if tail_n > 0 {
        words.push(under1000(tail_n));
    }
    Some(words.join(" "))
}

fn slv_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "nič", "ena", "dva", "tri", "štiri", "pet", "šest", "sedem", "osem", "devet",
    ];
    const T: [&str; 10] = [
        "deset",
        "enajst",
        "dvanajst",
        "trinajst",
        "štirinajst",
        "petnajst",
        "šestnajst",
        "sedemnajst",
        "osemnajst",
        "devetnajst",
    ];
    const X: [&str; 8] = [
        "dvajset",
        "trideset",
        "štirideset",
        "petdeset",
        "šestdeset",
        "sedemdeset",
        "osemdeset",
        "devetdeset",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("nič".into());
    }
    // one-and-twenty CLOSED (dvaindvajset) — dictionary hits
    // (validated: dvainštirideset); 2 stays "dva" in compounds
    let tail = |x: u64| -> String {
        if x <= 19 {
            return tens_units(x, &U, &T, &["", ""]);
        }
        let t = X[((x / 10) - 2) as usize];
        let unit = (x % 10) as usize;
        if unit == 0 {
            return t.to_string();
        }
        // elision before "in": only štiri drops its final i
        // (štirindvajset); ena/dva keep the a (enain-, dvain-)
        let stem = match unit {
            4 => "štir",
            _ => U[unit],
        };
        format!("{stem}in{t}")
    };
    let under1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut out = String::new();
        if h == 1 {
            out.push_str("sto");
        } else if h == 2 {
            out.push_str("dvesto");
        } else if h > 2 {
            out.push_str(U[h as usize]);
            out.push_str("sto");
        }
        if r > 0 {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&tail(r));
        }
        out
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut words: Vec<String> = Vec::new();
    if mil == 1 {
        words.push("milijon".into());
    } else if mil == 2 {
        words.push("dva milijona".into());
    } else if mil > 2 {
        words.push(under1000(mil));
        // 3-4: nominative plural (tri milijoni); 5+ genitive plural
        // (pet milijonov) — loop-1 review caught the collapsed branch
        let c10 = (mil % 10) as u32;
        let c100 = (mil % 100) as u32;
        if (3..=4).contains(&c10) && !(13..=14).contains(&c100) {
            words.push("milijoni".into());
        } else {
            words.push("milijonov".into());
        }
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            words.push("tisoč".into());
        } else if t == 2 {
            words.push("dva tisoč".into());
        } else {
            words.push(under1000(t));
            words.push("tisoč".into());
        }
    }
    let tail_n = rest % 1_000;
    if tail_n > 0 {
        words.push(under1000(tail_n));
    }
    Some(words.join(" "))
}

/// Decimal shape shared by the batch: comma decimals with per-locale
/// digit words; the `cfg!`-free tables keep every normalizer local.
struct Batch1Spec {
    integer: fn(u64) -> Option<String>,
    decimal_word: &'static str,
    digits: [&'static str; 10],
    /// dot is strict thousands grouping (ca/pl style) or rejected
    dot_thousands: bool,
}

fn batch1_normalize(core: &str, spec: &Batch1Spec) -> Option<String> {
    if let Some((int_part, frac)) = core.split_once(',') {
        let ok = !int_part.is_empty()
            && int_part.chars().all(|c| c.is_ascii_digit())
            && !frac.is_empty()
            && frac.chars().all(|c| c.is_ascii_digit());
        if !ok {
            return None;
        }
        let mut out = (spec.integer)(int_part.parse::<u64>().ok()?)?;
        out.push(' ');
        out.push_str(spec.decimal_word);
        for c in frac.chars() {
            out.push(' ');
            out.push_str(spec.digits[c.to_digit(10)? as usize]);
        }
        return Some(out);
    }
    if core.contains('.') {
        if !spec.dot_thousands {
            return None;
        }
        for (i, g) in core.split('.').enumerate() {
            let ok = !g.is_empty()
                && g.chars().all(|c| c.is_ascii_digit())
                && (if i == 0 {
                    (1..=3).contains(&g.len())
                } else {
                    g.len() == 3
                });
            if !ok {
                return None;
            }
        }
        let plain: String = core.chars().filter(|c| *c != '.').collect();
        return (spec.integer)(plain.parse::<u64>().ok()?);
    }
    if core.is_empty() || core.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    (spec.integer)(core.parse::<u64>().ok()?)
}

macro_rules! batch1_normalizer {
    ($name:ident, $lang:expr, $spec:expr) => {
        struct $name;
        impl LangNormalizer for $name {
            fn lang(&self) -> &'static str {
                $lang
            }
            fn normalize_numeric(&self, core: &str) -> Option<String> {
                batch1_normalize(core, &$spec)
            }
        }
    };
}

batch1_normalizer!(
    CatalanNormalizer,
    "ca",
    Batch1Spec {
        integer: ca_integer,
        decimal_word: "coma",
        digits: ["zero", "un", "dos", "tres", "quatre", "cinc", "sis", "set", "vuit", "nou"],
        dot_thousands: true,
    }
);
batch1_normalizer!(
    CzechNormalizer,
    "cs",
    Batch1Spec {
        integer: cs_integer,
        decimal_word: "celá",
        digits: [
            "nula", "jeden", "dva", "tři", "čtyři", "pět", "šest", "sedm", "osm", "devět"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    DanishNormalizer,
    "da",
    Batch1Spec {
        integer: da_integer,
        decimal_word: "komma",
        digits: ["nul", "en", "to", "tre", "fire", "fem", "seks", "syv", "otte", "ni"],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    DutchNormalizer,
    "nl",
    Batch1Spec {
        integer: nl_integer,
        decimal_word: "komma",
        digits: ["nul", "een", "twee", "drie", "vier", "vijf", "zes", "zeven", "acht", "negen"],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    PolishNormalizer,
    "pl",
    Batch1Spec {
        integer: pl_integer,
        decimal_word: "przecinek",
        digits: [
            "zero",
            "jeden",
            "dwa",
            "trzy",
            "cztery",
            "pięć",
            "sześć",
            "siedem",
            "osiem",
            "dziewięć"
        ],
        dot_thousands: true,
    }
);
batch1_normalizer!(
    SlovakNormalizer,
    "sk",
    Batch1Spec {
        integer: sk_integer,
        decimal_word: "celá",
        digits: [
            "nula", "jeden", "dva", "tri", "štyri", "päť", "šesť", "sedem", "osem", "deväť"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    SlovenianNormalizer,
    "sl",
    Batch1Spec {
        integer: slv_integer,
        decimal_word: "cela",
        digits: ["nič", "ena", "dva", "tri", "štiri", "pet", "šest", "sedem", "osem", "devet"],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    SwedishNormalizer,
    "sv",
    Batch1Spec {
        integer: sv_integer,
        decimal_word: "komma",
        digits: ["noll", "ett", "två", "tre", "fyra", "fem", "sex", "sju", "åtta", "nio"],
        dot_thousands: false,
    }
);

// ──────────────────────────────────────────────────────────────────
// Batch-2 locales (#92 campaign wave 2, voicegarden v0.5.0–v0.6.0):
// af cy eu hu is ka kk ko lb lv ml ne no sq sr uk
//
// All probe-validated against the real voices (0 words dropped with
// spelled-out numbers; see the LANG_BUNDLES row comments). Word forms
// were additionally checked against the bundle lexicons — forms the
// dictionary carries resolve at the dictionary tier, the rest speak
// through the per-character fallback (e.g. Hungarian closed compounds
// beyond kétézer, Norwegian tjueto outside the nob bundle).
//
// hr excluded (coqui voice has multi-char <BOS> tokens — the
// codepoints path cannot address them); vi excluded (grapheme voice,
// confirmed by the voicegarden audit).
//
// Conventions follow the languages: closed agglutinative compounds
// (hu ml ne — 21-99 are single words there), conjunction-linked tens
// (is "og", sr "и", sq "e"), space-separated tens elsewhere; closed
// hundreds where the language writes them closed (hu ka sr uk);
// paucal agreement where the grammar requires it (sr uk, Russian
// style); ko groups by 만 (10,000), not thousands; ne has the
// लाख (100,000) tier.
// ──────────────────────────────────────────────────────────────────

fn af_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "nul", "een", "twee", "drie", "vier", "vyf", "ses", "sewe", "agt", "nege",
    ];
    const T: [&str; 10] = [
        "tien",
        "elf",
        "twaalf",
        "dertien",
        "veertien",
        "vyftien",
        "sestien",
        "sewentien",
        "agtien",
        "negentien",
    ];
    const X: [&str; 8] = [
        "twintig", "dertig", "veertig", "vyftig", "sestig", "sewentig", "tagtig", "negentig",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("nul".into());
    }
    // tens+units SPLIT with "en" ("twee en twintig" — spaced, not
    // the normative hyphenated "twee-en-twintig"; the bundle
    // dictionary carries no closed forms either way);
    // hundreds/thousands closed ("tweehonderd", "tweeduisend")
    let tail = |x: u64| -> String {
        if x <= 19 {
            tens_units(x, &U, &T, &["", ""])
        } else {
            let t = X[((x / 10) - 2) as usize];
            let u = (x % 10) as usize;
            if u == 0 {
                t.to_string()
            } else {
                format!("{} en {t}", U[u])
            }
        }
    };
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("honderd");
        } else if h > 1 {
            o.push_str(U[h as usize]);
            o.push_str("honderd");
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("een miljoen".into());
    } else if mil > 1 {
        w.push(u1000(mil));
        w.push("miljoen".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("duisend".into());
        } else {
            // duisend CLOSES onto a single-word multiplier
            // (tweeduisend, honderdduisend); compound tails keep the
            // space ("een en twintig duisend")
            let m = u1000(t);
            if m.contains(' ') {
                w.push(m);
                w.push("duisend".into());
            } else {
                w.push(format!("{m}duisend"));
            }
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        w.push(u1000(r));
    }
    Some(w.join(" "))
}

fn cy_integer(n: u64) -> Option<String> {
    // Welsh, modern decimal system (deg/dau ddeg, not the traditional
    // vigesimal ugain/ar hugain). Soft/aspirate mutations on cant
    // (dau gant, tri chant) and mil (dwy fil) follow the grammar;
    // the dictionary carries deg/cant/mil/fil/chant directly.
    const U: [&str; 10] = [
        "sero", "un", "dau", "tri", "pedwar", "pump", "chwech", "saith", "wyth", "naw",
    ];
    // feminine multipliers for mil (mil is feminine; pum before a
    // noun, like the hundreds table)
    const UF: [&str; 10] = [
        "sero", "un", "dwy", "tair", "pedair", "pum", "chwe", "saith", "wyth", "naw",
    ];
    const T: [&str; 10] = [
        "deg",
        "un deg un",
        "un deg dau",
        "un deg tri",
        "un deg pedwar",
        "un deg pump",
        "un deg chwech",
        "un deg saith",
        "un deg wyth",
        "un deg naw",
    ];
    // 20-90: multiplier + deg, 2 takes the soft mutation ddeg
    const X: [&str; 8] = [
        "dau ddeg",
        "tri deg",
        "pedwar deg",
        "pum deg",
        "chwe deg",
        "saith deg",
        "wyth deg",
        "naw deg",
    ];
    // hundreds: multiplier + mutated cant
    const H: [&str; 9] = [
        "dau gant",
        "tri chant",
        "pedwar cant",
        "pum cant",
        "chwe chant",
        "saith cant",
        "wyth cant",
        "naw cant",
        "",
    ];
    if n > 999_999 {
        return None;
    }
    if n == 0 {
        return Some("sero".into());
    }
    let tail = |x: u64| -> String {
        match x {
            0..=9 => U[x as usize].to_string(),
            10..=19 => T[(x - 10) as usize].to_string(),
            _ => {
                let t = X[((x / 10) - 2) as usize];
                let u = (x % 10) as usize;
                if u == 0 {
                    t.to_string()
                } else {
                    format!("{t} {}", U[u])
                }
            }
        }
    };
    let u100 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("cant");
        } else if h > 1 {
            o.push_str(H[(h - 2) as usize]);
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000;
    let rest = n % 1_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("mil".into());
    } else if mil > 1 {
        // mil is feminine: dwy fil (soft mutation), tair mil,
        // pedair mil …
        if mil == 2 {
            w.push("dwy fil".into());
        } else {
            let m = if mil <= 9 { UF[mil as usize] } else { "" };
            if m.is_empty() {
                w.push(u100(mil));
            } else {
                w.push(m.into());
            }
            w.push("mil".into());
        }
    }
    if rest > 0 {
        w.push(u100(rest));
    }
    Some(w.join(" "))
}

fn eu_integer(n: u64) -> Option<String> {
    // 20/40/60/80 full forms and their -ta- compound prefixes
    // (21 = hogei eta bat → written hogeita bat); 30/50/70/90
    // prefixes (30 = hogeita hamar, 31 = hogeita hamaika = 20+11)
    const B: [(&str, &str); 4] = [
        ("hogei", "hogeita"),
        ("berrogei", "berrogeita"),
        ("hirurogei", "hirurogeita"),
        ("laurogei", "laurogeita"),
    ];
    const L: [&str; 4] = ["hogeita", "berrogeita", "hirurogeita", "laurogeita"];
    // batua closed hundreds 200-900
    const H: [&str; 8] = [
        "berrehun",
        "hirurehun",
        "laurehun",
        "bostehun",
        "seirehun",
        "zazpiehun",
        "zortziehun",
        "bederatziehun",
    ];
    const U: [&str; 10] = [
        "zero",
        "bat",
        "bi",
        "hiru",
        "lau",
        "bost",
        "sei",
        "zazpi",
        "zortzi",
        "bederatzi",
    ];
    const T: [&str; 10] = [
        "hamar",
        "hamaika",
        "hamabi",
        "hamahiru",
        "hamalau",
        "hamabost",
        "hamasei",
        "hamazazpi",
        "hemezortzi",
        "hemeretzi",
    ];
    if n > 999_999 {
        return None;
    }
    if n == 0 {
        return Some("zero".into());
    }
    // Basque is vigesimal above 20: 42 = berrogeita bi ("40-and 2"),
    // 30 = hogeita hamar ("20-and 10") — the -ta- linker is part of
    // the written form
    let tail = |x: u64| -> String {
        match x {
            0..=9 => U[x as usize].to_string(),
            10..=19 => T[(x - 10) as usize].to_string(),
            _ => {
                let tens = (x / 10) * 10;
                let u = (x % 10) as usize;
                match tens {
                    20 | 40 | 60 | 80 => {
                        let (full, linked) = B[((tens / 10 - 2) / 2) as usize];
                        if u == 0 {
                            full.to_string()
                        } else {
                            format!("{linked} {}", U[u])
                        }
                    }
                    _ => format!("{} {}", L[((tens - 30) / 20) as usize], T[u]),
                }
            }
        }
    };
    let u100 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("ehun");
        } else if h > 1 {
            o.push_str(H[(h - 2) as usize]);
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000;
    let rest = n % 1_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("mila".into());
    } else if mil > 1 {
        w.push(u100(mil));
        w.push("mila".into());
    }
    if rest > 0 {
        w.push(u100(rest));
    }
    Some(w.join(" "))
}

fn hu_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "nulla", "egy", "kettő", "három", "négy", "öt", "hat", "hét", "nyolc", "kilenc",
    ];
    const T: [&str; 10] = [
        "tíz",
        "tizenegy",
        "tizenkettő",
        "tizenhárom",
        "tizennégy",
        "tizenöt",
        "tizenhat",
        "tizenhét",
        "tizennyolc",
        "tizenkilenc",
    ];
    const X: [&str; 8] = [
        "húsz",
        "harminc",
        "negyven",
        "ötven",
        "hatvan",
        "hetven",
        "nyolcvan",
        "kilencven",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("nulla".into());
    }
    // Agglutinative CLOSED compounds: huszonegy, negyvenkettő (full
    // kettő in tens — the dictionary carries huszonkettő,
    // negyvenkettő, harminckettő), kétszáz, kétezer, egymillió.
    let tail = |x: u64| -> String {
        match x {
            0..=9 => U[x as usize].to_string(),
            10..=19 => T[(x - 10) as usize].to_string(),
            20..=29 if !x.is_multiple_of(10) => format!("huszon{}", U[(x % 10) as usize]),
            _ => {
                let t = X[((x / 10) - 2) as usize];
                let u = (x % 10) as usize;
                if u == 0 {
                    t.to_string()
                } else {
                    format!("{t}{}", U[u])
                }
            }
        }
    };
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("száz");
        } else if h == 2 {
            o.push_str("kétszáz");
        } else if h > 2 {
            o.push_str(U[h as usize]);
            o.push_str("száz");
        }
        if r > 0 {
            o.push_str(&tail(r));
        }
        o
    };
    // attributive kettő→két when the compound multiplies a scale
    // word (huszonkétezer, százkétezer, negyvenkétmillió)
    let ket = |m: String| -> String {
        if m.ends_with("kettő") {
            let head = &m[..m.len() - "kettő".len()];
            format!("{head}két")
        } else {
            m
        }
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("egymillió".into());
    } else if mil == 2 {
        w.push("kétmillió".into());
    } else if mil > 2 {
        w.push(format!("{}millió", ket(u1000(mil))));
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("ezer".into());
        } else if t == 2 {
            w.push("kétezer".into());
        } else {
            w.push(format!("{}ezer", ket(u1000(t))));
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        w.push(u1000(r));
    }
    Some(w.join(" "))
}

// Icelandic tables: units, neuter/feminine/masculine 1-4 (gender
// agreement on scale words), teens, tens.
const IS_U: [&str; 10] = [
    "núll", "einn", "tveir", "þrír", "fjórir", "fimm", "sex", "sjö", "átta", "níu",
];
const IS_N: [&str; 5] = ["", "eitt", "tvö", "þrjú", "fjögur"]; // neuter 1-4
const IS_F: [&str; 5] = ["", "ein", "tvær", "þrjár", "fjórar"]; // feminine 1-4
                                                                // plain masculine citation — the final units group keeps tveir
const IS_M: [&str; 5] = ["", "einn", "tveir", "þrír", "fjórir"];
const IS_T: [&str; 10] = [
    "tíu",
    "ellefu",
    "tólf",
    "þrettán",
    "fjórtán",
    "fimmtán",
    "sextán",
    "sautján",
    "átján",
    "nítján",
];
const IS_X: [&str; 8] = [
    "tuttugu",
    "þrjátíu",
    "fjörutíu",
    "fimmtíu",
    "sextíu",
    "sjötíu",
    "áttatíu",
    "níutíu",
];

fn is_integer(n: u64) -> Option<String> {
    // Icelandic: compound tens, "og" between tens and units AND after
    // the hundreds word or a scale word before a sub-100 remainder
    // (hundrað og einn, tvö þúsund og fimm), gender agreement on the
    // scale words — neuter tvö/þrjú/fjögur for hundrað/hundruð and
    // þúsund, feminine tvær/þrjár/fjórar for milljón(ir), masculine
    // citation in the final units group. The expanded isl bundle
    // carries tvö, hundruð, þrjú, tvær, þrjár, fjögur, og directly.
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("núll".into());
    }
    // multiplier group with the FINAL unit in the requested gender
    // (compound multipliers included: 102,000 → hundrað og tvö þúsund)
    let mult = |x: u64, n_1_4: &[&str; 5]| -> String {
        let tens = x % 100;
        let higher = x - tens;
        let tail = |t: u64| -> String {
            match t {
                0 => String::new(),
                1..=4 => n_1_4[t as usize].to_string(),
                5..=9 => IS_U[t as usize].to_string(),
                10..=19 => IS_T[(t - 10) as usize].to_string(),
                _ => {
                    let u = (t % 10) as usize;
                    let base = IS_X[((t / 10) - 2) as usize];
                    if u == 0 {
                        base.to_string()
                    } else if u <= 4 {
                        format!("{base} og {}", n_1_4[u])
                    } else {
                        format!("{base} og {}", IS_U[u])
                    }
                }
            }
        };
        let t = tail(tens);
        if higher == 0 {
            return t;
        }
        let h = higher / 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("hundrað");
        } else if h == 2 {
            o.push_str("tvö hundruð");
        } else if h == 3 {
            o.push_str("þrjú hundruð");
        } else if h == 4 {
            o.push_str("fjögur hundruð");
        } else if h > 4 {
            o.push_str(IS_U[h as usize]);
            o.push_str(" hundruð");
        }
        if !t.is_empty() {
            o.push_str(" og ");
            o.push_str(&t);
        }
        o
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("ein milljón".into());
    } else if mil > 1 {
        w.push(mult(mil, &IS_F));
        w.push("milljónir".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("eitt þúsund".into());
        } else {
            w.push(mult(t, &IS_N));
            w.push("þúsund".into());
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        // a sub-100 remainder after a scale word takes "og"
        // (tvö þúsund og fimm, ein milljón og einn)
        if r < 100 && !w.is_empty() {
            w.push(format!("og {}", mult(r, &IS_M)));
        } else {
            w.push(mult(r, &IS_M));
        }
    }
    Some(w.join(" "))
}

fn ka_integer(n: u64) -> Option<String> {
    // Georgian: vigesimal below 100 (ოცი 20, ოცდა… 20+r, ორმოცი 40 =
    // 2×20 …), closed hundreds (ორასი, სამასი — dictionary carries
    // ოცდაორი, ორასი, ასი, ათასი), separate-word thousands above 1000.
    const U: [&str; 10] = [
        "ნული",
        "ერთი",
        "ორი",
        "სამი",
        "ოთხი",
        "ხუთი",
        "ექვსი",
        "შვიდი",
        "რვა",
        "ცხრა",
    ];
    const T: [&str; 10] = [
        "ათი",
        "თერთმეტი",
        "თორმეტი",
        "ცამეტი",
        "თოთხმეტი",
        "თხუთმეტი",
        "თექვსმეტი",
        "ჩვიდმეტი",
        "თვრამეტი",
        "ცხრამეტი",
    ];
    // bases at 20·k and their compound prefixes (Ორმოცდაორი = 2×20+2)
    const B: [(&str, &str); 4] = [
        ("ოცი", "ოცდა"),
        ("ორმოცი", "ორმოცდა"),
        ("სამოცი", "სამოცდა"),
        ("ოთხმოცი", "ოთხმოცდა"),
    ];
    // closed hundreds 200-900
    const H: [&str; 8] = [
        "ორასი",
        "სამასი",
        "ოთხასი",
        "ხუთასი",
        "ექვსასი",
        "შვიდასი",
        "რვაასი",
        "ცხრაასი",
    ];
    fn tail(x: u64) -> String {
        match x {
            0..=9 => U[x as usize].to_string(),
            10..=19 => T[(x - 10) as usize].to_string(),
            _ => {
                let k = (x / 20) as usize; // 1..=4
                let r = x % 20;
                let (full, prefix) = B[k - 1];
                if r == 0 {
                    full.to_string()
                } else {
                    format!("{prefix}{}", tail(r))
                }
            }
        }
    }
    if n > 999_999 {
        return None;
    }
    if n == 0 {
        return Some("ნული".into());
    }
    let u100 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("ასი");
        } else if h > 1 {
            o.push_str(H[(h - 2) as usize]);
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000;
    let rest = n % 1_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("ათასი".into());
    } else if mil > 1 {
        w.push(u100(mil));
        w.push("ათასი".into());
    }
    if rest > 0 {
        w.push(u100(rest));
    }
    Some(w.join(" "))
}

fn kk_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "нөл",
        "бір",
        "екі",
        "үш",
        "төрт",
        "бес",
        "алты",
        "жеті",
        "сегіз",
        "тоғыз",
    ];
    const T: [&str; 10] = [
        "он",
        "он бір",
        "он екі",
        "он үш",
        "он төрт",
        "он бес",
        "он алты",
        "он жеті",
        "он сегіз",
        "он тоғыз",
    ];
    const X: [&str; 8] = [
        "жиырма",
        "отыз",
        "қырық",
        "елу",
        "алпыс",
        "жетпіс",
        "сексен",
        "тоқсан",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("нөл".into());
    }
    // Kazakh: space-separated throughout (қырық екі, екі жүз, екі мың)
    let tail = |x: u64| tens_units(x, &U, &T, &X);
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("жүз");
        } else if h > 1 {
            o.push_str(&tail(h));
            o.push_str(" жүз");
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("бір миллион".into());
    } else if mil > 1 {
        w.push(u1000(mil));
        w.push("миллион".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("бір мың".into());
        } else {
            w.push(u1000(t));
            w.push("мың".into());
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        w.push(u1000(r));
    }
    Some(w.join(" "))
}

fn ko_integer(n: u64) -> Option<String> {
    // Sino-Korean cardinals (일이삼…); Korean groups by 만 (10,000),
    // not thousands — 99,999,999 is the full range of the 만/백만
    // ladder without the 억 (10^8) tier.
    const U: [&str; 10] = ["영", "일", "이", "삼", "사", "오", "육", "칠", "팔", "구"];
    const X: [&str; 9] = [
        "십", "이십", "삼십", "사십", "오십", "육십", "칠십", "팔십", "구십",
    ];
    if n > 99_999_999 {
        return None;
    }
    if n == 0 {
        return Some("영".into());
    }
    let tail = |x: u64| -> String {
        if x < 10 {
            return U[x as usize].to_string();
        }
        let t = X[((x / 10) - 1) as usize];
        let u = (x % 10) as usize;
        if u == 0 {
            t.to_string()
        } else {
            format!("{t}{}", U[u])
        }
    };
    let u100 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push('백');
        } else if h > 1 {
            o.push_str(U[h as usize]);
            o.push('백');
        }
        if r > 0 {
            o.push_str(&tail(r));
        }
        o
    };
    // thousands chunk (x ≤ 9999): 천 = 1,000; 구천 = 9,000 — the 만
    // multiplier and remainder both go through it
    let chunk4 = |x: u64| -> String {
        let t = x / 1_000;
        let r = x % 1_000;
        let mut o = String::new();
        if t == 1 {
            o.push('천');
        } else if t > 1 {
            o.push_str(U[t as usize]);
            o.push('천');
        }
        if r > 0 {
            o.push_str(&u100(r));
        }
        o
    };
    let u10000 = |x: u64| -> String {
        // Korean writes number compounds solid: 만이천, 이만천,
        // 구천구백구십구만구천구백구십구
        let m = x / 10_000;
        let r = x % 10_000;
        let mut o = String::new();
        if m == 1 {
            o.push('만');
        } else if m > 1 {
            o.push_str(&chunk4(m));
            o.push('만');
        }
        if r > 0 {
            o.push_str(&chunk4(r));
        }
        o
    };
    Some(u10000(n))
}

fn lb_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "null", "eent", "zwéi", "dräi", "véier", "fënnef", "sechs", "siwen", "aacht", "nénge",
    ];
    const T: [&str; 10] = [
        "zéng",
        "eelef",
        "zwielef",
        "dräizéng",
        "véierzéng",
        "fofzéng",
        "siechzéng",
        "siwwenzéng",
        "uechtzéng",
        "nonnzéng",
    ];
    const X: [&str; 8] = [
        "zwanzeg",
        "drësseg",
        "véierzeg",
        "fofzeg",
        "sechzeg",
        "siwwenzeg",
        "achtzeg",
        "nonnzeg",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("null".into());
    }
    // one-and-twenty CLOSED with the "an" ligature (eenanzwanzeg);
    // honnert/dausend close onto the multiplier (zweehonnert)
    let tail = |x: u64| -> String {
        if x <= 19 {
            tens_units(x, &U, &T, &["", ""])
        } else {
            let t = X[((x / 10) - 2) as usize];
            let u = (x % 10) as usize;
            if u == 0 {
                t.to_string()
            } else if u == 1 {
                // een (not standalone eent) in compounds
                format!("eenan{t}")
            } else {
                format!("{}an{t}", U[u])
            }
        }
    };
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("honnert");
        } else if h > 1 {
            // unaccented zwee- in closed compounds (zweehonnert);
            // zwéi stays the standalone citation form
            if h == 2 {
                o.push_str("zwee");
            } else {
                o.push_str(U[h as usize]);
            }
            o.push_str("honnert");
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("eng Millioun".into());
    } else if mil > 1 {
        w.push(u1000(mil));
        w.push("Milliounen".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("dausend".into());
        } else if t == 2 {
            w.push("zweedausend".into());
        } else {
            // dausend CLOSES onto a single-word multiplier
            // (dräidausend, honnertdausend); compound tails keep the
            // space (eenanzwanzeg dausend)
            let m = u1000(t);
            if m.contains(' ') {
                w.push(m);
                w.push("dausend".into());
            } else {
                w.push(format!("{m}dausend"));
            }
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        w.push(u1000(r));
    }
    Some(w.join(" "))
}

fn lv_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "nulle", "viens", "divi", "trīs", "četri", "pieci", "seši", "septiņi", "astoņi", "deviņi",
    ];
    const T: [&str; 10] = [
        "desmit",
        "vienpadsmit",
        "divpadsmit",
        "trīspadsmit",
        "četrpadsmit",
        "piecpadsmit",
        "sešpadsmit",
        "septiņpadsmit",
        "astoņpadsmit",
        "deviņpadsmit",
    ];
    const X: [&str; 8] = [
        "divdesmit",
        "trīsdesmit",
        "četrdesmit",
        "piecdesmit",
        "sešdesmit",
        "septiņdesmit",
        "astoņdesmit",
        "deviņdesmit",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("nulle".into());
    }
    // space-separated (četrdesmit divi); scale words agree with the
    // multiplier — singular after a final 1 that is not 11 (divdesmit
    // viens tūkstotis), plural otherwise (divi simti, tūkstoši,
    // miljoni)
    let tail = |x: u64| tens_units(x, &U, &T, &X);
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("simts");
        } else if h > 1 {
            o.push_str(&tail(h));
            if h % 10 == 1 && h % 100 != 11 {
                o.push_str(" simts");
            } else {
                o.push_str(" simti");
            }
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("miljons".into());
    } else if mil > 1 {
        w.push(u1000(mil));
        if mil % 10 == 1 && mil % 100 != 11 {
            w.push("miljons".into());
        } else {
            w.push("miljoni".into());
        }
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("tūkstotis".into());
        } else {
            w.push(u1000(t));
            if t % 10 == 1 && t % 100 != 11 {
                w.push("tūkstotis".into());
            } else {
                w.push("tūkstoši".into());
            }
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        w.push(u1000(r));
    }
    Some(w.join(" "))
}

fn ml_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "പൂജ്യം",
        "ഒന്ന്",
        "രണ്ട്",
        "മൂന്ന്",
        "നാല്",
        "അഞ്ച്",
        "ആറ്",
        "ഏഴ്",
        "എട്ട്",
        "ഒൻപത്",
    ];
    const T: [&str; 10] = [
        "പത്ത്",
        "പതിനൊന്ന്",
        "പന്ത്രണ്ട്",
        "പതിമൂന്ന്",
        "പതിനാല്",
        "പതിനഞ്ച്",
        "പതിനാറ്",
        "പതിനേഴ്",
        "പതിനെട്ട്",
        "പത്തൊമ്പത്",
    ];
    const X: [&str; 8] = [
        "ഇരുപത്",
        "മുപ്പത്",
        "നാൽപത്",
        "അമ്പത്",
        "അറുപത്",
        "എഴുപത്",
        "എൺപത്",
        "തൊണ്ണൂറ്",
    ];
    // compound stems: ഇരുപത് + രണ്ട് → ഇരുപത്തിരണ്ട് (22)
    const S: [&str; 8] = [
        "ഇരുപത്തി",
        "മുപ്പത്തി",
        "നാൽപത്തി",
        "അമ്പത്തി",
        "അറുപത്തി",
        "എഴുപത്തി",
        "എൺപത്തി",
        "തൊണ്ണൂറ്റി",
    ];
    if n > 99_999 {
        return None;
    }
    if n == 0 {
        return Some("പൂജ്യം".into());
    }
    // Malayalam 21-99 are single closed words via the -പത്തി- stem
    let tail = |x: u64| -> String {
        match x {
            0..=9 => U[x as usize].to_string(),
            10..=19 => T[(x - 10) as usize].to_string(),
            _ => {
                let s = S[((x / 10) - 2) as usize];
                let u = (x % 10) as usize;
                if u == 0 {
                    X[((x / 10) - 2) as usize].to_string()
                } else {
                    format!("{s}{}", U[u])
                }
            }
        }
    };
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("നൂറ്");
        } else if h > 1 {
            o.push_str(U[h as usize]);
            o.push_str(" നൂറ്");
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000;
    let rest = n % 1_000;
    let mut w: Vec<String> = Vec::new();
    if mil > 0 {
        w.push(u1000(mil));
        w.push("ആയിരം".into());
    }
    if rest > 0 {
        w.push(u1000(rest));
    }
    Some(w.join(" "))
}

const NE_U: [&str; 10] = [
    "शून्य",
    "एक",
    "दुई",
    "तीन",
    "चार",
    "पाँच",
    "छ",
    "सात",
    "आठ",
    "नौ",
];

const NE_T: [&str; 10] = [
    "दस",
    "एघार",
    "बाह्र",
    "तेह्र",
    "चौध",
    "पन्ध्र",
    "सोह्र",
    "सत्र",
    "अठार",
    "उन्नाइस",
];

const NE_C: [&str; 80] = [
    "बीस",
    "एक्काइस",
    "बाइस",
    "तेइस",
    "चौबीस",
    "पच्चीस",
    "छब्बीस",
    "सत्ताइस",
    "अठ्ठाइस",
    "उनन्तीस",
    "तीस",
    "एकत्तीस",
    "बत्तीस",
    "तेत्तीस",
    "चौतीस",
    "पैंतीस",
    "छत्तीस",
    "सैंतीस",
    "अठतीस",
    "उनन्चालीस",
    "चालीस",
    "एकचालीस",
    "बयालीस",
    "त्रियालीस",
    "चवालीस",
    "पैंतालीस",
    "छयालीस",
    "सैंचालीस",
    "अठचालीस",
    "उनन्चास",
    "पचास",
    "एकाउन्न",
    "बाउन्न",
    "त्रिपन्न",
    "चवन्न",
    "पचपन्न",
    "छपन्न",
    "सन्तपन्न",
    "अन्ठपन्न",
    "उनन्सठी",
    "साठी",
    "एकसठी",
    "बयसठी",
    "तिरसठी",
    "चौंसठी",
    "पैंसठी",
    "छयसठी",
    "सत्सठी",
    "अठसठी",
    "उनन्सत्तरी",
    "सत्तरी",
    "एकहत्तर",
    "बहत्तर",
    "तिहत्तर",
    "चौहत्तर",
    "पचहत्तर",
    "छयहत्तर",
    "सतहत्तर",
    "अठहत्तर",
    "उनासी",
    "असी",
    "एकासी",
    "बयासी",
    "त्रीरासी",
    "चौरासी",
    "पचासी",
    "छयासी",
    "सतासी",
    "अठासी",
    "उनान्नब्बे",
    "नब्बे",
    "एकान्नब्बे",
    "बयान्नब्बे",
    "त्रियान्नब्बे",
    "चौरान्नब्बे",
    "पञ्चान्नब्बे",
    "छयान्नब्बे",
    "सन्तान्नब्बे",
    "अठान्नब्बे",
    "उनान्सय",
];

fn ne_integer(n: u64) -> Option<String> {
    // Nepali 20-99 are lexicalized single words (बयालीस 42 — the
    // dictionary carries them); listed by decade
    if n > 999_999 {
        return None;
    }
    if n == 0 {
        return Some("शून्य".into());
    }
    let tail = |x: u64| -> String {
        match x {
            0..=9 => NE_U[x as usize].to_string(),
            10..=19 => NE_T[(x - 10) as usize].to_string(),
            _ => NE_C[(x - 20) as usize].to_string(),
        }
    };
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("सय");
        } else if h > 1 {
            o.push_str(&tail(h));
            o.push_str(" सय");
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    // South Asian scale: हजार (1,000) then लाख (100,000)
    let lakh = n / 100_000;
    let rest = n % 100_000;
    let mut w: Vec<String> = Vec::new();
    if lakh > 0 {
        w.push(tail(lakh));
        w.push("लाख".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("एक हजार".into());
        } else {
            w.push(u1000(t));
            w.push("हजार".into());
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        w.push(u1000(r));
    }
    Some(w.join(" "))
}

fn no_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "null", "én", "to", "tre", "fire", "fem", "seks", "sju", "åtte", "ni",
    ];
    // compounds take the unaccented form (tjueen, not tjueén — the
    // acute marks standalone emphasis)
    const UC: [&str; 10] = [
        "null", "en", "to", "tre", "fire", "fem", "seks", "sju", "åtte", "ni",
    ];
    const T: [&str; 10] = [
        "ti", "elleve", "tolv", "tretten", "fjorten", "femten", "seksten", "sytten", "atten",
        "nitten",
    ];
    const X: [&str; 8] = [
        "tjue", "tretti", "førti", "femti", "seksti", "sytti", "åtti", "nitti",
    ];
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("null".into());
    }
    // Bokmål writes 21-99 as single words (førtito — the expanded nob
    // bundle carries it); scale words stay separate (to hundre,
    // førtito tusen)
    let tail = |x: u64| -> String {
        if x <= 19 {
            tens_units(x, &U, &T, &["", ""])
        } else {
            let t = X[((x / 10) - 2) as usize];
            let u = (x % 10) as usize;
            if u == 0 {
                t.to_string()
            } else {
                format!("{t}{}", UC[u])
            }
        }
    };
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("hundre");
        } else if h > 1 {
            o.push_str(&tail(h));
            o.push_str(" hundre");
        }
        if r > 0 {
            if !o.is_empty() {
                // "og" between the hundreds word and the tail
                // (to hundre og femti) — Bokmål convention
                o.push_str(" og ");
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("én million".into());
    } else if mil > 1 {
        w.push(u1000(mil));
        w.push("millioner".into());
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("ett tusen".into());
        } else {
            w.push(u1000(t));
            w.push("tusen".into());
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        // a sub-100 remainder after a scale word takes "og"
        // (to tusen og fem, én million og fem)
        if r < 100 && !w.is_empty() {
            w.push(format!("og {}", u1000(r)));
        } else {
            w.push(u1000(r));
        }
    }
    Some(w.join(" "))
}

fn sq_integer(n: u64) -> Option<String> {
    const U: [&str; 10] = [
        "zero", "një", "dy", "tre", "katër", "pesë", "gjashtë", "shtatë", "tetë", "nëntë",
    ];
    const T: [&str; 10] = [
        "dhjetë",
        "njëmbëdhjetë",
        "dymbëdhjetë",
        "trembëdhjetë",
        "katërmbëdhjetë",
        "pesëmbëdhjetë",
        "gjashtëmbëdhjetë",
        "shtatëmbëdhjetë",
        "tetëmbëdhjetë",
        "nëntëmbëdhjetë",
    ];
    const X: [&str; 8] = [
        "njëzet",
        "tridhjetë",
        "dyzet",
        "pesëdhjetë",
        "gjashtëdhjetë",
        "shtatëdhjetë",
        "tetëdhjetë",
        "nëntëdhjetë",
    ];
    if n > 999_999 {
        return None;
    }
    if n == 0 {
        return Some("zero".into());
    }
    // "e" links tens and units (dyzet e dy); closed hundreds
    // (njëqind, dyqind) and mijë
    let tail = |x: u64| -> String {
        if x <= 19 {
            tens_units(x, &U, &T, &["", ""])
        } else {
            let t = X[((x / 10) - 2) as usize];
            let u = (x % 10) as usize;
            if u == 0 {
                t.to_string()
            } else {
                format!("{t} e {}", U[u])
            }
        }
    };
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h == 1 {
            o.push_str("njëqind");
        } else if h > 1 {
            o.push_str(U[h as usize]);
            o.push_str("qind");
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000;
    let rest = n % 1_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("një mijë".into());
    } else if mil > 1 {
        w.push(u1000(mil));
        w.push("mijë".into());
    }
    if rest > 0 {
        w.push(u1000(rest));
    }
    Some(w.join(" "))
}

const SR_U: [&str; 10] = [
    "нула",
    "један",
    "два",
    "три",
    "четири",
    "пет",
    "шест",
    "седам",
    "осам",
    "девет",
];

const SR_T: [&str; 10] = [
    "десет",
    "једанаест",
    "дванаест",
    "тринаест",
    "четрнаест",
    "петнаест",
    "шеснаест",
    "седамнаест",
    "осамнаест",
    "деветнаест",
];

const SR_X: [&str; 8] = [
    "двадесет",
    "тридесет",
    "четрдесет",
    "петдесет",
    "шестдесет",
    "седамдесет",
    "осамдесет",
    "деветдесет",
];

const SR_H: [&str; 9] = [
    "сто",
    "двеста",
    "триста",
    "четиристо",
    "петсто",
    "шестсто",
    "седамсто",
    "осамсто",
    "деветсто",
];

fn sr_integer(n: u64) -> Option<String> {
    // Serbian Cyrillic — the sr_RS voices consume Cyrillic text.
    // "и" links tens and units; hundreds are single words (двеста,
    // триста, четиристо, петсто …); хиљада is feminine (две хиљаде)
    // with paucal; милион takes the genitive plural above 1.
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("нула".into());
    }
    let tail = |x: u64| -> String {
        if x <= 19 {
            tens_units(x, &SR_U, &SR_T, &["", ""])
        } else {
            let t = SR_X[((x / 10) - 2) as usize];
            let u = (x % 10) as usize;
            if u == 0 {
                t.to_string()
            } else {
                format!("{t} и {}", SR_U[u])
            }
        }
    };
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h >= 1 {
            o.push_str(SR_H[(h - 1) as usize]);
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("милион".into());
    } else if mil > 1 {
        // nominative singular after a final 1 (двадесет један
        // милион), genitive plural otherwise
        w.push(u1000(mil));
        let c10 = (mil % 10) as u32;
        let c100 = (mil % 100) as u32;
        if c10 == 1 && c100 != 11 {
            w.push("милион".into());
        } else {
            w.push("милиона".into());
        }
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("хиљаду".into());
        } else {
            let c10 = (t % 10) as u32;
            let c100 = (t % 100) as u32;
            // хиљада is feminine: једна/две (not један/два) — also
            // inside compound multipliers (двадесет две хиљаде)
            w.push(sr_fem_group(t));
            if c10 == 1 && c100 != 11 {
                w.push("хиљада".into());
            } else if (2..=4).contains(&c10) && !(12..=14).contains(&c100) {
                w.push("хиљаде".into());
            } else {
                w.push("хиљада".into());
            }
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        w.push(u1000(r));
    }
    Some(w.join(" "))
}

/// Ukrainian multiplier group with feminine final 1/2 (the тисяча
/// agreement), the same shape as Russian `ru_group(n, fem)`:
/// двадцять одна тисяча, двадцять дві тисячі.
fn uk_fem_group(n: u64) -> String {
    // feminine одна/дві replace the masculine final units
    const UF: [&str; 10] = [
        "нуль",
        "одна",
        "дві",
        "три",
        "чотири",
        "п'ять",
        "шість",
        "сім",
        "вісім",
        "дев'ять",
    ];
    let h = (n / 100) as usize;
    let r = n % 100;
    let mut words: Vec<&str> = Vec::new();
    if h > 0 {
        words.push(UK_H[h - 1]);
    }
    match r {
        0 => {}
        1..=9 => words.push(UF[r as usize]),
        10..=19 => words.push(UK_T[(r - 10) as usize]),
        _ => {
            words.push(UK_X[((r / 10) - 2) as usize]);
            let u = (r % 10) as usize;
            if u > 0 {
                words.push(UF[u]);
            }
        }
    }
    words.join(" ")
}

/// Serbian multiplier group with feminine final 1/2 (the хиљада
/// agreement), the same shape as Russian `ru_group(n, fem)`:
/// двадесет једна хиљада, двадесет две хиљаде.
fn sr_fem_group(n: u64) -> String {
    // feminine једна/две replace the masculine final units
    const UF: [&str; 10] = [
        "нула",
        "једна",
        "две",
        "три",
        "четири",
        "пет",
        "шест",
        "седам",
        "осам",
        "девет",
    ];
    let h = (n / 100) as usize;
    let r = n % 100;
    let mut words: Vec<&str> = Vec::new();
    if h > 0 {
        words.push(SR_H[h - 1]);
    }
    match r {
        0 => {}
        1..=9 => words.push(UF[r as usize]),
        10..=19 => words.push(SR_T[(r - 10) as usize]),
        _ => {
            words.push(SR_X[((r / 10) - 2) as usize]);
            let u = (r % 10) as usize;
            if u > 0 {
                // "и" links tens and units here too
                words.push("и");
                words.push(UF[u]);
            }
        }
    }
    words.join(" ")
}

const UK_U: [&str; 10] = [
    "нуль",
    "один",
    "два",
    "три",
    "чотири",
    "п'ять",
    "шість",
    "сім",
    "вісім",
    "дев'ять",
];

const UK_T: [&str; 10] = [
    "десять",
    "одинадцять",
    "дванадцять",
    "тринадцять",
    "чотирнадцять",
    "п'ятнадцять",
    "шістнадцять",
    "сімнадцять",
    "вісімнадцять",
    "дев'ятнадцять",
];

const UK_X: [&str; 8] = [
    "двадцять",
    "тридцять",
    "сорок",
    "п'ятдесят",
    "шістдесят",
    "сімдесят",
    "вісімдесят",
    "дев'яносто",
];

const UK_H: [&str; 9] = [
    "сто",
    "двісті",
    "триста",
    "чотириста",
    "п'ятсот",
    "шістсот",
    "сімсот",
    "вісімсот",
    "дев'ятсот",
];

fn uk_integer(n: u64) -> Option<String> {
    // Ukrainian: no conjunction in compound tens (сорок два); single-
    // word hundreds (двісті, триста, чотириста, п'ятсот …) — the uk
    // bundle carries them all; тисяча is feminine (дві тисячі) with
    // paucal; мільйон is masculine with paucal.
    if n > 999_999_999 {
        return None;
    }
    if n == 0 {
        return Some("нуль".into());
    }
    let tail = |x: u64| tens_units(x, &UK_U, &UK_T, &UK_X);
    let u1000 = |x: u64| -> String {
        let h = x / 100;
        let r = x % 100;
        let mut o = String::new();
        if h >= 1 {
            o.push_str(UK_H[(h - 1) as usize]);
        }
        if r > 0 {
            if !o.is_empty() {
                o.push(' ');
            }
            o.push_str(&tail(r));
        }
        o
    };
    let mil = n / 1_000_000;
    let rest = n % 1_000_000;
    let mut w: Vec<String> = Vec::new();
    if mil == 1 {
        w.push("мільйон".into());
    } else if mil > 1 {
        w.push(u1000(mil));
        let c10 = (mil % 10) as u32;
        let c100 = (mil % 100) as u32;
        if c10 == 1 && c100 != 11 {
            w.push("мільйон".into());
        } else if (2..=4).contains(&c10) && !(12..=14).contains(&c100) {
            w.push("мільйони".into());
        } else {
            w.push("мільйонів".into());
        }
    }
    if rest >= 1_000 {
        let t = rest / 1_000;
        if t == 1 {
            w.push("тисяча".into());
        } else {
            let c10 = (t % 10) as u32;
            let c100 = (t % 100) as u32;
            // тисяча is feminine: одна/дві (not один/два) — also
            // inside compound multipliers (двадцять дві тисячі);
            // nominative singular after a final 1 (not 11)
            w.push(uk_fem_group(t));
            if c10 == 1 && c100 != 11 {
                w.push("тисяча".into());
            } else if (2..=4).contains(&c10) && !(12..=14).contains(&c100) {
                w.push("тисячі".into());
            } else {
                w.push("тисяч".into());
            }
        }
    }
    let r = rest % 1_000;
    if r > 0 {
        w.push(u1000(r));
    }
    Some(w.join(" "))
}

// ── batch-2 specs + registration ──

batch1_normalizer!(
    AfrikaansNormalizer,
    "af",
    Batch1Spec {
        integer: af_integer,
        decimal_word: "komma",
        digits: ["nul", "een", "twee", "drie", "vier", "vyf", "ses", "sewe", "agt", "nege"],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    WelshNormalizer,
    "cy",
    Batch1Spec {
        integer: cy_integer,
        decimal_word: "pwynt",
        digits: ["sero", "un", "dau", "tri", "pedwar", "pump", "chwech", "saith", "wyth", "naw"],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    BasqueNormalizer,
    "eu",
    Batch1Spec {
        integer: eu_integer,
        decimal_word: "koma",
        digits: [
            "zero",
            "bat",
            "bi",
            "hiru",
            "lau",
            "bost",
            "sei",
            "zazpi",
            "zortzi",
            "bederatzi"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    HungarianNormalizer,
    "hu",
    Batch1Spec {
        integer: hu_integer,
        decimal_word: "vessző",
        digits: [
            "nulla", "egy", "kettő", "három", "négy", "öt", "hat", "hét", "nyolc", "kilenc"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    IcelandicNormalizer,
    "is",
    Batch1Spec {
        integer: is_integer,
        decimal_word: "komma",
        digits: [
            "núll", "einn", "tveir", "þrír", "fjórir", "fimm", "sex", "sjö", "átta", "níu"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    GeorgianNormalizer,
    "ka",
    Batch1Spec {
        integer: ka_integer,
        decimal_word: "მძიმე",
        digits: [
            "ნული",
            "ერთი",
            "ორი",
            "სამი",
            "ოთხი",
            "ხუთი",
            "ექვსი",
            "შვიდი",
            "რვა",
            "ცხრა"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    KazakhNormalizer,
    "kk",
    Batch1Spec {
        integer: kk_integer,
        decimal_word: "үтір",
        digits: [
            "нөл",
            "бір",
            "екі",
            "үш",
            "төрт",
            "бес",
            "алты",
            "жеті",
            "сегіз",
            "тоғыз"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    KoreanNormalizer,
    "ko",
    Batch1Spec {
        integer: ko_integer,
        decimal_word: "점",
        digits: ["영", "일", "이", "삼", "사", "오", "육", "칠", "팔", "구"],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    LuxembourgishNormalizer,
    "lb",
    Batch1Spec {
        integer: lb_integer,
        decimal_word: "Komma",
        digits: [
            "null", "eent", "zwéi", "dräi", "véier", "fënnef", "sechs", "siwen", "aacht", "nénge"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    LatvianNormalizer,
    "lv",
    Batch1Spec {
        integer: lv_integer,
        decimal_word: "komats",
        digits: [
            "nulle", "viens", "divi", "trīs", "četri", "pieci", "seši", "septiņi", "astoņi",
            "deviņi"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    MalayalamNormalizer,
    "ml",
    Batch1Spec {
        integer: ml_integer,
        decimal_word: "ദശാംശം",
        digits: [
            "പൂജ്യം",
            "ഒന്ന്",
            "രണ്ട്",
            "മൂന്ന്",
            "നാല്",
            "അഞ്ച്",
            "ആറ്",
            "ഏഴ്",
            "എട്ട്",
            "ഒൻപത്"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    NepaliNormalizer,
    "ne",
    Batch1Spec {
        integer: ne_integer,
        decimal_word: "दशमलव",
        digits: [
            "शून्य",
            "एक",
            "दुई",
            "तीन",
            "चार",
            "पाँच",
            "छ",
            "सात",
            "आठ",
            "नौ"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    NorwegianNormalizer,
    "no",
    Batch1Spec {
        integer: no_integer,
        decimal_word: "komma",
        digits: ["null", "én", "to", "tre", "fire", "fem", "seks", "sju", "åtte", "ni"],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    AlbanianNormalizer,
    "sq",
    Batch1Spec {
        integer: sq_integer,
        decimal_word: "presje",
        digits: [
            "zero", "një", "dy", "tre", "katër", "pesë", "gjashtë", "shtatë", "tetë", "nëntë"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    SerbianNormalizer,
    "sr",
    Batch1Spec {
        integer: sr_integer,
        decimal_word: "зарез",
        digits: [
            "нула",
            "један",
            "два",
            "три",
            "четири",
            "пет",
            "шест",
            "седам",
            "осам",
            "девет"
        ],
        dot_thousands: false,
    }
);
batch1_normalizer!(
    UkrainianNormalizer,
    "uk",
    Batch1Spec {
        integer: uk_integer,
        decimal_word: "кома",
        digits: [
            "нуль",
            "один",
            "два",
            "три",
            "чотири",
            "п'ять",
            "шість",
            "сім",
            "вісім",
            "дев'ять"
        ],
        dot_thousands: false,
    }
);

// ──────────────────────────────────────────────────────────────────
// Provisioning (model setup time; never inside the audio path)
// ──────────────────────────────────────────────────────────────────

/// Provision the exact bundle named by an allowlist row. A local manifest,
/// matching SHA stamp, and both FST files let an installed voice start without
/// a network request. If the cache is incomplete or its stamp differs from the
/// cached manifest, consult the remote archive and verify any download.
///
/// # Errors
/// A one-line message (network, checksum, manifest); the engine logs
/// it once and retries on a later utterance (throttled).
pub fn provision_bundle(archive: Archive, key: &str) -> Result<PathBuf, String> {
    let cache = voicegarden_lexicons::default_cache_dir();
    let manifest_name = match archive {
        Archive::Base => "lexicons.json",
        Archive::Expanded => "expanded.json",
    };
    provision_from(&cache, manifest_name, key, || {
        let base = voicegarden_lexicons::DEFAULT_BASE;
        match archive {
            Archive::Base => LexiconArchive::new(base, &cache),
            Archive::Expanded => LexiconArchive::new_expanded(base, &cache),
        }
        .map_err(|e| format!("lexicon archive: {e}"))
    })
}

/// Is the bundle for `key` present and stamp-verified in the cache,
/// with NO network access? (The catalogue's offline-readiness check —
/// #106. Shares the exact cache-hit logic `provision_from` uses.)
#[cfg(feature = "sherpaonnx")]
pub(crate) fn bundle_cached(archive: Archive, key: &str) -> bool {
    let cache = voicegarden_lexicons::default_cache_dir();
    let manifest_name = match archive {
        Archive::Base => "lexicons.json",
        Archive::Expanded => "expanded.json",
    };
    cached_bundle_dir(&cache, manifest_name, key).is_some()
}

/// The stamp-verified cache dir for `key`, or None (the shared
/// cache-hit predicate of [`provision_bundle`], factored out so the
/// catalogue can consult it offline).
#[cfg(feature = "sherpaonnx")]
fn cached_bundle_dir(cache: &Path, manifest_name: &str, key: &str) -> Option<PathBuf> {
    let dir = cache.join(key);
    let stamp = std::fs::read_to_string(dir.join(".sha256")).ok()?;
    let has_bundle =
        dir.join(format!("{key}.fst")).is_file() && dir.join(format!("{key}.pho")).is_file();
    if !has_bundle {
        return None;
    }
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(cache.join(manifest_name)).ok()?).ok()?;
    manifest
        .languages
        .iter()
        .any(|entry| entry.lang == key && entry.sha256 == stamp)
        .then_some(dir)
}

fn provision_from(
    cache: &Path,
    manifest_name: &str,
    key: &str,
    open_archive: impl FnOnce() -> Result<LexiconArchive, String>,
) -> Result<PathBuf, String> {
    if let Some(dir) = cached_bundle_dir(cache, manifest_name, key) {
        return Ok(dir);
    }
    let dir = cache.join(key);
    let stamp = std::fs::read_to_string(dir.join(".sha256"))
        .ok()
        .filter(|_| {
            // a stamp without the bundle files proves nothing
            dir.join(format!("{key}.fst")).is_file() && dir.join(format!("{key}.pho")).is_file()
        });

    let archive = open_archive()?;
    // VoiceGarden also resolves BCP-47 aliases. The allowlist names an exact
    // bundle, so reject an alias that resolves to a different key.
    let Some(entry) = archive.entry(key).filter(|entry| entry.lang == key) else {
        return Err(format!("no '{key}' bundle in the archive manifest"));
    };
    // files + stamp already match the REMOTE manifest (the local
    // manifest is absent/stale): no fetch needed
    if stamp.as_deref() == Some(entry.sha256.as_str()) {
        return Ok(dir);
    }
    let bundle = archive
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
        floravox_g2p_fst::LexiconWriter::new(dir.join("de"))
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
        let mut fe = G2pFrontend::open(&dir, "de", &tokens).expect("frontend");

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
        let mut fe = G2pFrontend::open(&dir, "de", &tokens).expect("frontend");

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
        let mut fe = G2pFrontend::open(&dir, "de", &tokens).expect("frontend");

        // "xyzzy" is OOV: no WFST in the mini bundle → rule fallback
        // spells English letter names → unmappable → dropped, counted
        let r = fe.phonemize("Haus xyzzy Tag");
        assert_eq!(r.words, 3);
        assert_eq!(r.dropped, 1);
        assert_eq!(r.sentences.len(), 1); // the two good words survive

        // everything dropped → empty sentences (caller falls back to text)
        let r = fe.phonemize("qqqq");
        assert!(r.sentences.is_empty(), "all-dropped falls back to text");
        assert_eq!(r.dropped, 1);
    }

    #[test]
    fn frontend_expands_digit_tokens_with_attached_punctuation() {
        let dir = tempfile::tempdir().unwrap().keep();
        mini_bundle(&dir);
        let tokens = mini_tokens(&dir);
        let mut fe = G2pFrontend::open(&dir, "de", &tokens).expect("frontend");

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
        floravox_g2p_fst::LexiconWriter::new(dir.join("de"))
            .write(vec![("schön".into(), "ʃ eː n".into())])
            .expect("write mini lexicon");
        let tokens = mini_tokens(&dir);
        let mut fe = G2pFrontend::open(&dir, "de", &tokens).expect("frontend");
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
        let Err(err) = G2pFrontend::open(&dir, "de", &dir.join("tokens2.txt")) else {
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
        // mixed alphanumerics and out-of-range numerals are None as
        // PURE numerals — the run-split handles them at the next layer
        assert!(digits_to_german("1e9").is_none());
        assert!(digits_plus_suffix("1e9").is_none()); // "e9" is not a suffix
        assert!(digits_to_german("1000000000").is_none());
        // ... but the run split decomposes them German-ly
        assert_eq!(split_digit_runs("1e9").as_deref(), Some("eins e neun"));
        assert_eq!(
            split_digit_runs("COVID-19").as_deref(),
            Some("covid neunzehn")
        );
        assert_eq!(split_digit_runs("A4").as_deref(), Some("a vier"));
        assert_eq!(split_digit_runs("b2b").as_deref(), Some("b zwei b"));
        assert_eq!(split_digit_runs("MP3").as_deref(), Some("mp drei"));
        // an unexpandable digit run kills the whole token (drop path)
        assert_eq!(split_digit_runs("x1000000000"), None);
        // leading zeros collapse (documented: fine for quantities)
        assert_eq!(d("007"), "sieben");
    }

    #[test]
    fn phonemize_drops_unhandled_numerics_and_expands_suffix_forms() {
        let dir = tempfile::tempdir().unwrap().keep();
        // the mini dictionary gains the suffix-expanded word
        floravox_g2p_fst::LexiconWriter::new(dir.join("de"))
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
        let mut fe = G2pFrontend::open(&dir, "de", &tokens).expect("frontend");

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

        // mixed tokens decompose into German runs — never the English
        // letter speller (loop-4 finding, live-verified). In this
        // WFST-less fixture the alphabetic runs still miss the
        // dictionary and drop; the RUN SPLIT itself is asserted above
        // and by the real-bundle tests.
        let r = fe.phonemize("Haus 1e9 Tag");
        assert_eq!(r.sentences.len(), 1); // sentence survives
        let r = fe.phonemize("Haus b2b Tag");
        assert_eq!(r.sentences.len(), 1);
    }

    #[test]
    fn provisioning_opens_the_archive_only_on_a_cache_miss() {
        let cache = tempfile::tempdir().unwrap();
        let cache = cache.path();
        std::fs::write(
            cache.join("expanded.json"),
            r#"{"version":"t","languages":[{"lang":"deu","name":"German","bcp47":"de",
                "entries":1,"license":"MIT","source":"t","phonetisaurus":null,
                "file":"deu.tar.gz","sha256":"abc123"}]}"#,
        )
        .unwrap();
        let dir = cache.join("deu");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("deu.fst"), b"x").unwrap();
        std::fs::write(dir.join("deu.pho"), b"x").unwrap();

        let archive_unavailable = || Err("archive unavailable".into());
        std::fs::write(dir.join(".sha256"), b"stale").unwrap();
        assert_eq!(
            provision_from(cache, "expanded.json", "deu", archive_unavailable),
            Err("archive unavailable".into()),
            "a stale stamp must open the archive"
        );
        std::fs::write(dir.join(".sha256"), b"abc123").unwrap();
        let got = provision_from(cache, "expanded.json", "deu", || {
            panic!("a complete cache must not open the archive")
        })
        .expect("cache hit");
        assert_eq!(got, dir);

        std::fs::remove_file(dir.join("deu.pho")).unwrap();
        assert_eq!(
            provision_from(cache, "expanded.json", "deu", archive_unavailable),
            Err("archive unavailable".into()),
            "an incomplete bundle must open the archive"
        );
    }

    /// #112: crate 0.3.0's `entry()` ran the bcp47 prefix heuristic
    /// in the same pass as exact matching, so an `en-GB` request
    /// resolved to the earlier `en` row — and the exact-id guard in
    /// `provision_from` rejected it as a wrong bundle (`no 'en-GB'
    /// bundle in the archive manifest`). The trap order — `en` listed
    /// BEFORE `en-GB` — is the point of this fixture. Fixed in
    /// voicegarden-lexicons 0.3.1 (exact id → exact bcp47 → prefix).
    #[test]
    fn provisioning_resolves_the_exact_bundle_id_not_a_prefix_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let base = tmp.path().join("base");
        std::fs::create_dir_all(cache.join("en-GB")).unwrap();
        std::fs::create_dir_all(&base).unwrap();
        // LOCAL manifest parseable but stale for en-GB (its en-GB
        // digest differs from the stamp, so the cache-hit check must
        // not short-circuit — a parse failure would reach the same
        // miss, but for the wrong reason)
        std::fs::write(
            cache.join("lexicons.json"),
            r#"{"version":"t","languages":[{"lang":"en-GB","name":"British English",
                "bcp47":"en-GB","entries":1,"license":"MIT","source":"t",
                "phonetisaurus":null,"file":"en-GB.tar.gz","sha256":"stale"}]}"#,
        )
        .unwrap();
        std::fs::write(cache.join("en-GB/en-GB.fst"), b"x").unwrap();
        std::fs::write(cache.join("en-GB/en-GB.pho"), b"x").unwrap();
        std::fs::write(cache.join("en-GB/.sha256"), b"bb").unwrap();
        // REMOTE manifest with the trap order: en BEFORE en-GB
        std::fs::write(
            base.join("lexicons.json"),
            r#"{"version":"t","languages":[
                {"lang":"en","name":"English","bcp47":"en","entries":1,"license":"MIT",
                 "source":"t","phonetisaurus":null,"file":"en.tar.gz","sha256":"aa"},
                {"lang":"en-GB","name":"British English","bcp47":"en-GB","entries":1,
                 "license":"MIT","source":"t","phonetisaurus":null,
                 "file":"en-GB.tar.gz","sha256":"bb"}]}"#,
        )
        .unwrap();
        // NOTE: no archive may be constructed before provision_from —
        // LexiconArchive::new writes the manifest through to the cache
        // (fetch_manifest), which would refresh the stale local
        // manifest above and let the cache-hit path short-circuit past
        // the entry() lookup this test exists to exercise.
        //
        // With crate 0.3.0 the prefix pass resolved 'en' here and the
        // exact-id guard returned Err; 0.3.1 resolves the exact id —
        // and the matching stamp means no fetch, so the whole path is
        // offline.
        let got = provision_from(&cache, "lexicons.json", "en-GB", || {
            Ok(LexiconArchive::new(base.to_string_lossy().to_string(), &cache).unwrap())
        })
        .expect("the exact en-GB bundle resolves through the archive");
        assert_eq!(got, cache.join("en-GB"));
    }

    // ── real-bundle coverage (env-gated, like Phase 2's) ──
    //
    //   SHERPA_TEST_THORSTEN_TOKENS=.../tokens.txt \
    //   SHERPA_TEST_DEU_DIR=.../voicegarden/lexicons/deu \
    //   cargo test --features sherpaonnx,sherpa-g2p --lib sherpa_g2p -- --ignored

    fn env_val(var: &str) -> Option<PathBuf> {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.exists())
    }

    #[test]
    #[ignore = "needs the real thorsten tokens.txt + deu bundle dir (SHERPA_TEST_THORSTEN_TOKENS, SHERPA_TEST_DEU_DIR)"]
    fn real_bundle_phonemizes_a_german_sentence() {
        let (Some(tokens), Some(bundle)) = (
            env_val("SHERPA_TEST_THORSTEN_TOKENS"),
            env_val("SHERPA_TEST_DEU_DIR"),
        ) else {
            eprintln!("skipping: env vars not set");
            return;
        };
        let key = bundle.file_name().unwrap().to_string_lossy().to_string();
        let mut fe = G2pFrontend::open(&bundle, &key, &tokens).expect("frontend");
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
    #[ignore = "needs the real thorsten tokens.txt + deu bundle dir (SHERPA_TEST_THORSTEN_TOKENS, SHERPA_TEST_DEU_DIR)"]
    fn real_bundle_pre_synthesis_phone_assertions() {
        // upstream #4139's lesson, as a gate: a non-empty WAV passes even
        // when the front-end dropped half the sentence, so the words that
        // matter must produce valid model phones BEFORE any synthesis —
        // a capitalized noun, a name ABSENT from the dictionary, and a
        // number (the digit normalizer's job)
        let (Some(tokens), Some(bundle)) = (
            env_val("SHERPA_TEST_THORSTEN_TOKENS"),
            env_val("SHERPA_TEST_DEU_DIR"),
        ) else {
            eprintln!("skipping: env vars not set");
            return;
        };
        let key = bundle.file_name().unwrap().to_string_lossy().to_string();
        let vocab = read_tokens_txt_public(&tokens).unwrap();
        let mut fe = G2pFrontend::open(&bundle, &key, &tokens).expect("frontend");

        // a name confirmed absent from the dictionary (pick from a small
        // candidate list so the test self-adjusts if the lexicon grows)
        let key = bundle.file_name().unwrap().to_string_lossy().to_string();
        let lex = floravox_g2p_fst::MmapLexicon::open(bundle.join(format!("{key}.fst")))
            .expect("direct lexicon open for the absence check");
        let _ = &key; // used above
        let absent_name = ["xanthippe", "balderich", "gunzelin"]
            .into_iter()
            .find(|n| lex.lookup(n).is_none())
            .expect("at least one candidate absent from the dictionary");

        let r = fe.phonemize("Haus");
        assert_eq!((r.words, r.dropped), (1, 0), "capitalized noun");
        for sentence in &r.sentences {
            for &cp in sentence {
                let c = char::from_u32(cp.cast_unsigned()).unwrap();
                assert!(vocab.contains(&c.to_string()), "Haus: {c:?} not in vocab");
            }
        }

        let r = fe.phonemize(absent_name);
        assert_eq!(
            (r.words, r.dropped),
            (1, 0),
            "dictionary-absent name must still be SPOKEN (WFST), not dropped"
        );
        assert!(!r.sentences.is_empty(), "WFST must speak the OOV word");
        for sentence in &r.sentences {
            for &cp in sentence {
                let c = char::from_u32(cp.cast_unsigned()).unwrap();
                assert!(
                    vocab.contains(&c.to_string()),
                    "{absent_name}: {c:?} not in vocab"
                );
            }
        }

        let r = fe.phonemize("42.");
        assert_eq!(r.dropped, 0, "number with punctuation");
        let sentence: String = r.sentences[0]
            .iter()
            .filter_map(|&cp| char::from_u32(cp.cast_unsigned()))
            .collect();
        assert!(sentence.ends_with('.'), "pause token survives: {sentence}");
        // and CONTENT before the pause: the number's own phones, not
        // just any stream + '.' — with deu, zweiundvierzig is a
        // dictionary hit and carries /ç/ (-zig)
        let before = &sentence[..sentence.len() - 1];
        assert!(
            before.contains('\u{00E7}'),
            "number phones before the pause: {sentence}"
        );
    }

    #[test]
    #[ignore = "needs the real deu bundle dir (SHERPA_TEST_DEU_DIR)"]
    fn real_bundle_oov_word_goes_through_the_wfst() {
        let Some(bundle) = env_val("SHERPA_TEST_DEU_DIR") else {
            eprintln!("skipping: env var not set");
            return;
        };
        // tokens from the bundle dir's sibling model? use thorsten env too
        let tokens = env_val("SHERPA_TEST_THORSTEN_TOKENS").expect("tokens env");
        let key = bundle.file_name().unwrap().to_string_lossy().to_string();
        let mut fe = G2pFrontend::open(&bundle, &key, &tokens).expect("frontend");
        // a nonce word: not in the dictionary, must still be spoken
        // (WFST guess) — dropped == 0 is the assertion that matters
        let r = fe.phonemize("Blorpinxel");
        assert_eq!(r.dropped, 0, "OOV must go through the WFST, not drop");
        assert_eq!(r.sentences.len(), 1);
    }

    #[test]
    fn identity_normalizer_drops_digits_with_a_count() {
        // a French-shaped bundle (no fr normalizer yet): digit tokens
        // are UNHANDLED — dropped and counted, never German-expanded
        // and never English letter-spelled via the rule fallback
        let dir = tempfile::tempdir().unwrap().keep();
        floravox_g2p_fst::LexiconWriter::new(dir.join("fr"))
            .write(vec![("bonjour".into(), "b o ʒ u ʁ".into())])
            .expect("write fr lexicon");
        let tokens = mini_tokens(&dir);
        let mut fe = G2pFrontend::open(&dir, "fr", &tokens).expect("frontend");
        let r = fe.phonemize("bonjour 42");
        assert_eq!(r.words, 2);
        assert_eq!(r.dropped, 1, "digits drop with a count for unhandled langs");
        assert_eq!(r.sentences.len(), 1, "the sentence survives");
        let sentence: String = r.sentences[0]
            .iter()
            .filter_map(|&cp| char::from_u32(cp.cast_unsigned()))
            .collect();
        assert!(sentence.contains('ʒ'), "bonjour spoke: {sentence}");
        assert!(
            !sentence.contains('e') || !sentence.contains("tsvae"),
            "no German zweiundvierzig: {sentence}"
        );
        assert!(
            !sentence.contains("w\u{028c}n"),
            "no English letter names: {sentence}"
        );
    }

    #[test]
    fn normalizer_registry_routes_by_language() {
        assert_eq!(normalizer_for("de").lang(), "de");
        assert_eq!(normalizer_for("de-DE").lang(), "de");
        assert_eq!(
            normalizer_for("deu").lang(),
            "de",
            "corpus-tag keys route too"
        );
        assert_eq!(normalizer_for("fr").lang(), "fr", "French routes");
        assert_eq!(normalizer_for("fra").lang(), "fr", "corpus-tag fra routes");
        assert_eq!(normalizer_for("es").lang(), "es");
        assert_eq!(normalizer_for("spa").lang(), "es", "corpus-tag spa routes");
        assert_eq!(normalizer_for("pt").lang(), "pt");
        assert_eq!(normalizer_for("por").lang(), "pt");
        assert_eq!(normalizer_for("it").lang(), "it");
        assert_eq!(normalizer_for("ita").lang(), "it");
        // still-unhandled → identity (Kurdish: no data anywhere)
        assert_eq!(normalizer_for("ku").lang(), "", "unhandled → identity");
        assert!(normalizer_for("ku").normalize_numeric("42").is_none());
    }

    #[test]
    fn french_digit_normalizer() {
        let f = |t: &str| FrenchNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("0"), "zéro");
        assert_eq!(f("1"), "un");
        assert_eq!(f("5"), "cinq");
        assert_eq!(f("16"), "seize");
        assert_eq!(f("17"), "dix-sept");
        assert_eq!(f("20"), "vingt");
        assert_eq!(f("21"), "vingt et un");
        assert_eq!(f("22"), "vingt-deux");
        assert_eq!(f("70"), "soixante-dix");
        assert_eq!(f("71"), "soixante-onze");
        assert_eq!(f("72"), "soixante-douze");
        assert_eq!(f("80"), "quatre-vingts");
        assert_eq!(f("81"), "quatre-vingt-un");
        assert_eq!(f("91"), "quatre-vingt-onze");
        assert_eq!(f("92"), "quatre-vingt-douze");
        assert_eq!(f("99"), "quatre-vingt-dix-neuf");
        assert_eq!(f("100"), "cent");
        assert_eq!(f("200"), "deuxcents");
        assert_eq!(f("1000"), "mille");
        assert_eq!(f("2000"), "deuxmille");
        assert_eq!(f("1,5"), "unvirgulecinq");
        assert_eq!(f("42"), "quarante-deux");
        // out of range → None → drop
        assert!(FrenchNormalizer.normalize_numeric("1000000000").is_none());
    }

    #[test]
    fn spanish_digit_normalizer() {
        let f = |t: &str| SpanishNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("0"), "cero");
        assert_eq!(f("5"), "cinco");
        assert_eq!(f("15"), "quince");
        assert_eq!(f("16"), "dieciséis");
        assert_eq!(f("20"), "veinte");
        assert_eq!(f("21"), "veintiuno");
        assert_eq!(f("30"), "treinta");
        assert_eq!(f("42"), "cuarenta y dos");
        assert_eq!(f("100"), "cien");
        assert_eq!(f("1000"), "mil");
        assert_eq!(f("1,5"), "unocomacinco");
        assert!(SpanishNormalizer.normalize_numeric("1000000000").is_none());
    }

    #[test]
    fn portuguese_digit_normalizer() {
        let f = |t: &str| PortugueseNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("0"), "zero");
        assert_eq!(f("3"), "três");
        assert_eq!(f("16"), "dezesseis");
        assert_eq!(f("20"), "vinte");
        assert_eq!(f("21"), "vinte e um");
        assert_eq!(f("42"), "quarenta e dois");
        assert_eq!(f("100"), "cem");
        assert_eq!(f("1000"), "mil");
        assert_eq!(f("1,5"), "umvírgulacinco");
    }

    #[test]
    fn italian_digit_normalizer() {
        let f = |t: &str| ItalianNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("0"), "zero");
        assert_eq!(f("3"), "tre");
        assert_eq!(f("16"), "sedici");
        assert_eq!(f("20"), "venti");
        assert_eq!(f("21"), "ventuno"); // vowel elision
        assert_eq!(f("28"), "ventotto"); // vowel elision
        assert_eq!(f("42"), "quarantadue");
        assert_eq!(f("100"), "cento");
        assert_eq!(f("1000"), "mila");
        assert_eq!(f("1,5"), "unovirgolacinque");
    }

    #[test]
    fn english_digit_normalizer() {
        let f = |t: &str| EnglishNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("0"), "zero");
        assert_eq!(f("3"), "three");
        assert_eq!(f("13"), "thirteen");
        assert_eq!(f("20"), "twenty");
        assert_eq!(f("21"), "twenty-one");
        assert_eq!(f("42"), "forty-two");
        assert_eq!(f("100"), "one hundred");
        assert_eq!(f("115"), "one hundred fifteen");
        assert_eq!(f("1000"), "one thousand");
        assert_eq!(f("1,000"), "one thousand");
        assert_eq!(f("1.5"), "one point five");
        assert_eq!(f("1,000,000"), "one million");
        assert_eq!(
            f("1234567"),
            "one million two hundred thirty-four thousand five hundred sixty-seven"
        );
        // "3.500" is a decimal in en-US convention (dot decimal) —
        // spoken digit-by-digit after the point, not European thousands
        assert_eq!(f("3.500"), "three point five zero zero");
        // malformed separators drop rather than guess a number
        assert!(EnglishNormalizer.normalize_numeric("12,34.5").is_none());
        assert!(EnglishNormalizer.normalize_numeric("1,.5").is_none());
        assert!(EnglishNormalizer.normalize_numeric("1234,567").is_none());
        assert!(EnglishNormalizer.normalize_numeric("1000000000").is_none());
        // mixed alphanumerics split (mirrors the German path)
        assert_eq!(f("A4"), "a four");
        assert_eq!(f("MP3"), "mp three");
        assert_eq!(f("COVID-19"), "covid nineteen");
        assert_eq!(f("b2b"), "b two b");
        // ordinals still drop rather than guess ("forty-eight th")
        assert!(EnglishNormalizer.normalize_numeric("48th").is_none());
    }

    #[test]
    fn russian_digit_normalizer() {
        let f = |t: &str| RussianNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("0"), "ноль");
        assert_eq!(f("3"), "три");
        assert_eq!(f("16"), "шестнадцать");
        assert_eq!(f("20"), "двадцать");
        assert_eq!(f("21"), "двадцать один");
        assert_eq!(f("42"), "сорок два"); // irregular сорок
        assert_eq!(f("90"), "девяносто");
        assert_eq!(f("100"), "сто");
        assert_eq!(f("200"), "двести"); // irregular hundreds
        assert_eq!(f("400"), "четыреста");
        assert_eq!(f("115"), "сто пятнадцать");
        assert_eq!(f("1000"), "одна тысяча"); // fem
        assert_eq!(f("2000"), "две тысячи"); // fem + plural
        assert_eq!(f("5000"), "пять тысяч");
        assert_eq!(f("21000"), "двадцать одна тысяча");
        assert_eq!(f("22000"), "двадцать две тысячи");
        assert_eq!(f("25000"), "двадцать пять тысяч");
        assert_eq!(f("100000"), "сто тысяч");
        assert_eq!(f("111000"), "сто одиннадцать тысяч"); // 11-14 exception
        assert_eq!(f("1000000"), "один миллион");
        assert_eq!(f("2000000"), "два миллиона");
        assert_eq!(f("5000000"), "пять миллионов");
        // the full agreement stack in one string: millions + feminine
        // thousands + paucal + tail
        assert_eq!(
            f("1234567"),
            "один миллион двести тридцать четыре тысячи пятьсот шестьдесят семь"
        );
        assert_eq!(f("101000"), "сто одна тысяча"); // fem + paucal
        assert_eq!(f("21000000"), "двадцать один миллион"); // masc before миллион
        assert_eq!(f("3,5"), "три запятая пять"); // comma decimal
                                                  // dot is not a Russian separator — drop, never guess. Note
                                                  // the deliberate asymmetry vs German: "1.000,5" (dot
                                                  // thousands + comma decimal) drops here but its German analog
                                                  // "3.500,50" expands — Russian text does not conventionally
                                                  // dot-group, and a lone dot is more likely a typo'd decimal
        assert!(RussianNormalizer.normalize_numeric("3.5").is_none());
        assert!(RussianNormalizer.normalize_numeric("3.500").is_none());
        assert!(RussianNormalizer.normalize_numeric("1.000,5").is_none());
        assert!(RussianNormalizer.normalize_numeric("1000000000").is_none());
    }

    #[test]
    fn batch2_digit_normalizers() {
        // Afrikaans: tens split with "en", hundreds/thousands closed
        let f = |t: &str| AfrikaansNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "twee en veertig");
        assert_eq!(f("21"), "een en twintig");
        assert_eq!(f("200"), "tweehonderd");
        assert_eq!(f("2000"), "tweeduisend");
        assert_eq!(f("1,5"), "een komma vyf");
        // Welsh: modern decimal (dau ddeg), mutations on cant/mil
        let f = |t: &str| WelshNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "pedwar deg dau");
        assert_eq!(f("20"), "dau ddeg");
        assert_eq!(f("11"), "un deg un");
        assert_eq!(f("100"), "cant");
        assert_eq!(f("300"), "tri chant");
        assert_eq!(f("2000"), "dwy fil"); // mil is feminine
        assert_eq!(f("5000"), "pum mil"); // pum before a noun
                                          // Basque: vigesimal -ta- compounds
        let f = |t: &str| BasqueNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "berrogeita bi");
        assert_eq!(f("30"), "hogeita hamar");
        assert_eq!(f("31"), "hogeita hamaika");
        assert_eq!(f("21"), "hogeita bat");
        assert_eq!(f("100"), "ehun");
        // batua closed hundreds
        assert_eq!(f("200"), "berrehun");
        assert_eq!(f("999"), "bederatziehun laurogeita hemeretzi");
        // Icelandic: "og" conjunction, gender agreement (neuter
        // tvö/þrjú/fjögur on hundrað/þúsund, fem tvær/þrjár on
        // milljónir), masculine citation in the units group
        let f = |t: &str| IcelandicNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "fjörutíu og tveir");
        assert_eq!(f("200"), "tvö hundruð");
        assert_eq!(f("300"), "þrjú hundruð");
        assert_eq!(f("101"), "hundrað og einn");
        assert_eq!(f("102"), "hundrað og tveir");
        assert_eq!(f("205"), "tvö hundruð og fimm");
        assert_eq!(f("342"), "þrjú hundruð og fjörutíu og tveir");
        assert_eq!(f("2000"), "tvö þúsund");
        assert_eq!(f("2005"), "tvö þúsund og fimm");
        assert_eq!(f("2042"), "tvö þúsund og fjörutíu og tveir");
        assert_eq!(f("1000000"), "ein milljón");
        assert_eq!(f("3000000"), "þrjár milljónir");
        // Luxembourgish: -an- closed compounds
        let f = |t: &str| {
            LuxembourgishNormalizer
                .normalize_numeric(t)
                .expect("in range")
        };
        assert_eq!(f("42"), "zwéianvéierzeg");
        assert_eq!(f("21"), "eenanzwanzeg");
        assert_eq!(f("200"), "zweehonnert");
        assert_eq!(f("2000"), "zweedausend");
        // Latvian: split, plural scale words
        let f = |t: &str| LatvianNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "četrdesmit divi");
        assert_eq!(f("100"), "simts");
        assert_eq!(f("200"), "divi simti");
        assert_eq!(f("2000"), "divi tūkstoši");
        assert_eq!(f("21000"), "divdesmit viens tūkstotis");
        assert_eq!(f("25000"), "divdesmit pieci tūkstoši");
        // Malayalam: -പത്തി- stem compounds
        let f = |t: &str| MalayalamNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "നാൽപത്തിരണ്ട്");
        assert_eq!(f("22"), "ഇരുപത്തിരണ്ട്");
        assert_eq!(f("40"), "നാൽപത്");
        assert_eq!(f("100"), "നൂറ്");
        // Nepali: lexicalized 20-99, लाख tier
        let f = |t: &str| NepaliNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "बयालीस");
        assert_eq!(f("21"), "एक्काइस");
        assert_eq!(f("99"), "उनान्सय");
        assert_eq!(f("100"), "सय");
        assert_eq!(f("100000"), "एक लाख");
        assert_eq!(f("142000"), "एक लाख बयालीस हजार");
        // Norwegian: closed 21-99 (førtito), separate scale words
        let f = |t: &str| NorwegianNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "førtito");
        assert_eq!(f("21"), "tjueen");
        assert_eq!(f("100"), "hundre");
        assert_eq!(f("200"), "to hundre");
        assert_eq!(f("250"), "to hundre og femti");
        assert_eq!(f("2000"), "to tusen");
        assert_eq!(f("2005"), "to tusen og fem");
        assert_eq!(f("42000"), "førtito tusen");
    }

    #[test]
    fn batch2_digit_normalizers_complex_scripts() {
        // Hungarian: closed agglutinative compounds, huszon- 20s,
        // két- scale multiplier, kétszáz/kétezer/egymillió
        let f = |t: &str| HungarianNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "negyvenkettő");
        assert_eq!(f("21"), "huszonegy");
        assert_eq!(f("22"), "huszonkettő");
        assert_eq!(f("200"), "kétszáz");
        assert_eq!(f("2000"), "kétezer");
        assert_eq!(f("21000"), "huszonegyezer");
        assert_eq!(f("22000"), "huszonkétezer");
        assert_eq!(f("102000"), "százkétezer");
        assert_eq!(f("1000000"), "egymillió");
        assert_eq!(
            f("123456789"),
            "százhuszonhárommillió négyszázötvenhatezer hétszáznyolcvankilenc"
        );
        // Georgian: vigesimal compounds, closed hundreds
        let f = |t: &str| GeorgianNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "ორმოცდაორი");
        assert_eq!(f("20"), "ოცი");
        assert_eq!(f("21"), "ოცდაერთი");
        assert_eq!(f("100"), "ასი");
        assert_eq!(f("200"), "ორასი");
        assert_eq!(f("1000"), "ათასი");
        // Kazakh: space-separated throughout (қырық екі, екі жүз, екі мың)
        let f = |t: &str| KazakhNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "қырық екі");
        assert_eq!(f("100"), "жүз");
        assert_eq!(f("200"), "екі жүз");
        // Korean: Sino-Korean, 만 grouping (천 tier inside the
        // multiplier), no 억
        let f = |t: &str| KoreanNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "사십이");
        assert_eq!(f("10000"), "만");
        assert_eq!(f("100000"), "십만");
        assert_eq!(f("1000000"), "백만");
        assert_eq!(f("12000"), "만이천");
        assert_eq!(f("99999999"), "구천구백구십구만구천구백구십구");
        assert!(KoreanNormalizer.normalize_numeric("100000000").is_none());
        // Albanian: "e" conjunction, closed hundreds
        let f = |t: &str| AlbanianNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "dyzet e dy");
        assert_eq!(f("100"), "njëqind");
        assert_eq!(f("200"), "dyqind");
        // Serbian: Cyrillic, "и" conjunction, closed hundreds, fem
        // две + paucal хиљаде
        let f = |t: &str| SerbianNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "четрдесет и два");
        assert_eq!(f("200"), "двеста");
        assert_eq!(f("400"), "четиристо");
        assert_eq!(f("1000"), "хиљаду");
        assert_eq!(f("2000"), "две хиљаде");
        assert_eq!(f("5000"), "пет хиљада");
        // feminine inside compound multipliers; singular after final 1
        assert_eq!(f("21000"), "двадесет и једна хиљада");
        assert_eq!(f("22000"), "двадесет и две хиљаде");
        assert_eq!(f("21000000"), "двадесет и један милион");
        // Ukrainian: no conjunction, closed hundreds, fem дві
        // тисячі, masc paucal millions
        let f = |t: &str| UkrainianNormalizer.normalize_numeric(t).expect("in range");
        assert_eq!(f("42"), "сорок два");
        assert_eq!(f("200"), "двісті");
        assert_eq!(f("500"), "п'ятсот");
        assert_eq!(f("2000"), "дві тисячі");
        assert_eq!(f("5000"), "п'ять тисяч");
        assert_eq!(f("21000"), "двадцять одна тисяча");
        assert_eq!(f("22000"), "двадцять дві тисячі");
        assert_eq!(f("2000000"), "два мільйони");
        assert_eq!(f("5000000"), "п'ять мільйонів");
    }

    #[test]
    fn batch2_normalizer_routes() {
        // bare codes, region forms, and the 639-3 tags the expanded
        // manifest carries (isl → is is the one that matters live)
        assert_eq!(normalizer_for("is").lang(), "is");
        assert_eq!(normalizer_for("is-IS").lang(), "is");
        assert_eq!(normalizer_for("isl").lang(), "is");
        for l in [
            "af", "cy", "eu", "hu", "ka", "kk", "ko", "lb", "lv", "ml", "ne", "no", "sq", "sr",
            "uk",
        ] {
            assert_eq!(normalizer_for(l).lang(), l);
        }
        assert_eq!(normalizer_for("no-NO").lang(), "no");
        assert_eq!(normalizer_for("nb").lang(), "no"); // folded to the nob routing language
        assert_eq!(normalizer_for("uk-UA").lang(), "uk");
        // unchanged: unknown locales stay identity (empty lang)
        assert_eq!(normalizer_for("zz").lang(), "");
    }

    #[test]
    fn russian_normalizer_routes() {
        assert_eq!(normalizer_for("ru").lang(), "ru");
        assert_eq!(normalizer_for("ru-RU").lang(), "ru");
        assert_eq!(normalizer_for("rus").lang(), "ru");
    }

    #[test]
    fn english_normalizer_routes() {
        assert_eq!(normalizer_for("en").lang(), "en");
        assert_eq!(normalizer_for("en-US").lang(), "en");
        assert_eq!(normalizer_for("en-GB").lang(), "en");
        assert_eq!(normalizer_for("eng").lang(), "en");
    }

    #[test]
    fn mms_digit_words_normalization() {
        // English: digits expand to words; other tokens untouched
        let (t, dropped) = normalize_digits_in_text("The number 42 is spoken.", "eng");
        assert_eq!(t, "The number forty two is spoken.");
        assert_eq!(dropped, 0);
        // decimals + thousands
        let (t, _) = normalize_digits_in_text("1.5 of 1,000 things", "en-US");
        assert_eq!(t, "one point five of one thousand things");
        // a locale with no rules: the numeric drops (counted), words pass
        let (t, dropped) = normalize_digits_in_text("abc 42 def", "zul");
        assert_eq!(t, "abc def");
        assert_eq!(dropped, 1);
        // German MMS: lang code routes to the German rules
        let (t, _) = normalize_digits_in_text("Ich habe 42 Häuser.", "deu");
        assert_eq!(t, "Ich habe zweiundvierzig Häuser.");
        // all-numeric unknown-locale text: everything drops (the
        // engine falls back to the original text rather than silence)
        let (t, dropped) = normalize_digits_in_text("1e9", "zul");
        assert_eq!(t, "");
        assert_eq!(dropped, 1);
    }

    #[test]
    fn hyphenated_tokens_split_for_dictionary_lookup() {
        // typed compound — no lexicon carries hyphenated entries
        assert_eq!(
            token_parts_with(&EnglishNormalizer, "forty-two"),
            Some(vec!["forty".to_string(), "two".to_string()])
        );
        // digit expansion output splits the same way
        assert_eq!(
            token_parts_with(&EnglishNormalizer, "42."),
            Some(vec!["forty".to_string(), "two.".to_string()])
        );
        // trailing punctuation stays attached to the LAST part
        assert_eq!(
            token_parts_with(&EnglishNormalizer, "well-known,"),
            Some(vec!["well".to_string(), "known,".to_string()])
        );
        // German compounds equally
        assert_eq!(
            token_parts_with(&GermanNormalizer, "Kfz-Kennzeichen"),
            Some(vec!["Kfz".to_string(), "Kennzeichen".to_string()])
        );
        // punctuation-only tokens pass through unchanged
        assert_eq!(
            token_parts_with(&GermanNormalizer, "--"),
            Some(vec!["--".to_string()])
        );
        // plain tokens untouched
        assert_eq!(
            token_parts_with(&GermanNormalizer, "Haus"),
            Some(vec!["Haus".to_string()])
        );
        // Unicode dashes split too (U+2010 hyphen, U+2013 en dash)
        assert_eq!(
            token_parts_with(&GermanNormalizer, "Kfz‐Kennzeichen"),
            Some(vec!["Kfz".to_string(), "Kennzeichen".to_string()])
        );
        assert_eq!(
            token_parts_with(&EnglishNormalizer, "well–known"),
            Some(vec!["well".to_string(), "known".to_string()])
        );
    }
}

#[test]
fn batch1_digit_normalizers() {
    // Catalan: vint-i-un range, quaranta-dos, coma decimals
    let f = |t: &str| CatalanNormalizer.normalize_numeric(t).expect("in range");
    assert_eq!(f("21"), "vint-i-un");
    assert_eq!(f("42"), "quaranta dos");
    assert_eq!(f("100"), "cent");
    assert_eq!(f("121"), "cent vint-i-un");
    assert_eq!(f("1.000"), "mil");
    assert_eq!(f("1,5"), "un coma cinc");
    assert_eq!(f("2.000.000"), "dos milions");
    // Czech: paucal thousands, celá decimals
    let f = |t: &str| CzechNormalizer.normalize_numeric(t).expect("in range");
    assert_eq!(f("42"), "čtyřicet dva");
    assert_eq!(f("2000"), "dva tisíce");
    assert_eq!(f("5000"), "pět tisíc");
    assert_eq!(f("21000"), "dvacet jeden tisíc");
    assert_eq!(f("1000000"), "milion");
    assert_eq!(f("1,5"), "jeden celá pět");
    assert_eq!(f("200"), "dvě stě"); // declined hundreds
    assert_eq!(f("500"), "pět set");
    assert_eq!(f("14"), "čtrnáct"); // irregular spelling
                                    // Danish: one-and-twenty SPLIT
    let f = |t: &str| DanishNormalizer.normalize_numeric(t).expect("in range");
    assert_eq!(f("42"), "to og fyrre");
    assert_eq!(f("21"), "en og tyve");
    assert_eq!(f("50"), "halvtreds");
    assert_eq!(f("1,5"), "en komma fem");
    assert_eq!(f("250"), "to hundrede og halvtreds");
    assert_eq!(f("2005"), "to tusinde og fem");
    assert_eq!(f("1000000"), "en million");
    assert_eq!(f("2000000"), "to millioner");
    assert!(DanishNormalizer.normalize_numeric("1.5").is_none());
    // Dutch: closed compounds with ligatures
    let f = |t: &str| DutchNormalizer.normalize_numeric(t).expect("in range");
    assert_eq!(f("21"), "eenentwintig");
    assert_eq!(f("42"), "tweeënveertig");
    assert_eq!(f("43"), "drieënveertig");
    assert_eq!(f("1000"), "duizend");
    assert_eq!(f("121"), "honderdeenentwintig"); // closed hundred join
    assert_eq!(f("3,14"), "drie komma een vier");
    assert!(DutchNormalizer.normalize_numeric("1.5").is_none());
    // Polish: paucal
    let f = |t: &str| PolishNormalizer.normalize_numeric(t).expect("in range");
    assert_eq!(f("42"), "czterdzieści dwa");
    assert_eq!(f("22"), "dwadzieścia dwa");
    assert_eq!(f("1000"), "tysiąc");
    assert_eq!(f("2000"), "dwa tysiące");
    assert_eq!(f("5000"), "pięć tysięcy");
    assert_eq!(f("1,5"), "jeden przecinek pięć");
    assert_eq!(f("3,14"), "trzy przecinek jeden cztery");
    assert_eq!(f("1.000.000"), "milion");
    assert_eq!(
        f("1234567"),
        "milion dwieście trzydzieści cztery tysiące pięćset sześćdziesiąt siedem"
    );
    // Slovak
    let f = |t: &str| SlovakNormalizer.normalize_numeric(t).expect("in range");
    assert_eq!(f("42"), "štyridsať dva");
    assert_eq!(f("2000"), "dva tisíce");
    assert_eq!(f("1,5"), "jeden celá päť");
    // Slovenian: one-and-twenty closed
    let f = |t: &str| SlovenianNormalizer.normalize_numeric(t).expect("in range");
    assert_eq!(f("42"), "dvainštirideset");
    assert_eq!(f("21"), "enaindvajset");
    assert_eq!(f("100"), "sto");
    assert_eq!(f("200"), "dvesto");
    assert_eq!(f("1,5"), "ena cela pet");
    // millions: dual 2, nominative plural 3-4, genitive 5+
    assert_eq!(f("2000000"), "dva milijona");
    assert_eq!(f("3000000"), "tri milijoni");
    assert_eq!(f("4000000"), "štiri milijoni");
    assert_eq!(f("5000000"), "pet milijonov");
    assert_eq!(f("14000000"), "štirinajst milijonov");
    // Swedish: compounds SPLIT (the swe dictionary gap)
    let f = |t: &str| SwedishNormalizer.normalize_numeric(t).expect("in range");
    assert_eq!(f("42"), "fyrtio två");
    assert_eq!(f("21"), "tjugo ett");
    assert_eq!(f("50"), "femtio");
    assert_eq!(f("1,5"), "ett komma fem");
    assert_eq!(f("1000"), "ettusen"); // neuter
    assert_eq!(f("1000000"), "en miljon"); // common gender
    assert_eq!(f("80"), "åttio"); // single t
    assert!(SwedishNormalizer.normalize_numeric("1.5").is_none());
    // out of range for all
    for n in &["ca", "cs", "da", "nl", "pl", "sk", "sl", "sv"] {
        let norm = normalizer_for(match *n {
            "ca" => "ca",
            "cs" => "cs",
            "da" => "da",
            "nl" => "nl",
            "pl" => "pl",
            "sk" => "sk",
            "sl" => "sl",
            _ => "sv",
        });
        assert!(norm.normalize_numeric("1000000000").is_none());
    }
}

#[test]
fn batch1_iso3_routes() {
    assert_eq!(normalizer_for("cat").lang(), "ca");
    assert_eq!(normalizer_for("ces").lang(), "cs");
    assert_eq!(normalizer_for("dan").lang(), "da");
    assert_eq!(normalizer_for("nld").lang(), "nl");
    assert_eq!(normalizer_for("pol").lang(), "pl");
    assert_eq!(normalizer_for("slk").lang(), "sk");
    assert_eq!(normalizer_for("slv").lang(), "sl");
    assert_eq!(normalizer_for("swe").lang(), "sv");
    assert_eq!(normalizer_for("sv-SE").lang(), "sv");
    // Icelandic flipped in batch-2 (v0.5.0 fixed the compound tens);
    // nob folds to the no normalizer alongside the bundle routing
    assert_eq!(normalizer_for("is").lang(), "is");
    assert_eq!(normalizer_for("isl").lang(), "is");
    assert_eq!(normalizer_for("nob").lang(), "no");
}
