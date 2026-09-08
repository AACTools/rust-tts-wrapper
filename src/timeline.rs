//! Playback timeline: map playback wall-clock time to a text position.
//!
//! Reader-style consumers (audiobook readers, sentence highlighters) need the
//! inverse of boundary events: "given that playback started N seconds ago,
//! which word / character is being spoken right now?" — queried repeatedly
//! against a static timeline while audio plays, with pause and seek support
//! and no re-synthesis. This module turns the boundary data the engines
//! already produce (`Vec<WordBoundary>`, or the
//! `(word, start, end, byte_offset, len, estimated)` callback tuples)
//! into that timeline.
//!
//! Semantics follow the pattern popularized by terminal reader projects
//! (e.g. readaloud): events are keyed in *playback* time — audio seconds
//! divided by the playback speed factor, compensating a tempo filter such as
//! ffmpeg's `atempo` — and lookups hold the last event at or before the
//! elapsed time rather than failing on gaps. Timings are kept as `f32`:
//! at playback lengths of a day the resolution is still ~5 ms, ample for
//! word-level highlighting.
//!
//! ```
//! use rust_tts_wrapper::timeline::{PlaybackClock, PlaybackTimeline};
//!
//! // Boundary-callback events (word, start_s, end_s, byte_offset, len, est),
//! // played at 1.25x through a tempo filter:
//! let events = vec![
//!     ("Hello".to_string(), 0.0, 0.4, 0, 5, false),
//!     ("world".to_string(), 0.5, 0.9, 6, 5, false),
//! ];
//! let timeline = PlaybackTimeline::from_callback_events(&events, 1.25);
//! assert_eq!(timeline.byte_offset_at(0.0), 0);
//! // 0.5s of audio takes 0.4s of playback at 1.25x:
//! assert_eq!(timeline.byte_offset_at(0.45), 6);
//!
//! let mut clock = PlaybackClock::start(1.25);
//! // ... after ~0.45s of playing (minus pauses):
//! // timeline.byte_offset_at(clock.elapsed_playback_secs())
//! ```

use std::time::Instant;

use crate::types::WordBoundary;

/// One word event on the playback timeline.
///
/// Units: `byte_offset` is a byte index into the source text (matching the
/// boundary-callback convention), `byte_len` a byte length. Both are -1 when
/// unknown. Slice with `&text[byte_offset..byte_offset + byte_len]` only
/// when both are non-negative.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    /// When the word starts, in playback seconds (audio time ÷ speed).
    pub playback_start: f32,
    /// When the word ends, in playback seconds.
    pub playback_end: f32,
    /// The spoken word text.
    pub word: String,
    /// Byte offset of the word in the source text, or -1 when unknown.
    pub byte_offset: i32,
    /// Byte length of the word, or -1 when unknown. When built via
    /// [`PlaybackTimeline::from_callback_events`] this is the length the
    /// caller supplied (the engine callbacks currently report a character
    /// count — a known inconsistency tracked in the repo TODO; when built
    /// via [`PlaybackTimeline::from_word_boundaries`] it is the matched
    /// slice's byte length).
    pub byte_len: i32,
    /// Whether the timings are estimates rather than measurements.
    pub estimated: bool,
}

/// A sorted `(playback_time → position)` timeline over word events.
#[derive(Debug, Clone, Default)]
pub struct PlaybackTimeline {
    entries: Vec<TimelineEntry>,
}

