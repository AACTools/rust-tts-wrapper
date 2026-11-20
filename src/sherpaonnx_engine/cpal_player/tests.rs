use std::sync::{Arc, Condvar, Mutex};

use super::{drain_into_buffer, resample_mono_to_device, Shared, SharedHandle};

fn shared_with(samples: &[f32], pushed_all: bool) -> SharedHandle {
    Arc::new((
        Mutex::new(Shared {
            queue: samples.iter().copied().collect(),
            pushed_all,
            drained: false,
            dead: false,
        }),
        Condvar::new(),
    ))
}

/// The callback fills the buffer from the queue and marks the
/// drain when everything pushed has been output.
#[test]
fn callback_drains_and_marks_done() {
    let sh = shared_with(&[0.1, 0.2, 0.3, 0.4], true);
    let mut buf = [0.0_f32; 8]; // cpal pre-fills silence
    buf.fill(0.0);
    drain_into_buffer(&sh, false, &mut buf);
    let g = sh.0.lock().unwrap();
    assert_eq!(&buf[..4], &[0.1, 0.2, 0.3, 0.4]);
    assert_eq!(&buf[4..], &[0.0; 4], "remainder stays silent");
    assert!(g.drained, "all pushed, queue empty");
}

/// A partial buffer does NOT drain a partially-pushed utterance.
#[test]
fn callback_partial_buffer_keeps_waiting() {
    let sh = shared_with(&[1.0; 10], false);
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
    let sh = shared_with(&[0.5; 1000], false);
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
    // near-source samples are close to the source values
    assert!((out[0] - 1.0).abs() < 1e-6);
    assert!((out[3] - 0.5).abs() < 1e-6);
    assert!((out[6] - 0.0).abs() < 1e-6);
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
