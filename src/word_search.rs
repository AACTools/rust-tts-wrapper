//! Incremental forward word search over a fixed text, for mapping spoken
//! words back to text positions.
//!
//! Engines deliver word events during synthesis (`WordBoundary` metadata,
//! alignment arrays, estimated plans) whose word text rarely matches the
//! source text exactly — case differs, accents are normalized, punctuation
//! is stripped. This provides one shared, byte-safe matcher for all the
//! places that recover a position: the ElevenLabs alignment loop, the
//! Google estimate fallback, the `timeline` module, and any consumer-side
//! scanner.
//!
//! Matching is: exact (byte-true) → case-insensitive with combining
//! diacritics (U+0300..=U+036F) ignored on both sides → miss. On a miss
//! the scan holds the last known offset and reports length -1, so
//! consumers can keep highlighting at the last position instead of
//! jumping. All offsets are byte indices into the original text and are
//! always derived from `char_indices()` — never from a separately
//! re-cased string (whose byte length can diverge: 'İ' is 2 bytes but
//! lowercases to 3).

/// Incremental forward word search with a cursor over a fixed text.
#[derive(Debug)]
pub struct WordSearch<'a> {
    text: &'a str,
    /// Lowercased view with combining marks removed; each produced char
    /// maps back to its source char's byte offset.
    lowered: Vec<(usize, char)>,
    /// Produced-index scan cursor.
    cursor: usize,
    /// Last matched byte offset, -1 before the first match.
    last_known: i32,
}

impl<'a> WordSearch<'a> {
    /// Build a search over `text`. Matching is strictly forward-only.
    #[must_use]
    pub fn new(text: &'a str) -> Self {
        let lowered = text
            .char_indices()
            .flat_map(|(i, c)| c.to_lowercase().map(move |lc| (i, lc)))
            .filter(|(_, c)| !is_combining_mark(*c))
            .collect();
        Self {
            text,
            lowered,
            cursor: 0,
            last_known: -1,
        }
    }

    /// Find `word` forward from the cursor. Returns
    /// `(byte_offset, byte_len)` of the match in the source text.
    ///
    /// On a miss (or an empty `word`), returns the held position —
    /// the last known offset (`-1` if nothing has matched yet) with
    /// `byte_len = -1` — without advancing the cursor.
    pub fn find_next(&mut self, word: &str) -> (i32, i32) {
        if word.is_empty() {
            return (self.last_known, -1);
        }

        // Case-sensitive match first: cheapest and byte-exact.
        let from = self.cursor_byte();
        if let Some(pos) = self.text[from..].find(word) {
            let abs = from + pos;
            self.last_known = abs as i32;
            self.cursor = self.produced_index_after(abs + word.len());
            return (abs as i32, word.len() as i32);
        }

        // Case-insensitive, combining-mark-insensitive sequence match.
        let needle: Vec<char> = word
            .to_lowercase()
            .chars()
            .filter(|c| !is_combining_mark(*c))
            .collect();
        if let Some((abs, next)) = self.find_lowered(&needle) {
            self.last_known = abs as i32;
            // The match may end mid-expansion (a needle shorter than the
            // source char's lowercase expansion): advance past the whole
            // source char, not just the consumed produced chars, so the
            // next search does not restart inside this character.
            let last_src = self.lowered[next - 1].0;
            let span_end = next_char_start(self.text, last_src);
            self.cursor = self.produced_index_after(span_end);
            return (abs as i32, (span_end - abs) as i32);
        }

        // Miss: hold the last known position.
        (self.last_known, -1)
    }

    /// The last matched byte offset, or -1 before the first match.
    #[must_use]
    pub fn last_known_offset(&self) -> i32 {
        self.last_known
    }

    /// Byte offset in `text` for the produced-index cursor. Always a
    /// source-char start (from `char_indices`) or `text.len()`, so
    /// slicing from it cannot panic.
    fn cursor_byte(&self) -> usize {
        match self.lowered.get(self.cursor) {
            Some(&(b, _)) => b,
            None => self.text.len(),
        }
    }

    /// Produced-index of the first lowered char whose source byte offset
    /// is at or after `byte`.
    fn produced_index_after(&self, byte: usize) -> usize {
        self.lowered.partition_point(|(b, _)| *b < byte)
    }

    /// Sequence-match `needle` in the lowered view from the cursor.
    /// Returns `(source byte offset, produced index just past the match)`.
    fn find_lowered(&self, needle: &[char]) -> Option<(usize, usize)> {
        if needle.is_empty() || self.lowered.len() < needle.len() {
            return None;
        }
        let mut k = self.cursor.min(self.lowered.len() - needle.len() + 1);
        while k + needle.len() <= self.lowered.len() {
            if self.lowered[k..k + needle.len()]
                .iter()
                .map(|(_, c)| *c)
                .eq(needle.iter().copied())
            {
                return Some((self.lowered[k].0, k + needle.len()));
            }
            k += 1;
        }
        None
    }
}

/// Combining diacritical marks (the common U+0300..=U+036F block),
/// ignored during case-insensitive matching so that accents added by
/// case folding (Turkish İ) and NFD text do not break word matching.
/// Marks outside this block (U+1AB0, U+20D0, Indic) are a documented
/// non-goal.
fn is_combining_mark(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036F}')
}