impl PlaybackTimeline {
    /// Build from boundary-callback tuples as delivered by the engines:
    /// `(word, start_secs, end_secs, byte_offset, len, estimated)`.
    ///
    /// `speed` is the playback speed factor (1.0 = normal, 1.25 = 25%
    /// faster playback via a tempo filter): audio timestamps are divided
    /// by it so the timeline is keyed in playback wall-clock time.
    /// Non-positive speeds fall back to 1.0.
    #[must_use]
    pub fn from_callback_events(events: &[(String, f32, f32, i32, i32, bool)], speed: f32) -> Self {
        let speed = if speed > 0.0 { speed } else { 1.0 };
        let mut entries: Vec<TimelineEntry> = events
            .iter()
            .map(|(word, start, end, off, len, est)| TimelineEntry {
                playback_start: start / speed,
                playback_end: end / speed,
                word: word.clone(),
                byte_offset: *off,
                byte_len: *len,
                estimated: *est,
            })
            .collect();
        // Ties on start resolve to the entry that ends last.
        entries.sort_by(|a, b| {
            a.playback_start
                .partial_cmp(&b.playback_start)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.playback_end
                        .partial_cmp(&b.playback_end)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        Self { entries }
    }

    /// Build from `Vec<WordBoundary>` (as returned by
    /// `synth_with_boundaries`), recovering each word's byte offset in
    /// `text` by scanning forward — case-insensitively on a miss, and
    /// holding the last known position when a word cannot be found at all
    /// (punctuation artifacts, normalization differences).
    ///
    /// # Panics (debug builds only)
    /// Panics via `debug_assert!` when the boundaries are not in
    /// non-decreasing audio-offset order — the forward scan assumes
    /// spoken order. Engine output always is.
    #[must_use]
    pub fn from_word_boundaries(boundaries: &[WordBoundary], text: &str, speed: f32) -> Self {
        let speed = if speed > 0.0 { speed } else { 1.0 };

        // Lowercased view of `text` where every produced char maps back to
        // the byte offset of the *source* char it came from. Case folding
        // can change byte length (e.g. 'İ' is 2 bytes, folds to 3), so
        // offsets must never be taken from a separately-lowercased string.
        let lowered: Vec<(usize, char)> = text
            .char_indices()
            .flat_map(|(i, c)| c.to_lowercase().map(move |lc| (i, lc)))
            .collect();
        let find_lowered = |from: usize, needle: &[char]| -> Option<(usize, usize)> {
            if needle.is_empty() || lowered.len() < needle.len() {
                return None;
            }
            let mut k = from.min(lowered.len() - needle.len() + 1);
            while k + needle.len() <= lowered.len() {
                if lowered[k..k + needle.len()]
                    .iter()
                    .map(|(_, c)| *c)
                    .eq(needle.iter().copied())
                {
                    // Byte offset of the first source char, and the
                    // produced-index just past the match.
                    return Some((lowered[k].0, k + needle.len()));
                }
                k += 1;
            }
            None
        };

        let mut entries = Vec::with_capacity(boundaries.len());
        let mut cursor = 0usize; // produced-index scan cursor
        let mut last_known = -1i32; // last matched byte offset
        let mut prev_offset = 0u64;
        for b in boundaries {
            debug_assert!(
                b.offset >= prev_offset,
                "boundaries must be in non-decreasing audio order"
            );
            prev_offset = b.offset;
            #[allow(clippy::cast_precision_loss)] // ms timestamps; f32 is ample
            let start_s = b.offset as f32 / 1000.0;
            #[allow(clippy::cast_precision_loss)]
            let end_s = (b.offset + b.duration) as f32 / 1000.0;

            let needle: Vec<char> = b.text.to_lowercase().chars().collect();
            let (byte_offset, byte_len) = if needle.is_empty() {
                (last_known, -1)
            } else {
                // Case-sensitive match first (cheapest, byte-exact).
                let exact = text[cursor_byte(text, &lowered, cursor)..]
                    .find(&b.text)
                    .map(|pos| {
                        let abs = cursor_byte(text, &lowered, cursor) + pos;
                        (abs as i32, b.text.len() as i32)
                    });
                if let Some(hit) = exact {
                    last_known = hit.0;
                    cursor = produced_index_after(&lowered, hit.0 as usize + b.text.len());
                    hit
                } else if let Some((abs, next)) = find_lowered(cursor, &needle) {
                    last_known = abs as i32;
                    let matched_bytes = match lowered.get(next - 1) {
                        Some(&(last_src, _)) => next_char_start(text, last_src) - abs,
                        None => b.text.len(),
                    };
                    cursor = next;
                    (abs as i32, matched_bytes as i32)
                } else {
                    // Word not found: hold the last known position.
                    (last_known, -1)
                }
            };
            entries.push(TimelineEntry {
                playback_start: start_s / speed,
                playback_end: end_s / speed,
                word: b.text.clone(),
                byte_offset,
                byte_len,
                estimated: b.estimated,
            });
        }
        Self { entries }
    }

    /// The byte offset into the source text being spoken at
    /// `playback_secs`, holding the last event at or before that time —
    /// including holding the last *known* offset across entries whose
    /// offset is unknown (-1). Returns 0 before the first event and on an
    /// empty timeline.
    #[must_use]
    pub fn byte_offset_at(&self, playback_secs: f32) -> i32 {
        let idx = self
            .entries
            .partition_point(|e| e.playback_start <= playback_secs);
        self.entries[..idx]
            .iter()
            .rev()
            .find(|e| e.byte_offset >= 0)
            .map_or(0, |e| e.byte_offset)
    }

    /// The word being spoken at `playback_secs`, if any has been
    /// scheduled by then.
    #[must_use]
    pub fn word_at(&self, playback_secs: f32) -> Option<&TimelineEntry> {
        self.entry_at(playback_secs)
    }

    /// All entries, in playback order.
    #[must_use]
    pub fn entries(&self) -> &[TimelineEntry] {
        &self.entries
    }

    /// Number of word events on the timeline.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Last event at or before `playback_secs` (binary search).
    fn entry_at(&self, playback_secs: f32) -> Option<&TimelineEntry> {
        let idx = self
            .entries
            .partition_point(|e| e.playback_start <= playback_secs);
        if idx == 0 {
            None
        } else {
            self.entries.get(idx - 1)
        }
    }
}

/// Byte offset in `text` for a produced-index cursor into `lowered`.
fn cursor_byte(text: &str, lowered: &[(usize, char)], cursor: usize) -> usize {
    match lowered.get(cursor) {
        Some(&(b, _)) => b,
        None => text.len(),
    }
}

/// Produced-index of the first lowered char whose source byte offset is
/// at or after `byte` (the scan position after a match ending at `byte`).
fn produced_index_after(lowered: &[(usize, char)], byte: usize) -> usize {
    lowered.partition_point(|(b, _)| *b < byte)
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

/// Wall-clock playback clock with pause accounting, for driving lookups
/// into a [`PlaybackTimeline`] while audio plays through an external
/// player (the caller owns the audio device; this is just the stopwatch).
#[derive(Debug)]
pub struct PlaybackClock {
    speed: f32,
    epoch: Instant,
    base_secs: f32,
    paused_total: std::time::Duration,
    pause_started: Option<Instant>,
}

impl PlaybackClock {
    /// Start counting playback time now. `speed` is informational here —
    /// the clock counts playback wall time (post-tempo); pair it with a
    /// timeline built with the same speed.
    #[must_use]
    pub fn start(speed: f32) -> Self {
        Self {
            speed: if speed > 0.0 { speed } else { 1.0 },
            epoch: Instant::now(),
            base_secs: 0.0,
            paused_total: std::time::Duration::ZERO,
            pause_started: None,
        }
    }

    /// The playback speed factor this clock was started with.
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Pause the clock. Pausing while already paused is a no-op.
    pub fn pause(&mut self) {
        if self.pause_started.is_none() {
            self.pause_started = Some(Instant::now());
        }
    }

    /// Resume the clock. Resuming while playing is a no-op.
    pub fn resume(&mut self) {
        if let Some(at) = self.pause_started.take() {
            self.paused_total += at.elapsed();
        }
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.pause_started.is_some()
    }

    /// Elapsed playback seconds since start (or the last
    /// [`seek`](Self::seek)), excluding paused time.
    #[must_use]
    pub fn elapsed_playback_secs(&self) -> f32 {
        let mut elapsed = self.epoch.elapsed();
        if let Some(at) = self.pause_started {
            // Currently paused: count only up to the pause moment.
            elapsed = elapsed.saturating_sub(at.elapsed());
        }
        self.base_secs + elapsed.as_secs_f32() - self.paused_total.as_secs_f32()
    }

    /// Move the clock to `playback_secs` (e.g. after seeking the audio
    /// player with `-ss`): subsequent elapsed readings continue from
    /// there. Negative values clamp to 0. Does not touch the audio
    /// player itself; a paused clock stays paused at the new position.
    pub fn seek(&mut self, playback_secs: f32) {
        self.epoch = Instant::now();
        self.base_secs = playback_secs.max(0.0);
        self.paused_total = std::time::Duration::ZERO;
        if self.pause_started.is_some() {
            self.pause_started = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events() -> Vec<(String, f32, f32, i32, i32, bool)> {
        vec![
            ("Hello".into(), 0.0, 0.40, 0, 5, false),
            ("playback".into(), 0.50, 1.00, 6, 8, false),
            ("world".into(), 1.10, 1.50, 15, 5, false),
        ]
    }

    #[test]
    fn lookup_holds_last_event() {
        let t = PlaybackTimeline::from_callback_events(&events(), 1.0);
        assert_eq!(t.len(), 3);
        assert_eq!(t.byte_offset_at(0.0), 0);
        assert_eq!(t.byte_offset_at(0.45), 0); // between words
        assert_eq!(t.byte_offset_at(0.6), 6);
        assert_eq!(t.byte_offset_at(1.49), 15);
        assert_eq!(t.byte_offset_at(99.0), 15); // after the end
        assert_eq!(t.word_at(0.6).unwrap().word, "playback");
    }

    #[test]
    fn before_first_event_returns_zero() {
        let t = PlaybackTimeline::from_callback_events(&events(), 1.0);
        assert_eq!(t.byte_offset_at(-1.0), 0);
        assert!(t.word_at(-1.0).is_none());
    }

    #[test]
    fn speed_divides_audio_time() {
        // At 2x playback, 1.0s of audio has played by wall second 0.5:
        // word starts 0.0 / 0.5 / 1.1 (audio) become 0.0 / 0.25 / 0.55.
        let t = PlaybackTimeline::from_callback_events(&events(), 2.0);
        assert_eq!(t.byte_offset_at(0.24), 0);
        assert_eq!(t.byte_offset_at(0.25), 6);
        assert_eq!(t.byte_offset_at(0.55), 15);
    }

    #[test]
    fn nonpositive_speed_falls_back_to_one() {
        let t = PlaybackTimeline::from_callback_events(&events(), 0.0);
        assert_eq!(t.byte_offset_at(0.6), 6);
    }

    #[test]
    fn unsorted_input_is_sorted_with_end_time_tiebreak() {
        let mut ev = events();
        // Same start as "playback" (0.5) but shorter end: must sort first.
        ev.push(("hi".into(), 0.50, 0.60, 24, 2, false));
        ev.reverse();
        let t = PlaybackTimeline::from_callback_events(&ev, 1.0);
        assert_eq!(t.entries()[0].word, "Hello");
        let tie = t
            .entries()
            .iter()
            .filter(|e| (e.playback_start - 0.5).abs() < f32::EPSILON)
            .collect::<Vec<_>>();
        assert_eq!(tie.len(), 2);
        assert_eq!(tie[0].word, "hi"); // earlier end wins the tie
        assert_eq!(tie[1].word, "playback");
    }

    #[test]
    fn duplicate_starts_hold_longest_end() {
        let ev = vec![
            ("a".to_string(), 1.0, 1.5, 10, 1, false),
            ("b".to_string(), 1.0, 1.9, 20, 1, false),
        ];
        let t = PlaybackTimeline::from_callback_events(&ev, 1.0);
        // Both start at 1.0; at 1.6 only b is "current" by our
        // last-at-or-before rule, which returns the final entry at that
        // time — b, the longer one, matching the sort tiebreak.
        assert_eq!(t.word_at(1.6).unwrap().word, "b");
    }

    #[test]
    fn empty_timeline_is_inert() {
        let t = PlaybackTimeline::from_callback_events(&[], 1.0);
        assert!(t.is_empty());
        assert_eq!(t.byte_offset_at(10.0), 0);
        assert!(t.word_at(10.0).is_none());
    }

    fn wb(text: &str, offset: u64, duration: u64) -> WordBoundary {
        WordBoundary {
            text: text.into(),
            offset,
            duration,
            estimated: false,
        }
    }

    #[test]
    fn from_word_boundaries_recovers_offsets_by_scanning() {
        let words = vec![wb("Hello", 0, 400), wb("world", 500, 400)];
        let t = PlaybackTimeline::from_word_boundaries(&words, "Say Hello, world!", 1.0);
        assert_eq!(t.entries()[0].byte_offset, 4); // "Hello"
        assert_eq!(t.entries()[1].byte_offset, 11); // "world"
        assert_eq!(t.entries()[0].byte_len, 5);
        assert_eq!(t.byte_offset_at(0.55), 11);
    }

    #[test]
    fn from_word_boundaries_case_insensitive_and_hold_last() {
        let words = vec![wb("HELLO", 0, 300), wb("xyzzy", 400, 300)];
        let t = PlaybackTimeline::from_word_boundaries(&words, "hello there", 1.0);
        assert_eq!(t.entries()[0].byte_offset, 0); // case-insensitive match
                                                   // Miss holds the last KNOWN offset; the length is unknown.
        assert_eq!(t.entries()[1].byte_offset, 0);
        assert!(t.entries()[1].byte_len < 0);
        assert_eq!(t.byte_offset_at(0.45), 0);
    }

    #[test]
    fn multibyte_text_never_panics_and_offsets_are_byte_true() {
        let text = "İstanbul çağırdı";
        let words = vec![wb("İstanbul", 0, 600), wb("çağırdı", 700, 600)];
        let t = PlaybackTimeline::from_word_boundaries(&words, text, 1.0);
        // "İstanbul" is 9 bytes (İ is 2); exact match is byte-true.
        assert_eq!(t.entries()[0].byte_offset, 0);
        assert_eq!(t.entries()[0].byte_len, 9);
        assert_eq!(&text[0..9], "İstanbul");
        assert_eq!(t.entries()[1].byte_offset, 10);
        // çağırdı = 11 bytes (ç, ğ and the dotless ı are 2 bytes each).
        assert_eq!(t.entries()[1].byte_len, 11);
        assert_eq!(&text[10..21], "çağırdı");
    }

    #[test]
    fn turkish_dotted_i_repro_no_panic_and_correct_offsets() {
        // The review-round repro: engine-normalized lowercase words against
        // text starting with İ (U+0130, 2 bytes; lowercases to 3 bytes incl.
        // a combining dot). Must not panic, and offsets must come from the
        // original text's byte space.
        let text = "İü abc";
        let words = vec![wb("i", 0, 200), wb("abc", 300, 400)];
        let t = PlaybackTimeline::from_word_boundaries(&words, text, 1.0);
        // "i" matches the lowercase expansion of İ: source byte 0, and the
        // matched span covers the whole source char (2 bytes).
        assert_eq!(t.entries()[0].byte_offset, 0);
        assert_eq!(t.entries()[0].byte_len, 2);
        // "abc" resolves in the original text's byte space: byte 5.
        assert_eq!(t.entries()[1].byte_offset, 5);
        assert_eq!(&text[5..8], "abc");
    }

    #[test]
    fn kelvin_sign_case_folding_shrinks() {
        // Kelvin sign K (3 bytes) lowercases to k (1 byte) — divergence in
        // the other direction.
        let text = "\u{212A}epler";
        let words = vec![wb("kepler", 0, 500)];
        let t = PlaybackTimeline::from_word_boundaries(&words, text, 1.0);
        assert_eq!(t.entries()[0].byte_offset, 0);
        assert_eq!(t.entries()[0].byte_len, 3 + "epler".len() as i32);
    }

    #[test]
    fn clock_counts_playback_and_pauses() {
        let mut clock = PlaybackClock::start(1.0);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let before = clock.elapsed_playback_secs();
        assert!(before >= 0.04, "elapsed {before}");
        assert!(!clock.is_paused());

        clock.pause();
        assert!(clock.is_paused());
        std::thread::sleep(std::time::Duration::from_millis(120));
        let paused = clock.elapsed_playback_secs();
        // Paused time is excluded — only tiny scheduling drift allowed.
        assert!(paused - before < 0.05, "paused drift {}", paused - before);

        clock.resume();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(clock.elapsed_playback_secs() >= paused, "resumes counting");
    }

    #[test]
    fn clock_seek_rebases_and_clamps_negative() {
        let mut clock = PlaybackClock::start(2.0);
        std::thread::sleep(std::time::Duration::from_millis(30));
        clock.seek(10.0);
        let e = clock.elapsed_playback_secs();
        assert!((10.0..10.5).contains(&e), "after seek {e}");
        assert!((clock.speed() - 2.0).abs() < f32::EPSILON);

        clock.seek(-5.0);
        assert!(clock.elapsed_playback_secs() < 0.5, "negative seek clamps");
    }

    #[test]
    fn clock_seek_while_paused_holds_position() {
        let mut clock = PlaybackClock::start(1.0);
        clock.pause();
        clock.seek(3.0);
        assert!(clock.is_paused());
        // Paused at 3.0: elapsed excludes ongoing pause.
        let e = clock.elapsed_playback_secs();
        assert!((2.95..3.05).contains(&e), "paused at seek target: {e}");
        std::thread::sleep(std::time::Duration::from_millis(80));
        let e2 = clock.elapsed_playback_secs();
        assert!(e2 - e < 0.05, "still frozen: drift {}", e2 - e);
    }
}
