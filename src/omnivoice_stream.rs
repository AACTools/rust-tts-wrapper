//! omnivoice-local streaming core: the C chunk-callback state and the
//! f32→PCM16 conversion, kept pure so they are unit-testable without
//! the C++ side (the same split as `qwen3_stream`).
//!
//! The omnivoice.cpp chunk callback returns a C `bool` where
//! qwen3-tts.cpp's returns `i32` — but the abort polarity is the SAME
//! zero value in both (`false` ≡ 0 aborts); only the return type
//! differs. Do not "flip" a polarity when porting between them. Like
//! qwen3, the runtime discards the buffered output entirely when a
//! callback is registered — every sample arrives here.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

/// Mutable state shared between the Rust chunk callback and the
/// `speak()` driver: the PCM16 sink, the volume gain, the delivered
/// sample count (for boundary estimation), and the cancellation bits.
pub struct OvStreamState<'a> {
    /// receives PCM16 LE mono 24 kHz bytes as the runtime emits chunks
    pub deliver: &'a mut dyn FnMut(&[u8]),
    /// linear volume gain (1.0 = unity), applied per chunk
    pub gain: f32,
    /// total f32 samples delivered (duration = n / 24_000)
    pub total_samples: usize,
    /// observed cancellation (the trampoline returned false)
    pub cancelled: bool,
    /// `stop()` flag, polled between chunks from the trampoline
    pub cancel_flag: Option<&'a AtomicBool>,
}

impl<'a> OvStreamState<'a> {
    /// Build the state around a PCM16-bytes sink, a volume gain and
    /// the engine's `stop()` flag.
    pub fn new(
        deliver: &'a mut dyn FnMut(&[u8]),
        gain: f32,
        cancel_flag: Option<&'a AtomicBool>,
    ) -> Self {
        Self {
            deliver,
            gain,
            total_samples: 0,
            cancelled: false,
            cancel_flag,
        }
    }
}

/// The f32→PCM16 conversion + volume gain for one streamed chunk.
/// Gain is linear per-sample: per-chunk application is identical to
/// whole-buffer application.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn chunk_to_pcm16(samples: &[f32], gain: f32) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| {
            let s = s * gain;
            (s.clamp(-1.0, 1.0) * 32_767.0) as i16
        })
        .collect()
}

/// The extern "C" chunk callback handed to `ov_synthesize` (issue
/// #151 P5). `user_data` points at an [`OvStreamState`]. Returns
/// **false** to stop generation (the omnivoice.cpp contract).
pub extern "C" fn ov_chunk_trampoline(samples: *const f32, n: i32, user_data: *mut c_void) -> bool {
    // SAFETY: user_data is `&mut OvStreamState` from the only call
    // site (speak's streaming branch), alive for the whole C call,
    // and the runtime invokes the callback synchronously on the
    // calling thread.
    let state: &mut OvStreamState = unsafe { &mut *(user_data as *mut OvStreamState) };
    if state.cancel_flag.is_some_and(|f| f.load(Ordering::Relaxed)) {
        state.cancelled = true;
        return false;
    }
    let n = n.max(0) as usize;
    if n == 0 || samples.is_null() {
        return true;
    }
    // SAFETY: the runtime owns `n` f32 samples for the duration of
    // the callback (the contract: previous chunks are invalid after
    // the callback returns — so convert now, inside it).
    let f32s = unsafe { std::slice::from_raw_parts(samples, n) };
    let pcm16 = chunk_to_pcm16(f32s, state.gain);
    let mut bytes = Vec::with_capacity(pcm16.len() * 2);
    for v in &pcm16 {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    (state.deliver)(&bytes);
    state.total_samples += n;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_to_pcm16_applies_gain_and_clamps() {
        let out = chunk_to_pcm16(&[0.0, 0.5, -0.5, 2.0], 1.0);
        assert_eq!(out[0], 0);
        assert!((i32::from(out[1]) - 16_383).abs() <= 2);
        assert!((i32::from(out[2]) + 16_383).abs() <= 2);
        assert_eq!(out[3], 32_767, "clamped");

        let gained = chunk_to_pcm16(&[0.25], 2.0);
        assert!((i32::from(gained[0]) - 16_383).abs() <= 2, "gain applied");
    }

    #[test]
    fn trampoline_delivers_and_counts() {
        let mut delivered: Vec<Vec<u8>> = Vec::new();
        let flag = AtomicBool::new(false);
        {
            let mut deliver = |b: &[u8]| delivered.push(b.to_vec());
            let mut state = OvStreamState::new(&mut deliver, 1.0, Some(&flag));
            let samples = [0.0f32; 2400];
            let keep =
                ov_chunk_trampoline(samples.as_ptr(), 2400, (&raw mut state).cast::<c_void>());
            assert!(keep, "continue");
            assert_eq!(state.total_samples, 2400);
        }
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].len(), 4800, "2400 f32 -> 4800 PCM16 bytes");
    }

    #[test]
    fn trampoline_aborts_when_stop_requested() {
        let mut sink = 0usize;
        let flag = AtomicBool::new(true);
        {
            let mut deliver = |_b: &[u8]| sink += 1;
            let mut state = OvStreamState::new(&mut deliver, 1.0, Some(&flag));
            let samples = [0.0f32; 10];
            let keep = ov_chunk_trampoline(samples.as_ptr(), 10, (&raw mut state).cast::<c_void>());
            assert!(!keep, "abort");
            assert!(state.cancelled);
        }
        assert_eq!(sink, 0, "nothing delivered on abort");
    }

    #[test]
    fn trampoline_tolerates_empty_chunk() {
        let mut count = 0usize;
        let keep = {
            let mut deliver = |_b: &[u8]| count += 1;
            let mut state = OvStreamState::new(&mut deliver, 1.0, None);
            let keep = ov_chunk_trampoline(std::ptr::null(), 0, (&raw mut state).cast::<c_void>());
            let total = state.total_samples;
            assert_eq!(total, 0);
            keep
        };
        assert!(keep);
        assert_eq!(count, 0);
    }
}
