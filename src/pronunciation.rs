//! User pronunciation overrides (#125): a plain-text file where the
//! user teaches the wrapper how words should sound, applied at the
//! text-normalization stage before engine-specific synthesis.
//!
//! ```text
//! # respell — universal fallback, works on every engine
//! botox = bow tocks
//!
//! # phoneme override — used where the engine accepts phonemes
//! botox = /ˈboʊ.tɑːks/
//!
//! # multi-word phrases (names) work for respells
//! san rafael = san rah fah el
//! ```
//!
//! Matching is whole-word, case-insensitive, longest-key-first: a
//! key never fires inside another word (`bow` does not rewrite
//! `elbow`), and `san rafael` wins over a bare `rafael` entry.
//! Applied per utterance; the file is re-read when its mtime moves
//! (editors take effect on the next utterance, no rebuild).
//!
//! Two application policies:
//!
//! * [`Policy::AllRespell`] — every entry contributes its respell
//!   form (entries with only a phoneme form are left unchanged).
//!   Used by engines that receive plain text.
//! * [`Policy::KeepPhonemeWords`] — entries that carry a phoneme
//!   form stay untouched so a phoneme-capable front-end (the sherpa
//!   G2P path) can inject the IPA itself; respell-only entries
//!   substitute. Used by the sherpa phoneme front-end input.
//!
//! Heteronyms are out of scope by design (#125 non-goals): a static
//! file cannot disambiguate `tear`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// One user entry, merged across lines with the same key (a respell
/// line and a `/ipa/` line for the same word combine).
#[derive(Debug, Clone, PartialEq)]
pub struct OverrideEntry {
    /// Match key, NFC, lowercased, internal whitespace collapsed to
    /// single spaces, trimmed.
    pub key: String,
    /// The respell replacement (spoken as plain text).
    pub respell: Option<String>,
    /// The IPA replacement `/…/` payload, when provided.
    pub phoneme: Option<String>,
}

/// The parsed file, ready to apply.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pronunciations {
    /// Longest key first, then lexicographic — deterministic order.
    entries: Vec<OverrideEntry>,
}

/// A single applied replacement: the original span in the input and
/// the span of its replacement in the output text.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanMap {
    /// The typed word's span in the ORIGINAL text (chars).
    pub orig: std::ops::Range<usize>,
    /// The replacement's span in the REWRITTEN text (chars).
    pub new: std::ops::Range<usize>,
}

/// The result of applying overrides to a text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    /// The rewritten text.
    pub text: String,
    /// Replacement spans, ordered by position. Everything outside
    /// these spans maps 1:1 between input and output.
    pub spans: Vec<SpanMap>,
}

/// Which entries substitute as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Substitute every entry's respell form (phoneme-only entries
    /// pass through). The plain-text engine policy.
    AllRespell,
    /// Leave phoneme-bearing entries untouched (a phoneme front-end
    /// handles them); substitute only respell-only entries.
    KeepPhonemeWords,
}

/// Parse the file content. Malformed lines are skipped (speech must
/// never fail because a settings file has a typo); returns the
/// entries and the number of skipped lines so callers can surface it.
#[must_use]
pub fn parse(source: &str) -> (Pronunciations, usize) {
    let mut by_key: HashMap<String, OverrideEntry> = HashMap::new();
    let mut skipped = 0usize;
    // BOMs must not glue themselves onto keys (#131): a leading one
    // (Notepad's default "UTF-8 with BOM") or one starting any later
    // line (concatenated saves) would silently kill that entry
    for raw in source.strip_prefix('\u{feff}').unwrap_or(source).lines() {
        let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key_raw, value_raw)) = line.split_once('=') else {
            skipped += 1;
            continue;
        };
        let key = normalize_key(key_raw);
        let value = value_raw.trim().to_string();
        if key.is_empty() || value.is_empty() || key.contains('=') {
            skipped += 1;
            continue;
        }
        let phoneme = if value.len() >= 2 && value.starts_with('/') && value.ends_with('/') {
            Some(value[1..value.len() - 1].trim().to_string())
        } else {
            None
        };
        let respell = if phoneme.is_some() { None } else { Some(value) };
        let entry = by_key.entry(key).or_insert(OverrideEntry {
            key: String::new(),
            respell: None,
            phoneme: None,
        });
        // a key may carry BOTH forms across lines (respell line +
        // phoneme line); last non-empty line of each form wins
        if let Some(r) = respell {
            entry.respell = Some(r);
        }
        if let Some(p) = phoneme {
            entry.phoneme = Some(p);
        }
    }
    let mut entries: Vec<OverrideEntry> = by_key
        .into_iter()
        .map(|(key, mut e)| {
            e.key = key;
            e
        })
        .collect();
    // longest key first so `san rafael` wins over `rafael`; ties
    // break lexicographically for determinism
    entries.sort_by(|a, b| {
        b.key
            .chars()
            .count()
            .cmp(&a.key.chars().count())
            .then_with(|| a.key.cmp(&b.key))
    });
    (Pronunciations { entries }, skipped)
}

