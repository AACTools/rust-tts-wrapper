//! #139: in-process playback for the sherpa local-play path on LINUX
//! (cpal — ALSA backend; PipeWire users go through the ALSA plugin),
//! mirroring the Windows WASAPI work (#120). LINUX ONLY by design
//! (Dasher-Apple review): macOS routes through bytes + AVAudioPlayer
//! — route changes, interruptions and session management come free
//! with AVFoundation, none of which a cached output stream can
//! promise. Built only under the `speaker` feature: cpal's ALSA
//! backend requires libasound2-dev at build time and sherpaonnx is a
//! default feature, so the subprocess fallback remains for
//! header-free builds.
//!
//! Architecture (the keep-warm design): ONE output stream, built
//! lazily at the device's default config and kept alive across
//! utterances — per-word speech never pays device-open cost. The
//! stream's callback captures ONE stable [`SharedHandle`] at build
//! time; each utterance RESETS that handle's per-utterance state
//! under its mutex (queue cleared, flags re-armed) — the handoff
//! that once swapped handles per utterance left the callback
//! draining an orphaned queue (loop review: utterance 2+ was silent
//! and long pushes deadlocked). Samples are linear-resampled
//! mono→device layout (rate AND channels) up front. `stop()`
//! (CANCEL_REQUESTED) clears the queue from the callback, so
//! playback stops within one buffer (~10–50 ms); the push and drain
//! waits are both cancellation-aware so stop() can never leave
//! speak() stuck.
//!
//! The WASAPI contract carries over verbatim: `true` = handled (do
//! NOT fall back); empty input or rate 0 → `true`; `false` only for
//! pre-playback unavailability (no host/device, build/play failure,
//! or the stream dying before any audio reached the device);
//! mid-stream death after audio started → `true` (the user heard
//! part of it — a fallback would replay from the top); cancellation
//! is not failure (the call site rescues with `|| CANCEL_REQUESTED`).
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::sherpaonnx_engine::CANCEL_REQUESTED;

/// The shared state between the producer (`play_pcm_cpal`) and the
/// audio callback — ONE per STREAM, stable for its lifetime. Queue
/// holds INTERLEAVED samples at the DEVICE's rate/channels (already
/// resampled when pushed). Per-utterance state (queue, flags) is
/// reset under the mutex by [`reset_for_utterance`].
struct Shared {
    queue: VecDeque<f32>,
    /// the producer has pushed the whole utterance
    pushed_all: bool,
    /// every pushed sample has been output (or the queue was cleared)
    drained: bool,
    /// the stream's error callback fired (device gone, etc.) —
    /// stream-lifetime state, NOT reset per utterance
    dead: bool,
}

type SharedHandle = Arc<(Mutex<Shared>, Condvar)>;

/// Arm this stream-shared handle for a new utterance: drop anything
/// pending (new speech supersedes old — the overlapping-voices fix)
/// and re-arm the completion flags.
fn reset_for_utterance(shared: &SharedHandle) {
    let mut g = shared
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    g.queue.clear();
    g.pushed_all = false;
    g.drained = false;
}

/// The callback body, standalone so it is testable headlessly.
/// cpal 0.18 pre-fills the buffer with silence — writing fewer
/// samples than `data.len()` leaves the rest silent (the underflow
/// behavior we want).
fn drain_into_buffer(shared: &SharedHandle, cancelled: bool, data: &mut [f32]) {
    let Ok(mut g) = shared.0.lock() else {
        return;
    };
    if cancelled {
        // stop() lands within one buffer: drop everything pending
        g.queue.clear();
        g.drained = true;
    } else {
        let n = g.queue.len().min(data.len());
        for slot in &mut data[..n] {
            *slot = g.queue.pop_front().unwrap_or_default();
        }
        if g.pushed_all && g.queue.is_empty() {
            g.drained = true;
        }
    }
    drop(g);
    // wake the producer's drain wait — non-blocking, callback-safe
    shared.1.notify_all();
}

struct OutputState {
    stream: cpal::Stream,
    rate: u32,
    channels: u16,
    shared: SharedHandle,
}

static OUTPUT: Mutex<Option<OutputState>> = Mutex::new(None);

/// Linear-resample mono samples to the device's rate and duplicate
/// across its channels — the same interpolation technique as the
/// pitch path's resampler, acceptable quality for speech, no new
/// dependency.
// sample counts are utterance-length (<< 2^52): the f64 casts are
// lossless in practice (same allow class as the engine's pitch path)
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
#[must_use]
fn resample_mono_to_device(
    samples: &[f32],
    from_rate: u32,
    to_rate: u32,
    channels: u16,
) -> Vec<f32> {
    if samples.is_empty() || channels == 0 {
        return Vec::new();
    }
    let channels = channels as usize;
    if from_rate == to_rate {
        let mut out = Vec::with_capacity(samples.len() * channels);
        for &s in samples {
            for _ in 0..channels {
                out.push(s);
            }
        }
        return out;
    }
    let ratio = f64::from(to_rate) / f64::from(from_rate);
    let out_frames = ((samples.len() as f64) * ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_frames * channels);
    let last = samples.len() - 1;
    for i in 0..out_frames {
        let src = (i as f64) / ratio;
        let i0 = src.floor() as usize;
        let frac = src - i0 as f64;
        let s0 = samples[i0.min(last)];
        let s1 = samples[(i0 + 1).min(last)];
        let v = f64::from(s0) + frac * (f64::from(s1) - f64::from(s0));
        #[allow(clippy::cast_possible_truncation)]
        let v = v as f32;
        for _ in 0..channels {
            out.push(v);
        }
    }
    out
}

