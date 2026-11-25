//! qwen3-local synthesis core helpers: the streaming callback state
//! and the f32→PCM16 conversion, pure so they are unit-testable
//! without the C++ side.

use std::ffi::c_void;

/// Mutable state shared between the Rust stream callback and the
/// speak() driver: delivered PCM16 chunks, the sample count (for
/// boundary estimation), the volume gain, and the cancellation bit.
pub struct StreamState<'a> {
    /// receives PCM16 LE mono bytes as they stream
    pub deliver: &'a mut dyn FnMut(&[u8]),
    /// linear volume gain (1.0 = unity)
    pub gain: f32,
    /// total f32 samples delivered (duration = n / sample_rate)
    pub total_samples: usize,
    /// observed cancellation; the trampoline returns 0 once set
    pub cancelled: bool,
}

impl<'a> StreamState<'a> {
    /// Build the state around a PCM16-bytes sink and a volume gain.
    pub fn new(deliver: &'a mut dyn FnMut(&[u8]), gain: f32) -> Self {
        Self {
            deliver,
            gain,
            total_samples: 0,
            cancelled: false,
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

/// The extern "C" callback handed to the C++ streaming API.
/// `user_data` points at a `StreamState`. Returns 0 to stop
/// generation (the upstream contract); 1 continues.
pub extern "C" fn stream_trampoline(samples: *const f32, n: i32, user_data: *mut c_void) -> i32 {
    // SAFETY: user_data is `&mut StreamState` from the only call site
    // (run_streaming), alive for the whole C++ call, and the C++
    // callback is invoked synchronously on the calling thread
    let state: &mut StreamState = unsafe { &mut *(user_data as *mut StreamState) };
    if crate::qwen3_local_engine::qwen_cancel_requested() {
        state.cancelled = true;
        return 0;
    }
    #[allow(clippy::cast_possible_truncation)]
    let n = n.max(0) as usize;
    if n == 0 || samples.is_null() {
        return 1;
    }
    let f32s = unsafe { std::slice::from_raw_parts(samples, n) };
    let pcm16 = chunk_to_pcm16(f32s, state.gain);
    let mut bytes = Vec::with_capacity(pcm16.len() * 2);
    for v in &pcm16 {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    (state.deliver)(&bytes);
    state.total_samples += n;
    1
}