/// Lowercase, collapse internal whitespace runs to single spaces.
/// Keys are stored in this form; the same transform is applied to
/// scanned text so the comparison is stable. (No Unicode
/// normalization: a decomposed key simply matches decomposed text —
/// the module must build without the sherpaonnx-only
/// unicode-normalization dependency.)
fn normalize_key(key: &str) -> String {
    let lower: String = key
        .trim()
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c == '\u{03C2}' { '\u{03C3}' } else { c })
        .collect();
    let mut out = String::with_capacity(lower.len());
    let mut in_space = false;
    for c in lower.chars() {
        if c.is_whitespace() {
            if !in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out.trim_end().to_string()
}

impl Pronunciations {
    /// True when no entries loaded (the common case — no file).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Apply overrides to `text` under `policy`. Whole-word,
    /// case-insensitive, longest-key-first, left-to-right,
    /// non-overlapping. Spans are CHAR ranges.
    #[must_use]
    pub fn apply(&self, text: &str, policy: Policy) -> Applied {
        if self.entries.is_empty() || text.is_empty() {
            return Applied {
                text: text.to_string(),
                spans: Vec::new(),
            };
        }
        // lowercased scan buffer with a char-index map back to the
        // original text (case folding can change char counts, e.g.
        // İ → i̇; spans must address the ORIGINAL text)
        let orig_chars: Vec<char> = text.chars().collect();
        let mut lowered = String::with_capacity(text.len());
        let mut lower_of: Vec<usize> = Vec::with_capacity(orig_chars.len() + 1);
        for (i, c) in orig_chars.iter().enumerate() {
            // fold each char independently and record the mapping
            // for every folded char it produces (canonical sigma —
            // same as normalize_key)
            for f in c.to_lowercase() {
                let f = if f == '\u{03C2}' { '\u{03C3}' } else { f };
                lowered.push(f);
                lower_of.push(i);
            }
        }
        lower_of.push(orig_chars.len()); // sentinel
        let lower_chars: Vec<char> = lowered.chars().collect();

        let mut out = String::with_capacity(text.len());
        let mut spans = Vec::new();
        let mut i = 0usize; // index into lower_chars
        let used: Vec<&OverrideEntry> = self
            .entries
            .iter()
            .filter(|e| match policy {
                Policy::AllRespell => e.respell.is_some(),
                Policy::KeepPhonemeWords => e.respell.is_some() && e.phoneme.is_none(),
            })
            .collect();
        while i < lower_chars.len() {
            let orig_i = lower_of[i];
            // keys only match at a word start: begin of text or a
            // non-alphanumeric char before
            let at_word_start = orig_i == 0 || !orig_chars[orig_i - 1].is_alphanumeric();
            if at_word_start {
                let mut hit: Option<(&OverrideEntry, usize)> = None; // entry + key char len
                for e in &used {
                    let k: Vec<char> = e.key.chars().collect();
                    if k.len() > lower_chars.len() - i {
                        continue;
                    }
                    if lower_chars[i..i + k.len()] != k[..] {
                        continue;
                    }
                    // word END boundary: end of text or a
                    // non-alphanumeric char after the key
                    let end = i + k.len();
                    let orig_end_char = lower_of[end - 1];
                    let after = orig_chars.get(orig_end_char + 1);
                    let at_word_end = after.is_none_or(|c| !c.is_alphanumeric());
                    if at_word_end {
                        hit = Some((e, k.len()));
                        break; // entries are longest-first
                    }
                }
                if let Some((e, klen)) = hit {
                    let orig_start = lower_of[i];
                    let last_orig = lower_of[i + klen - 1];
                    // advance past EVERY folded char of the last
                    // matched original char — a single original char
                    // can fold to several (İ → i + U+0307); stopping
                    // at klen alone re-emitted it once more
                    let mut j = i + klen;
                    while j < lower_chars.len() && lower_of[j] == last_orig {
                        j += 1;
                    }
                    let orig_end = last_orig + 1;
                    let replacement = e.respell.clone().unwrap_or_default();
                    let new_start = out.chars().count();
                    out.push_str(&replacement);
                    let new_end = out.chars().count();
                    spans.push(SpanMap {
                        orig: orig_start..orig_end,
                        new: new_start..new_end,
                    });
                    i = j;
                    continue;
                }
            }
            // no match at this position: copy the ORIGINAL char(s)
            // whose fold started here (keeps out and lower_of in
            // sync without re-folding)
            let mut j = i + 1;
            while j < lower_chars.len() && lower_of[j] == lower_of[i] {
                j += 1;
            }
            let start = lower_of[i];
            let end = lower_of[j];
            for c in &orig_chars[start..end] {
                out.push(*c);
            }
            i = j;
        }
        Applied { text: out, spans }
    }

    /// The phoneme form for a single-word key (NFC, lowercased),
    /// when the entry carries one. The per-token lookup for
    /// phoneme-capable front-ends.
    #[must_use]
    pub fn phoneme_for(&self, word: &str) -> Option<&str> {
        let key = normalize_key(word);
        if key.contains(' ') {
            return None; // phrase keys cannot fire per-token
        }
        self.entries
            .iter()
            .find(|e| e.key == key)
            .and_then(|e| e.phoneme.as_deref())
    }
}

/// Translate a boundary-event span reported against the REWRITTEN
/// text back to the ORIGINAL text. Engine boundary offsets are BYTE
/// indices (the callback contract: `byte_offset`/`byte_len`); the
/// span maps work in chars — convert at both ends. An offset inside
/// a replacement maps to the original word's span (every spoken
/// word of a respell highlights the word the user typed); offsets
/// elsewhere shift by the accumulated delta. `orig` is the text the
/// caller submitted, `new` the rewritten text the engine saw.
#[must_use]
pub fn remap_boundary_span(
    orig: &str,
    new: &str,
    spans: &[SpanMap],
    offset: i32,
    length: i32,
) -> (i32, i32) {
    // byte → char index in the REWRITTEN text (engines emit word
    // starts, but floor to a char boundary rather than slice mid-char)
    let mut off_b = (offset.max(0) as usize).min(new.len());
    while !new.is_char_boundary(off_b) {
        off_b -= 1; // floor (loop-review r2: a mid-char byte offset
                    // from an engine must never panic the utterance)
    }
    let new_char = new[..off_b].chars().count();
    let mut shift = 0i64;
    for s in spans {
        if new_char >= s.new.start && new_char < s.new.end {
            // inside a replacement: the original word's span
            return char_span_to_bytes(orig, s.orig.clone());
        }
        if s.new.end <= new_char {
            shift += i64::from((s.orig.end - s.orig.start) as u32)
                - i64::from((s.new.end - s.new.start) as u32);
        }
    }
    let mapped_char = (new_char as i64 + shift).max(0) as usize;
    let start_b = char_index_to_byte(orig, mapped_char);
    let mut end_b = start_b + (length.max(0) as usize).min(orig.len().saturating_sub(start_b));
    // never hand back a span that splits a character: round the end
    // UP to the next boundary (a highlight slightly wider is fine;
    // a mid-char slice panics byte-slicing consumers)
    while end_b < orig.len() && !orig.is_char_boundary(end_b) {
        end_b += 1;
    }
    (start_b as i32, (end_b - start_b) as i32)
}

/// The byte offset and byte length of a CHAR range in `s`.
fn char_span_to_bytes(s: &str, chars: std::ops::Range<usize>) -> (i32, i32) {
    let start_b = char_index_to_byte(s, chars.start);
    let end_b = char_index_to_byte(s, chars.end);
    (start_b as i32, (end_b - start_b) as i32)
}

/// Byte offset of a CHAR index in `s` (clamped to len).
fn char_index_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(b, _)| b)
}

