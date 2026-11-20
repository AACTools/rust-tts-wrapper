use std::sync::{Arc, Condvar, Mutex};

use super::{
    drain_into_buffer, resample_mono_to_device, reset_for_utterance, Shared, SharedHandle,
};

fn fresh_shared() -> SharedHandle {
    Arc::new((
        Mutex::new(Shared {
            queue: std::collections::VecDeque::new(),
            pushed_all: false,
            drained: false,
            dead: false,
        }),
        Condvar::new(),
    ))
}

/// THE loop-review C1 pin: the stream's callback drains the ONE
/// stable handle. Two sequential utterances against the SAME handle
/// (as the stream sees them) — reset, push, drain, repeat — must both
/// deliver their audio. The broken design swapped handles per
/// utterance and utterance 2 was silent.
#[test]
fn sequential_utterances_on_the_stable_handle() {
    let sh = fresh_shared();

    // utterance 1
    reset_for_utterance(&sh);
    {
        let mut g = sh.0.lock().unwrap();
        g.queue.extend([0.1, 0.2, 0.3]);
        g.pushed_all = true;
    }
    let mut buf = [0.0_f32; 8];
    drain_into_buffer(&sh, false, &mut buf);
    assert_eq!(&buf[..3], &[0.1, 0.2, 0.3]);
    assert!(sh.0.lock().unwrap().drained, "utterance 1 drained");

    // utterance 2 on the SAME handle — the handoff that broke
    reset_for_utterance(&sh);
    assert!(!sh.0.lock().unwrap().drained, "re-armed for round 2");
    {
        let mut g = sh.0.lock().unwrap();
        g.queue.extend([0.7, 0.8]);
        g.pushed_all = true;
    }
    let mut buf2 = [0.0_f32; 8];
    drain_into_buffer(&sh, false, &mut buf2);
    assert_eq!(&buf2[..2], &[0.7, 0.8], "utterance 2 is NOT silent");
    assert!(sh.0.lock().unwrap().drained);
}

/// reset drops stale pending audio — new speech supersedes old
/// (the overlapping-voices fix at the state level).
#[test]
fn reset_drops_pending_audio() {
    let sh = fresh_shared();
    {
        let mut g = sh.0.lock().unwrap();
        g.queue.extend([0.5; 1000]);
        g.pushed_all = true;
    }
    reset_for_utterance(&sh);
    let g = sh.0.lock().unwrap();
    assert!(g.queue.is_empty());
    assert!(!g.pushed_all && !g.drained, "flags re-armed");
}

/// A partial buffer does NOT drain a partially-pushed utterance.
#[test]
fn callback_partial_buffer_keeps_waiting() {
    let sh = fresh_shared();
    {
        let mut g = sh.0.lock().unwrap();
        g.queue.extend([1.0; 10]);
    }
    let mut buf = [0.0_f32; 4];
    drain_into_buffer(&sh, false, &mut buf);
    let g = sh.0.lock().unwrap();
    assert!(!g.drained, "producer has not pushed all yet");
    assert_eq!(g.queue.len(), 6);
}

/// stop() clears the queue from the callback within one buffer —
/// the structural fix for the uninterruptible-subprocess report.
#[test]
fn callback_cancel_clears_and_drains() {
    let sh = fresh_shared();
    {
        let mut g = sh.0.lock().unwrap();
        g.queue.extend([0.5; 1000]);
    }
    let mut buf = [0.0_f32; 16];
    drain_into_buffer(&sh, true, &mut buf);
    let g = sh.0.lock().unwrap();
    assert!(g.drained, "cancel marks the drain");
    assert!(g.queue.is_empty(), "pending audio dropped");
    assert_eq!(buf, [0.0_f32; 16], "buffer stays silent");
}

/// Same-rate resampling is pure channel duplication.
#[test]
fn resample_same_rate_duplicates_channels() {
    let out = resample_mono_to_device(&[0.25, -0.25], 24_000, 24_000, 2);
    assert_eq!(out, vec![0.25, 0.25, -0.25, -0.25]);
}

/// 16 kHz → 48 kHz triples the frame count (each source frame
/// interpolated to three).
#[test]
fn resample_16k_to_48k_triples() {
    let out = resample_mono_to_device(&[1.0, 0.5, 0.0], 16_000, 48_000, 1);
    assert_eq!(out.len(), 9, "3 frames × 3");
    assert!((out[0] - 1.0).abs() < 1e-6);
    assert!((out[3] - 0.5).abs() < 1e-6);
    assert!((out[6] - 0.0).abs() < 1e-6);
}

/// 22.05 kHz → 48 kHz (non-integer ratio, the catalog's common
/// upsample): exact ceil(n·ratio) frames, tail clamped.
#[test]
fn resample_22k05_to_48k_exact_length() {
    let src = vec![0.2_f32; 2205]; // 100 ms
    let out = resample_mono_to_device(&src, 22_050, 48_000, 1);
    assert_eq!(out.len(), 4800, "100 ms at 48 kHz");
    assert!(out.iter().all(|&v| (v - 0.2).abs() < 1e-6));
}

/// Degenerate inputs don't panic.
#[test]
fn resample_degenerate() {
    assert_eq!(resample_mono_to_device(&[], 16_000, 48_000, 2).len(), 0);
    assert_eq!(resample_mono_to_device(&[0.1], 16_000, 48_000, 0).len(), 0);
    // one source frame upsamples to a constant run
    let out = resample_mono_to_device(&[0.75], 8_000, 24_000, 1);
    assert!(out.iter().all(|&v| (v - 0.75).abs() < 1e-6));
}
