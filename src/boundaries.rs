//! Progressive word-boundary firing, shared by the cloud and sherpa-onnx
//! engines.
//!
//! Engines without real API timing data get 150-wpm estimates. Firing
//! them in one batch after synthesis leaves callers that interleave marks
//! with playback (e.g. the VoiceGarden-SPD speech-dispatcher module)
//! unable to highlight in sync on long utterances. [`EstimateFirer`]
//! anchors the estimates onto the delivered-audio clock instead: estimate
//! *i* fires once ≥ its start-time worth of PCM has actually been emitted.

use crate::engine::estimate_word_boundaries;
use crate::types::WordBoundary;

/// One estimated boundary event with source-text position resolved.
#[derive(Debug, Clone)]
pub struct EstimateEvent {
    /// The spoken word.
    pub word: String,
    /// Estimate start time in seconds (rate-1.0 baseline).
    pub start_s: f32,
    /// Estimate end time in seconds (rate-1.0 baseline).
    pub end_s: f32,
    /// Byte offset into the spoken plain text (-1 when unresolvable).
    pub char_offset: i32,
    /// Character length of the word.
    pub char_len: i32,
}

/// Pre-resolved estimated boundaries for an utterance, in firing order.
pub struct EstimatePlan {
    events: Vec<EstimateEvent>,
}

impl EstimatePlan {
    /// Build from the crate's 150-wpm estimator, resolving char offsets in
    /// the spoken text. SSML input is stripped first so offsets and word
    /// lists match what is actually spoken.
    #[must_use]
    pub fn build(text: &str) -> Self {
        let plain = if text.trim_start().to_ascii_lowercase().starts_with("<speak") {
            crate::engine::strip_ssml_to_text(text)
        } else {
            text.to_string()
        };
        let estimated = estimate_word_boundaries(&plain);
        Self::from_estimates(&estimated, &plain)
    }

    /// Build from pre-computed boundaries against `plain` text (used by
    /// engines that already have the plain text and estimates in hand).
    #[must_use]
    pub fn from_estimates(estimated: &[WordBoundary], plain: &str) -> Self {
        let mut events = Vec::with_capacity(estimated.len());
        let mut search_from = 0usize;
        for b in estimated {
            #[allow(clippy::cast_possible_truncation)]
            let char_offset = plain[search_from..]
                .find(&b.text)
                .map_or(-1, |pos| (search_from + pos) as i32);
            if char_offset >= 0 {
                search_from = char_offset as usize + b.text.len();
            }
            #[allow(clippy::cast_precision_loss)]
            let start = b.offset as f32 / 1000.0;
            #[allow(clippy::cast_precision_loss)]
            let end = (b.offset + b.duration) as f32 / 1000.0;
            let char_len = b.text.chars().count() as i32;
            events.push(EstimateEvent {
                word: b.text.clone(),
                start_s: start,
                end_s: end,
                char_offset,
                char_len,
            });
        }
        Self { events }
    }

    /// Number of events in the plan.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// The event at `idx` (firing order).
    #[must_use]
    pub fn event(&self, idx: usize) -> Option<&EstimateEvent> {
        self.events.get(idx)
    }

    /// True when the plan has no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Fires [`EstimatePlan`] events as cumulative delivered audio crosses
/// each estimate's start time (scaled by `time_scale` to account for the
/// engine's actual speech rate). Owns the plan so it can be moved into
/// 'static callbacks.
pub struct EstimateFirer {
    plan: Box<EstimatePlan>,
    next: usize,
    samples: u64,
    rate: Option<u32>,
    time_scale: f32,
}

impl EstimateFirer {
    /// Create a firer for `plan`. `time_scale` converts estimate seconds
    /// into delivered-audio seconds (e.g. `1/speed` for engines whose
    /// rate parameter compresses duration; 1.0 when estimates already
    /// match delivery).
    #[must_use]
    pub fn new(plan: EstimatePlan, time_scale: f32) -> Self {
        Self {
            plan: Box::new(plan),
            next: 0,
            samples: 0,
            rate: None,
            time_scale,
        }
    }

    /// Record `samples` newly-emitted PCM16-mono samples and fire every
    /// estimate whose (scaled) start time has been reached.
    pub fn on_samples(
        &mut self,
        samples: u64,
        rate_now: Option<u32>,
        fire: &mut dyn FnMut(&EstimateEvent),
    ) {
        self.samples += samples;
        if let Some(r) = rate_now {
            self.rate = Some(r);
        }
        let Some(rate) = self.rate else { return };
        #[allow(clippy::cast_precision_loss)]
        let rate_f = rate as f32;
        while self.next < self.plan.events.len() {
            let e = &self.plan.events[self.next];
            #[allow(clippy::cast_precision_loss)]
            let threshold = (e.start_s * self.time_scale * rate_f) as u64;
            if self.samples >= threshold {
                fire(e);
                self.next += 1;
            } else {
                break;
            }
        }
    }

    /// Fire every remaining estimate (stream ended before their times).
    pub fn flush(&mut self, fire: &mut dyn FnMut(&EstimateEvent)) {
        while self.next < self.plan.events.len() {
            fire(&self.plan.events[self.next]);
            self.next += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn estimate_event(word: &str, start_s: f32, end_s: f32) -> EstimateEvent {
        EstimateEvent {
            word: word.into(),
            start_s,
            end_s,
            char_offset: -1,
            char_len: word.len() as i32,
        }
    }

    #[test]
    fn firer_fires_at_scaled_thresholds() {
        let events = vec![
            estimate_event("one", 0.0, 0.5),
            estimate_event("two", 0.5, 1.0),
        ];
        let plan = EstimatePlan { events };
        let mut firer = EstimateFirer::new(plan, 1.0);
        let mut seen: Vec<String> = Vec::new();
        firer.on_samples(6000, Some(24_000), &mut |e| seen.push(e.word.clone()));
        assert_eq!(seen, vec!["one"], "0.25s of audio → only first word");
        firer.on_samples(6000, None, &mut |e| seen.push(e.word.clone()));
        assert_eq!(seen, vec!["one", "two"], "0.5s total → second word");
    }

    #[test]
    fn firer_applies_time_scale() {
        // Speed 2× → audio half as long → estimates scale by 1/2.
        let events = vec![
            estimate_event("one", 0.0, 0.5),
            estimate_event("two", 0.5, 1.0),
        ];
        let plan = EstimatePlan { events };
        let mut firer = EstimateFirer::new(plan, 0.5);
        let mut count = 0usize;
        firer.on_samples(6000, Some(24_000), &mut |_| count += 1);
        assert_eq!(count, 2, "0.25s audio at 2× covers both estimates");
    }

    #[test]
    fn firer_flush_fires_remainder() {
        let events = vec![estimate_event("one", 10.0, 10.5)];
        let plan = EstimatePlan { events };
        let mut firer = EstimateFirer::new(plan, 1.0);
        let mut count = 0usize;
        firer.on_samples(1000, Some(8000), &mut |_| count += 1);
        assert_eq!(count, 0);
        firer.flush(&mut |_| count += 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn plan_from_estimates_resolves_offsets() {
        use crate::types::WordBoundary;
        let est = vec![
            WordBoundary {
                text: "hello".into(),
                offset: 0,
                duration: 400,
                estimated: false,
            },
            WordBoundary {
                text: "world".into(),
                offset: 400,
                duration: 400,
                estimated: false,
            },
        ];
        let plan = EstimatePlan::from_estimates(&est, "hello world");
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.events[0].char_offset, 0);
        assert_eq!(plan.events[1].char_offset, 6);
    }
}
