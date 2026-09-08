//! Playback timeline: map playback wall-clock time to a text position.
//!
//! Reader-style consumers (audiobook readers, sentence highlighters) need the
//! inverse of boundary events: "given that playback started N seconds ago,
//! which word / character is being spoken right now?" — queried repeatedly
//! against a static timeline while audio plays, with pause and seek support
//! and no re-synthesis. This module turns the boundary data the engines
//! already produce (`Vec<WordBoundary>`, or the
//! `(word, start, end, char_offset, char_len, estimated)` callback tuples)
//! into that timeline.
//!
//! Semantics follow the pattern popularized by terminal reader projects
//! (e.g. readaloud): events are keyed in *playback* time — audio seconds
//! divided by the playback speed factor, compensating a tempo filter such as
//! ffmpeg's `atempo` — and lookups hold the last event at or before the
//! elapsed time rather than failing on gaps.
//!
//! ```
//! use rust_tts_wrapper::timeline::{PlaybackClock, PlaybackTimeline};
//!
//! // Boundaries captured from a speak() callback, played at 1.25x:
//! // (word, start_s, end_s, char_offset, char_len, estimated)
//! let events = vec![
//!     ("Hello".to_string(), 0.0, 0.4, 0, 5, false),
//!     ("world".to_string(), 0.5, 0.9, 6, 5, false),
//! ];
//! let timeline = PlaybackTimeline::from_boundary_events(&events, 1.25);
//! assert_eq!(timeline.char_offset_at(0.0), 0);
//! // 0.5s of audio takes 0.4s of playback at 1.25x:
//! assert_eq!(timeline.char_offset_at(0.45), 6);
//!
//! let mut clock = PlaybackClock::start(1.25);
//! // ... after ~0.45s of playing (minus pauses):
//! // timeline.char_offset_at(clock.elapsed_playback_secs())
//! ```

use std::time::Instant;

use crate::types::WordBoundary;