// ── global state: default path, mtime refresh, frontend override ──

/// The frontend-explicit file (ONE static shared by the setter and
/// the resolver — an earlier draft had two same-named locals and the
/// setter wrote the one nobody read).
static EXPLICIT_FILE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn explicit_file() -> Option<PathBuf> {
    EXPLICIT_FILE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The active file: an explicit frontend path wins, then
/// `RUST_TTS_PRONUNCIATIONS`, then the default location.
fn resolved_path() -> PathBuf {
    if let Some(p) = explicit_file() {
        return p;
    }
    if let Some(p) = std::env::var_os("RUST_TTS_PRONUNCIATIONS") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    default_path()
}

/// `~/.rust-tts-wrapper/pronunciations.txt` — home resolved the same
/// way as every other per-user path (#123: `HOME`, then
/// `USERPROFILE`); None when no home is resolvable (the
/// fresh-machine guard).
#[must_use]
pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match home {
        Some(h) if !h.is_empty() => PathBuf::from(h)
            .join(".rust-tts-wrapper")
            .join("pronunciations.txt"),
        _ => PathBuf::from(".rust-tts-wrapper").join("pronunciations.txt"),
    }
}

/// Point the overrides at an explicit file (the ABI setter for
/// frontends such as Dasher keeping the file under `%APPDATA%`).
/// Takes effect on the next utterance. Pass an empty path to
/// return to the default resolution.
pub fn set_file(path: Option<PathBuf>) {
    *EXPLICIT_FILE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = path;
    // force a reload of the new file on next use
    STATE
        .get_or_init(|| Rw {
            state: Mutex::new(State {
                path: None,
                mtime: None,
                loaded: None,
                skipped: 0,
            }),
        })
        .write()
        .force_reload();
}