/// Byte offset of the next char boundary at or after `from` (the char
/// starting at `from`, or the end of the text).
fn next_char_start(text: &str, from: usize) -> usize {
    if from >= text.len() {
        text.len()
    } else {
        let mut i = from + 1;
        while i < text.len() && !text.is_char_boundary(i) {
            i += 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str, words: &[&str]) -> Vec<(i32, i32)> {
        let mut s = WordSearch::new(text);
        words.iter().map(|w| s.find_next(w)).collect()
    }

    #[test]
    fn exact_matches_are_byte_true() {
        let hits = all("Say Hello, world!", &["Say", "Hello,", "world!"]);
        assert_eq!(hits[0], (0, 3));
        assert_eq!(hits[1], (4, 6));
        assert_eq!(hits[2], (11, 6));
    }

    #[test]
    fn case_insensitive_match() {
        let hits = all("hello there", &["HELLO"]);
        assert_eq!(hits[0], (0, 5));
    }

    #[test]
    fn miss_holds_last_known_without_advancing() {
        let mut s = WordSearch::new("one two three");
        assert_eq!(s.find_next("one"), (0, 3));
        assert_eq!(s.find_next("zzz"), (0, -1)); // held
                                                 // Cursor did not advance past the miss:
        assert_eq!(s.find_next("two"), (4, 3));
        assert_eq!(s.last_known_offset(), 4);
    }

    #[test]
    fn first_word_miss_reports_unknown() {
        let mut s = WordSearch::new("abc def");
        assert_eq!(s.find_next("zzz"), (-1, -1));
    }

    #[test]
    fn empty_word_is_a_no_op() {
        let mut s = WordSearch::new("abc");
        assert_eq!(s.find_next(""), (-1, -1));
        assert_eq!(s.find_next("abc"), (0, 3));
    }

    #[test]
    fn word_longer_than_text_misses() {
        assert_eq!(all("ab", &["abcdef"]), vec![(-1, -1)]);
    }

    #[test]
    fn empty_text_misses_everything() {
        assert_eq!(all("", &["a"]), vec![(-1, -1)]);
    }

    #[test]
    fn turkish_dotted_i_whole_word_matches() {
        // İ (2 bytes) lowercases to i + combining dot (3 bytes); the dot
        // is ignored so the normalized word matches the whole source.
        let hits = all("İstanbul çağırdı", &["istanbul"]);
        assert_eq!(hits[0], (0, 9));
    }

    #[test]
    fn turkish_dotted_i_repro_no_panic() {
        // The original repro: normalized lowercase words against İ text.
        let hits = all("İü abc", &["i", "abc"]);
        assert_eq!(hits[0], (0, 2)); // whole İ
        assert_eq!(hits[1], (5, 3)); // in the original byte space
    }

    #[test]
    fn cursor_advances_past_partial_expansion() {
        // Needle "i" consumes one of İ's two produced chars; the next
        // word must still resolve at its true offset.
        let hits = all("İa İa", &["i", "İa"]);
        assert_eq!(hits[0], (0, 2));
        assert_eq!(hits[1], (4, 3));
    }

    #[test]
    fn kelvin_sign_case_folding() {
        // Kelvin sign K (3 bytes) lowercases to k (1 byte).
        let hits = all("\u{212A}epler", &["kepler"]);
        assert_eq!(hits[0], (0, 8));
    }

    #[test]
    fn composed_accent_needle_vs_nfd_text_misses() {
        // Documented limitation: composed 'é' vs NFD 'e' + U+0301 text.
        let hits = all("cafe\u{0301} tonight", &["café", "tonight"]);
        assert_eq!(hits[0], (-1, -1));
        assert_eq!(hits[1], (7, 7));
    }

    #[test]
    fn multibyte_exact_spans_are_byte_lengths() {
        let hits = all("İstanbul çağırdı", &["İstanbul", "çağırdı"]);
        assert_eq!(hits[0], (0, 9));
        assert_eq!(hits[1], (10, 11)); // ç, ğ, dotless ı are 2 bytes each
    }

    #[test]
    fn consecutive_misses_hold_without_advancing() {
        let mut s = WordSearch::new("alpha beta gamma");
        assert_eq!(s.find_next("alpha"), (0, 5));
        assert_eq!(s.find_next("xx"), (0, -1));
        assert_eq!(s.find_next("yy"), (0, -1));
        assert_eq!(s.find_next("beta"), (6, 4));
    }

    #[test]
    fn lowered_match_at_text_end_leaves_sane_cursor() {
        let mut s = WordSearch::new("Say HELLO");
        assert_eq!(s.find_next("hello"), (4, 5));
        // Cursor at end: further searches miss without panicking.
        assert_eq!(s.find_next("anything"), (4, -1));
    }

    #[test]
    fn emoji_and_zwj_text_is_safe() {
        let text = "👍🏽 ok";
        let hits = all(text, &["ok"]);
        assert_eq!(hits[0], (9, 2)); // emoji+modifier = 8 bytes, space = 1
    }
}