/// One word event on the playback timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    /// When the word starts, in playback seconds (audio time ÷ speed).
    pub playback_start: f32,
    /// When the word ends, in playback seconds.
    pub playback_end: f32,
    /// The spoken word text.
    pub word: String,
    /// Byte offset of the word in the source text, or -1 when unknown.
    pub char_offset: i32,
    /// Character length of the word, or -1 when unknown.
    pub char_len: i32,
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
    /// `(word, start_secs, end_secs, char_offset, char_len, estimated)`.
    ///
    /// `speed` is the playback speed factor (1.0 = normal, 1.25 = 25%
    /// faster playback via a tempo filter): audio timestamps are divided
    /// by it so the timeline is keyed in playback wall-clock time.
    #[must_use]
    pub fn from_boundary_events(events: &[(String, f32, f32, i32, i32, bool)], speed: f32) -> Self {
        let speed = if speed > 0.0 { speed } else { 1.0 };
        let mut entries: Vec<TimelineEntry> = events
            .iter()
            .map(|(word, start, end, off, len, est)| TimelineEntry {
                playback_start: start / speed,
                playback_end: end / speed,
                word: word.clone(),
                char_offset: *off,
                char_len: *len,
                estimated: *est,
            })
            .collect();
        entries.sort_by(|a, b| {
            a.playback_start
                .partial_cmp(&b.playback_start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self { entries }
    }

    /// Build from `Vec<WordBoundary>` (as returned by
    /// `synth_with_boundaries`), recovering each word's byte offset in
    /// `text` by scanning forward — case-insensitively on a miss, and
    /// holding the last known position when a word cannot be found at all
    /// (punctuation artifacts, normalization differences). This mirrors
    /// what the engine-side boundary searches do, on the consumer side.
    #[must_use]
    pub fn from_boundaries(boundaries: &[WordBoundary], text: &str, speed: f32) -> Self {
        let speed = if speed > 0.0 { speed } else { 1.0 };
        let lower_text = text.to_lowercase();
        let mut entries = Vec::with_capacity(boundaries.len());
        let mut search_pos = 0usize;
        for b in boundaries {
            #[allow(clippy::cast_precision_loss)] // ms timestamps; f32 precision is ample
            let start_s = b.offset as f32 / 1000.0;
            #[allow(clippy::cast_precision_loss)]
            let end_s = (b.offset + b.duration) as f32 / 1000.0;
            let (char_offset, char_len) = if b.text.is_empty() {
                (-1, -1)
            } else {
                let mut idx = text[search_pos.min(text.len())..]
                    .find(&b.text)
                    .map_or(-1, |pos| (search_pos + pos) as i32);
                if idx < 0 {
                    idx = lower_text[search_pos.min(lower_text.len())..]
                        .find(&b.text.to_lowercase())
                        .map_or(-1, |pos| (search_pos + pos) as i32);
                }
                if idx >= 0 {
                    search_pos = idx as usize + b.text.len();
                    (idx, b.text.chars().count() as i32)
                } else {
                    // Word not found: hold the last known position.
                    (search_pos.min(i32::MAX as usize) as i32, -1)
                }
            };
            entries.push(TimelineEntry {
                playback_start: start_s / speed,
                playback_end: end_s / speed,
                word: b.text.clone(),
                char_offset,
                char_len,
                estimated: b.estimated,
            });
        }
        Self { entries }
    }

    /// The byte offset into the source text being spoken at
    /// `playback_secs`, holding the last event at or before that time.
    /// Returns 0 before the first event and on an empty timeline.
    #[must_use]
    pub fn char_offset_at(&self, playback_secs: f32) -> i32 {
        self.entry_at(playback_secs)
            .map_or(0, |e| if e.char_offset >= 0 { e.char_offset } else { 0 })
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
        let mut lo = 0usize;
        let mut hi = self.entries.len();
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            if self.entries[mid].playback_start <= playback_secs {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            None
        } else {
            self.entries.get(lo - 1)
        }
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
    /// there. Does not touch the audio player itself.
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
        let t = PlaybackTimeline::from_boundary_events(&events(), 1.0);
        assert_eq!(t.len(), 3);
        assert_eq!(t.char_offset_at(0.0), 0);
        assert_eq!(t.char_offset_at(0.45), 0); // between words
        assert_eq!(t.char_offset_at(0.6), 6);
        assert_eq!(t.char_offset_at(1.49), 15);
        assert_eq!(t.char_offset_at(99.0), 15); // after the end
        assert_eq!(t.word_at(0.6).unwrap().word, "playback");
    }

    #[test]
    fn before_first_event_returns_zero() {
        let t = PlaybackTimeline::from_boundary_events(&events(), 1.0);
        assert_eq!(t.char_offset_at(-1.0), 0);
        assert!(t.word_at(-1.0).is_none());
    }

    #[test]
    fn speed_divides_audio_time() {
        // At 2x playback, 1.0s of audio has played by wall second 0.5:
        // word starts 0.0 / 0.5 / 1.1 (audio) become 0.0 / 0.25 / 0.55.
        let t = PlaybackTimeline::from_boundary_events(&events(), 2.0);
        assert_eq!(t.char_offset_at(0.24), 0);
        assert_eq!(t.char_offset_at(0.25), 6);
        assert_eq!(t.char_offset_at(0.55), 15);
    }

    #[test]
    fn nonpositive_speed_falls_back_to_one() {
        let t = PlaybackTimeline::from_boundary_events(&events(), 0.0);
        assert_eq!(t.char_offset_at(0.6), 6);
    }

    #[test]
    fn unsorted_input_is_sorted() {
        let mut ev = events();
        ev.reverse();
        let t = PlaybackTimeline::from_boundary_events(&ev, 1.0);
        assert_eq!(t.entries()[0].word, "Hello");
        assert_eq!(t.char_offset_at(1.2), 15);
    }

    #[test]
    fn empty_timeline_is_inert() {
        let t = PlaybackTimeline::from_boundary_events(&[], 1.0);
        assert!(t.is_empty());
        assert_eq!(t.char_offset_at(10.0), 0);
        assert!(t.word_at(10.0).is_none());
    }

    #[test]
    fn from_boundaries_recovers_offsets_by_scanning() {
        let words = vec![
            WordBoundary {
                text: "Hello".into(),
                offset: 0,
                duration: 400,
                estimated: false,
            },
            WordBoundary {
                text: "world".into(),
                offset: 500,
                duration: 400,
                estimated: false,
            },
        ];
        let t = PlaybackTimeline::from_boundaries(&words, "Say Hello, world!", 1.0);
        assert_eq!(t.entries()[0].char_offset, 4); // "Hello"
        assert_eq!(t.entries()[1].char_offset, 11); // "world"
        assert_eq!(t.char_offset_at(0.55), 11);
    }

    #[test]
    fn from_boundaries_case_insensitive_and_hold_last() {
        let words = vec![
            WordBoundary {
                text: "HELLO".into(),
                offset: 0,
                duration: 300,
                estimated: true,
            },
            WordBoundary {
                text: "xyzzy".into(), // not in the text at all
                offset: 400,
                duration: 300,
                estimated: true,
            },
        ];
        let t = PlaybackTimeline::from_boundaries(&words, "hello there", 1.0);
        assert_eq!(t.entries()[0].char_offset, 0); // case-insensitive match
        assert_eq!(t.entries()[1].char_offset, 5); // holds last position
        assert!(t.entries()[1].char_len < 0);
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
    fn clock_seek_rebases() {
        let mut clock = PlaybackClock::start(2.0);
        std::thread::sleep(std::time::Duration::from_millis(30));
        clock.seek(10.0);
        let e = clock.elapsed_playback_secs();
        assert!((10.0..10.5).contains(&e), "after seek {e}");
        assert!((clock.speed() - 2.0).abs() < f32::EPSILON);
    }
}