struct State {
    path: Option<PathBuf>,
    mtime: Option<std::time::SystemTime>,
    loaded: Option<Arc<Pronunciations>>,
    /// Malformed lines in the last successful load (ABI surfaces it
    /// so GUI hosts can flag typos in-place — stderr is invisible
    /// there).
    skipped: usize,
}

struct Rw {
    state: Mutex<State>,
}

impl Rw {
    fn write(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

static STATE: OnceLock<Rw> = OnceLock::new();

impl State {
    fn force_reload(&mut self) {
        self.loaded = None;
        self.mtime = None;
        self.skipped = 0;
    }
}

/// The currently active overrides (mtime-checked, reloaded on
/// change). None when no file exists or it is empty — the no-cost
/// common path.
#[must_use]
pub fn active() -> Option<Arc<Pronunciations>> {
    let rw = STATE.get_or_init(|| Rw {
        state: Mutex::new(State {
            path: None,
            mtime: None,
            loaded: None,
            skipped: 0,
        }),
    });
    let path = resolved_path();
    let mut st = rw.write();
    // path changed under us (setter or env): drop the cache
    if st.path.as_ref() != Some(&path) {
        st.force_reload();
        st.path = Some(path);
    }
    let path = st.path.clone()?;
    let path = path.as_path();
    let mtime = modified_time(path);
    if st.loaded.is_some() && st.mtime == mtime {
        return st.loaded.clone();
    }
    let Ok(source) = std::fs::read_to_string(path) else {
        // unreadable/missing: treat as no overrides (never fail
        // speech because a settings file moved)
        st.force_reload();
        st.mtime = mtime;
        return None;
    };
    let (p, skipped) = parse(&source);
    st.skipped = skipped;
    if skipped > 0 {
        static SKIPPED_LOGGED: AtomicU64 = AtomicU64::new(0);
        if SKIPPED_LOGGED.swap(1, Ordering::Relaxed) == 0 {
            eprintln!(
                "pronunciations: skipped {skipped} malformed line(s) in {}",
                path.display()
            );
        }
    }
    st.mtime = mtime;
    let arc = Arc::new(p);
    st.loaded = Some(arc.clone());
    Some(arc)
}

/// What a frontend editor needs to know about the active overrides
/// (#131): entry count, malformed-line count from the last load,
/// and whether the feature is live at all (a bindings pin older
/// than the feature silently ignores the file).
#[must_use]
pub fn stats() -> (usize, usize) {
    // ensure the state reflects the CURRENT file (set_file only
    // invalidates; a stats call before any speak must still load)
    let _ = active();
    let rw = STATE.get_or_init(|| Rw {
        state: Mutex::new(State {
            path: None,
            mtime: None,
            loaded: None,
            skipped: 0,
        }),
    });
    let st = rw.write();
    (st.loaded.as_ref().map_or(0, |p| p.len()), st.skipped)
}

/// Was a pronunciation file loaded at all (even one whose entries
/// are ALL malformed — the skipped count is still real)? The ABI's
/// boolean return distinguishes that from "no file".
#[must_use]
pub fn file_loaded() -> bool {
    // ensure the state reflects the CURRENT file
    let _ = active();
    STATE
        .get_or_init(|| Rw {
            state: Mutex::new(State {
                path: None,
                mtime: None,
                loaded: None,
                skipped: 0,
            }),
        })
        .write()
        .loaded
        .is_some()
}

/// Dry-run (#131): the text after the text-form (respell)
/// substitutions — what a plain-text engine will speak. Phoneme-form
/// entries are NOT substituted here (only a phoneme front-end uses
/// them); document that in the editor preview.
#[must_use]
pub fn preview(text: &str) -> String {
    match active() {
        Some(p) if !p.is_empty() => p.apply(text, Policy::AllRespell).text,
        _ => text.to_string(),
    }
}

/// mtime of a file, None when missing. The refresh signal — a new
/// mtime (including None ↔ Some on create/delete) reloads.
fn modified_time(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(source: &str) -> Pronunciations {
        let (p, skipped) = parse(source);
        assert_eq!(skipped, 0, "unexpected skipped lines");
        p
    }

    #[test]
    fn parses_both_forms_and_merges_by_key() {
        let p = entries("botox = bow tocks\nbotox = /ˈboʊ.tɑːks/\nnguyen = wen\n");
        assert_eq!(p.len(), 2);
        let botox = p.entries.iter().find(|e| e.key == "botox").unwrap();
        assert_eq!(botox.respell.as_deref(), Some("bow tocks"));
        // the phoneme payload keeps its symbol separators (splitting
        // IPA into symbols is the front-end's job)
        assert_eq!(botox.phoneme.as_deref(), Some("ˈboʊ.tɑːks"));
    }

    #[test]
    fn whole_word_case_insensitive_with_punctuation() {
        let p = entries("botox = bow tocks\nbow = boh\n");
        let a = p.apply(
            "I said Botox, (botox) not elbow or botoxic!",
            Policy::AllRespell,
        );
        assert_eq!(
            a.text,
            "I said bow tocks, (bow tocks) not elbow or botoxic!"
        );
        assert_eq!(a.spans.len(), 2);
    }

    #[test]
    fn longest_phrase_wins() {
        let p = entries("rafael = raf ael\nsan rafael = san rah fah el\n");
        let a = p.apply("San Rafael is a city", Policy::AllRespell);
        assert_eq!(a.text, "san rah fah el is a city");
    }

    #[test]
    fn keep_phoneme_words_policy_leaves_phoneme_entries() {
        let p = entries("botox = bow tocks\nbotox = /x/\nplain = resPELLed\n");
        let a = p.apply("botox stays, plain goes", Policy::KeepPhonemeWords);
        assert_eq!(a.text, "botox stays, resPELLed goes");
    }

    #[test]
    fn spans_address_original_text() {
        let p = entries("botox = bow tocks\n");
        let a = p.apply("um Botox end", Policy::AllRespell);
        assert_eq!(a.text, "um bow tocks end");
        assert_eq!(a.spans.len(), 1);
        let s = &a.spans[0];
        // char ranges into the ORIGINAL text
        let orig: Vec<char> = "um Botox end".chars().collect();
        assert_eq!(&orig[s.orig.clone()], "Botox".chars().collect::<Vec<_>>());
    }

    #[test]
    fn boundary_remap_inside_before_after() {
        let p = entries("botox = bow tocks\n");
        let orig = "um Botox end";
        let a = p.apply(orig, Policy::AllRespell);
        let r = |off: i32, len: i32| remap_boundary_span(orig, &a.text, &a.spans, off, len);
        // ASCII: byte offsets == char offsets here
        assert_eq!(r(0, 2), (0, 2));
        assert_eq!(r(3, 3), (3, 5));
        assert_eq!(r(7, 5), (3, 5));
        // an offset INSIDE the replacement also maps to the typed
        // word (11 < new end 12) — every spoken word highlights it
        assert_eq!(r(11, 1), (3, 5));
        // "end" starts at new offset 13; shift = 5 - 9 = -4
        assert_eq!(r(13, 3), (9, 3));
    }

    /// Loop-review r2 catch: an engine reporting a mid-character
    /// byte offset (astral chars) must never panic — floor to the
    /// character boundary instead.
    #[test]
    fn boundary_remap_floors_mid_char_offsets() {
        let p = entries("botox = bow tocks\n");
        let orig = "\u{1F600} Botox end";
        let a = p.apply(orig, Policy::AllRespell);
        // byte 2 is inside the 4-byte emoji in the REWRITTEN text
        let (off, len) = remap_boundary_span(orig, &a.text, &a.spans, 2, 1);
        // floored start (0) and a length rounded up to the full char
        assert_eq!((off, len), (0, 4));
        assert_eq!(
            &orig[off as usize..off as usize + len as usize],
            "\u{1F600}"
        );
    }

    /// Loop-review catch: engine boundary offsets are BYTE indices
    /// and the crate's callback contract says so — with non-ASCII
    /// text the byte↔char conversion must land the highlight on the
    /// typed word, not mid-`ü`.
    #[test]
    fn boundary_remap_is_byte_unit_for_non_ascii() {
        let p = entries("botox = bow tocks\n");
        let orig = "Grüße, Botox end";
        let a = p.apply(orig, Policy::AllRespell);
        // "Botox" starts at BYTE 9 in the original (ü is 2 bytes)
        // and in the rewritten text too (prefix unchanged)
        let (off, len) = remap_boundary_span(orig, &a.text, &a.spans, 9, 5);
        assert_eq!(
            &orig[off as usize..(off + len) as usize],
            "Botox",
            "maps to the typed word's BYTE span"
        );
        // a boundary after the replacement shifts back by the delta
        // in BYTE terms
        let (off2, _) = remap_boundary_span(orig, &a.text, &a.spans, 18, 3);
        assert_eq!(&orig[off2 as usize..], " end");
    }

    /// Loop-review catch: a key matching a strict prefix of a
    /// multi-char fold (İ → i + U+0307) used to re-emit the original
    /// character once more.
    #[test]
    fn multi_char_folds_are_consumed_once() {
        let p = entries("i = eye\n");
        let a = p.apply("Say İ now", Policy::AllRespell);
        assert_eq!(a.text, "Say eye now");
    }

    /// Keys fold per-char, exactly as the scan folds text (final
    /// sigma: Σ → σ at word end too).
    #[test]
    fn greek_sigma_keys_match() {
        let p = entries("καφές = καφέσ\n");
        let a = p.apply("ΚΑΦΈΣ", Policy::AllRespell);
        assert_eq!(a.text, "καφέσ");
    }

    #[test]
    fn skips_malformed_lines() {
        let (p, skipped) = parse("# comment\n\nno equals sign\n=x\nk =\nok = fine\n");
        assert_eq!(skipped, 3);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn unicode_keys_fold_case() {
        let p = entries("Grüße = grueze\n");
        let a = p.apply("Grüße dir, grüße allen", Policy::AllRespell);
        assert_eq!(a.text, "grueze dir, grueze allen");
    }

    #[test]
    fn phoneme_lookup_is_single_word_only() {
        let p = entries("san rafael = /x/\nbotox = bow tocks\nbotox = /y/\n");
        assert_eq!(p.phoneme_for("san rafael"), None); // phrase: not per-token
        assert_eq!(p.phoneme_for("BOTOX"), Some("y")); // case-folded
        assert_eq!(p.phoneme_for("elbow"), None);
    }

    /// The global singleton + env resolution need the process-wide
    /// env lock (same discipline as TTS_MODELS_DIR tests).
    fn env_locked() -> std::sync::MutexGuard<'static, ()> {
        crate::local_models::test_util::env_lock()
    }

    #[test]
    fn active_loads_env_file_and_reloads_on_mtime() {
        let _env = env_locked();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pron.txt");
        std::fs::write(&file, "botox = bow tocks\n").unwrap();
        // point the global at the temp file via the frontend setter
        // (no env mutation needed — and the setter is the ABI path)
        set_file(Some(file.clone()));
        let a = crate::pronunciation::active().expect("loaded");
        assert_eq!(a.len(), 1);
        let applied = a.apply("Botox now", Policy::AllRespell);
        assert_eq!(applied.text, "bow tocks now");

        // edit the file: mtime moves, next utterance sees it
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, "botox = bah tocks\nzed = zed\n").unwrap();
        let b = crate::pronunciation::active().expect("reloaded");
        assert_eq!(b.len(), 2);
        assert_eq!(
            b.apply("Botox now", Policy::AllRespell).text,
            "bah tocks now"
        );

        // missing file: no overrides, speech never fails
        set_file(Some(dir.path().join("absent.txt")));
        assert!(crate::pronunciation::active().is_none());
        // back to default resolution
        set_file(None);
    }

    #[test]
    fn default_path_lives_under_home() {
        let _env = env_locked();
        let saved_home = std::env::var_os("HOME");
        std::env::set_var("HOME", "/tmp/opencode/fakehome");
        std::env::remove_var("RUST_TTS_PRONUNCIATIONS");
        let p = default_path();
        assert_eq!(
            p,
            std::path::PathBuf::from("/tmp/opencode/fakehome")
                .join(".rust-tts-wrapper")
                .join("pronunciations.txt")
        );
        // USERPROFILE fallback when HOME is unset (the #123 shape)
        std::env::remove_var("HOME");
        std::env::set_var("USERPROFILE", "/tmp/opencode/fakeprof");
        assert_eq!(
            default_path(),
            std::path::PathBuf::from("/tmp/opencode/fakeprof")
                .join(".rust-tts-wrapper")
                .join("pronunciations.txt")
        );
        std::env::remove_var("USERPROFILE");
        match saved_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn active_snapshot_phoneme_lookup() {
        let _env = env_locked();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pron.txt");
        std::fs::write(&file, "botox = bow tocks\nbotox = /ˈboʊ.tɑːks/\n").unwrap();
        set_file(Some(file));
        let p = active().expect("loaded");
        assert_eq!(p.phoneme_for("BOTOX"), Some("ˈboʊ.tɑːks"));
        set_file(None);
    }

    #[test]
    fn no_entries_is_identity() {
        let p = Pronunciations::default();
        let a = p.apply("anything at all", Policy::AllRespell);
        assert_eq!(a.text, "anything at all");
        assert!(a.spans.is_empty(), "identity keeps spans empty");
    }
}