/// Build (or reuse) the persistent output stream and play `samples`
/// through it. Returns the WASAPI-contract bool; see the module doc.
pub(super) fn play_pcm_cpal(samples: &[f32], sample_rate: u32) -> bool {
    if samples.is_empty() || sample_rate == 0 {
        return true; // handled: nothing to play
    }
    // Ensure the stream exists and is alive. The lock is held across
    // the (re)build on purpose: building under the lock serializes
    // concurrent first-calls (the loop-review double-build race left
    // the loser's stream dropped mid-utterance) and the build is a
    // bounded one-off, unlike pushing/waiting which must NOT hold it.
    let (shared, rate, channels) = {
        let mut slot = OUTPUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let needs_rebuild = slot.as_ref().is_none_or(|st| {
            let g = st
                .shared
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            g.dead
        });
        if needs_rebuild {
            // drop the dead stream first (ALSA's Drop joins its
            // thread — the old callback is provably stopped)
            *slot = None;
            match build_stream() {
                Some(state) => *slot = Some(state),
                None => return false, // pre-playback unavailability
            }
        }
        let st = slot.as_ref().expect("stream just ensured");
        (Arc::clone(&st.shared), st.rate, st.channels)
    };

    // Arm the STREAM's stable handle for this utterance (the
    // callback drains THIS handle — no per-utterance swapping).
    reset_for_utterance(&shared);
    let device_samples = resample_mono_to_device(samples, sample_rate, rate, channels);
    play_out(&shared, &device_samples, rate)
}

/// Push the utterance chunk-by-chunk and wait for the drain. Split
/// out of `play_pcm_cpal` for review clarity.
fn play_out(shared: &SharedHandle, device_samples: &[f32], device_rate: u32) -> bool {
    // push in bounded chunks so the callback's lock is never held
    // across a whole utterance (priority-inversion dropouts)
    const CHUNK: usize = 16384; // interleaved samples per push
    const HIGH_WATER: usize = 65536; // producer backpressure threshold
    let mut pushed_any = false;
    for chunk in device_samples.chunks(CHUNK) {
        {
            let mut g = shared
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while g.queue.len() >= HIGH_WATER && !g.dead {
                if CANCEL_REQUESTED.load(Ordering::SeqCst) {
                    // stop() during the push phase: stop pushing
                    // (#120 semantics — cancellation is not failure)
                    return true;
                }
                g = match shared
                    .1
                    .wait_timeout(g, std::time::Duration::from_millis(250))
                {
                    Ok((ng, _)) => ng,
                    Err(p) => {
                        let (ng, _) = p.into_inner();
                        ng
                    }
                };
            }
            if g.dead {
                if pushed_any {
                    // audio already reached the user via a dying
                    // device; a fallback would replay from the top
                    return true;
                }
                // died before ANY audio: a legitimate fallback
                return false;
            }
            g.queue.extend(chunk.iter().copied());
            pushed_any = true;
        }
        shared.1.notify_all();
    }
    {
        let mut g = shared
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.pushed_all = true;
    }
    shared.1.notify_all();

    // drain: the wait IS the tail drain (no cutoff, no extra sleep).
    // Guarded by `dead` and a hard cap in DEVICE time (utterance
    // length + 500 ms) so a vanished device cannot hang speak().
    #[allow(clippy::cast_possible_truncation)]
    let cap = std::time::Duration::from_millis(
        device_samples.len() as u64 * 1000 / u64::from(device_rate.max(1)) + 500,
    );
    let started = std::time::Instant::now();
    let mut g = shared
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while !g.drained && !g.dead {
        if CANCEL_REQUESTED.load(Ordering::SeqCst) {
            // the callback clears on its next tick; treat as handled
            // (cancellation is not failure — #120 semantics)
            return true;
        }
        if started.elapsed() > cap {
            break; // never hang the caller on a silent device death
        }
        g = match shared
            .1
            .wait_timeout(g, std::time::Duration::from_millis(250))
        {
            Ok((ng, _)) => ng,
            Err(p) => {
                let (ng, _) = p.into_inner();
                ng
            }
        };
    }
    true
}

fn build_stream() -> Option<OutputState> {
    let shared: SharedHandle = Arc::new((
        Mutex::new(Shared {
            queue: VecDeque::new(),
            pushed_all: false,
            drained: false,
            dead: false,
        }),
        Condvar::new(),
    ));
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let supported = device.default_output_config().ok()?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        // f32 output is near-universal on ALSA; anything else takes
        // the subprocess fallback rather than converting formats
        // inside the audio callback
        return None;
    }
    let config: cpal::StreamConfig = supported.into();
    let rate = config.sample_rate; // 0.18: SampleRate is a plain u32
    let channels = config.channels;

    let cb_shared = Arc::clone(&shared);
    let err_shared = Arc::clone(&shared);
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                drain_into_buffer(&cb_shared, CANCEL_REQUESTED.load(Ordering::SeqCst), data);
            },
            move |err| {
                let mut g = err_shared
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                g.dead = true;
                let _ = err;
                err_shared.1.notify_all();
            },
            None,
        )
        .ok()?;
    // cpal 0.18: streams start PAUSED — an unplayed stream never
    // fires its callback (a silent no-op, not an error)
    stream.play().ok()?;
    Some(OutputState {
        stream,
        rate,
        channels,
        shared,
    })
}

#[cfg(test)]
mod tests;
