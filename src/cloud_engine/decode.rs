#![allow(clippy::wildcard_imports)] // shared-import pattern for the split modules
use super::*;

/// Size of each audio chunk delivered via `on_audio` for JSON-body engines
/// (ElevenLabs `audio_base64`, Google `audioContent`). 8 KiB matches the
/// streaming-Read buffer used for HTTP-response engines. Exposed as a
/// named constant so tests can pin against the same value the production
/// `speak()` loop uses, rather than against the magic literal `8192`.
#[cfg(feature = "cloud")]
pub(crate) const STREAMING_CHUNK_SIZE: usize = 8192;

/// Decode an MP3 byte buffer to little-endian mono PCM16. Multi-channel input
/// is downmixed by averaging interleaved samples. Returns an empty vec if no
/// frames decode. Used so cloud engines deliver uniform PCM16 through
/// `on_audio` (matching the local SherpaOnnx / SAPI engines) instead of raw
/// MP3 bytes that a SAPI site would have to decode itself.
#[cfg(feature = "cloud")]
pub(crate) fn decode_audio_to_pcm16_mono(bytes: &[u8], ext_hint: &str) -> Vec<u8> {
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    // Cursor needs an owned buffer: MediaSourceStream boxes the source as
    // `dyn MediaSource + 'static`, so a borrowed `&[u8]` cursor won't compile.
    let mss = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes.to_vec())),
        MediaSourceStreamOptions::default(),
    );
    let mut hint = Hint::new();
    hint.with_extension(ext_hint);
    let mut format = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(p) => p.format,
        Err(_) => return Vec::new(),
    };
    let track = format.default_track().cloned();
    let Some(track) = track else {
        return Vec::new();
    };
    let Ok(mut decoder) =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())
    else {
        return Vec::new();
    };

    let mut pcm: Vec<u8> = Vec::new();
    while let Ok(packet) = format.next_packet() {
        let Ok(decoded_buf) = decoder.decode(&packet) else {
            continue;
        };
        mix_packet_to_mono_pcm16(&decoded_buf, &mut pcm);
    }
    pcm
}

/// MP3 → PCM16 mono via the generic decoder.
pub(crate) fn decode_mp3_to_pcm16_mono(mp3: &[u8]) -> Vec<u8> {
    decode_audio_to_pcm16_mono(mp3, "mp3")
}

/// Scale a normalised f32 sample (`[-1.0, 1.0]`) to little-endian PCM16 and
/// append it. Pulled out so the F32 branch above stays readable.
#[cfg(feature = "cloud")]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub(crate) fn push_mono_f32(out: &mut Vec<u8>, s: f32) {
    let s16 = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
    out.extend_from_slice(&s16.to_le_bytes());
}

// ============================================================================
// Incremental streaming decode
//
// The original delivery path buffered the entire HTTP response
// (`resp.bytes()`) before decoding, so `on_audio` only fired once synthesis
// had fully completed. The helpers below decode compressed audio as it
// arrives over the network: a reader pushes bytes into a shared pipe while
// a symphonia pipeline pulls packets off it, so PCM16 chunks reach the
// caller's `on_audio` callback while the body is still downloading.
// ============================================================================

/// A thread-safe byte pipe: a network reader pushes bytes, the symphonia
/// `MediaSourceStream` pulls them. `finish()` marks end-of-stream; `fail()`
/// propagates a network error to the reading side.
#[cfg(feature = "cloud")]
pub(crate) struct SharedPipe {
    state: std::sync::Mutex<PipeState>,
    ready: std::sync::Condvar,
}

#[cfg(feature = "cloud")]
pub(crate) struct PipeState {
    buf: std::collections::VecDeque<u8>,
    eof: bool,
    error: Option<String>,
}

#[cfg(feature = "cloud")]
impl SharedPipe {
    pub(crate) fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(PipeState {
                buf: std::collections::VecDeque::new(),
                eof: false,
                error: None,
            }),
            ready: std::sync::Condvar::new(),
        }
    }

    /// Push downloaded bytes for the reader side.
    pub(crate) fn push(&self, bytes: &[u8]) {
        let mut st = self.state.lock().unwrap();
        st.buf.extend(bytes.iter().copied());
        drop(st);
        self.ready.notify_all();
    }

    /// Signal that no more bytes will arrive.
    pub(crate) fn finish(&self) {
        let mut st = self.state.lock().unwrap();
        st.eof = true;
        drop(st);
        self.ready.notify_all();
    }

    /// Signal a network failure.
    pub(crate) fn fail(&self, msg: String) {
        let mut st = self.state.lock().unwrap();
        st.error = Some(msg);
        st.eof = true;
        drop(st);
        self.ready.notify_all();
    }

    /// Blocking pull of up to `out.len()` bytes.
    pub(crate) fn pull(&self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut st = self.state.lock().unwrap();
        loop {
            if !st.buf.is_empty() {
                let n = out.len().min(st.buf.len());
                for slot in out.iter_mut().take(n) {
                    *slot = st.buf.pop_front().expect("buf non-empty checked");
                }
                return Ok(n);
            }
            if let Some(err) = st.error.take() {
                return Err(std::io::Error::other(err));
            }
            if st.eof {
                return Ok(0);
            }
            st = self.ready.wait(st).unwrap();
        }
    }
}

#[cfg(feature = "cloud")]
impl std::io::Read for SharedPipe {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        SharedPipe::pull(self, out)
    }
}

#[cfg(feature = "cloud")]
impl std::io::Seek for SharedPipe {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "network stream is not seekable",
        ))
    }
}

#[cfg(feature = "cloud")]
impl symphonia::core::io::MediaSource for SharedPipe {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None // unknown while streaming
    }
}

/// Mix one decoded packet down to mono PCM16 LE bytes, appending to `out`.
/// Shared by the whole-buffer decoder and the incremental one so their
/// output stays byte-identical.
#[cfg(feature = "cloud")]
pub(crate) fn mix_packet_to_mono_pcm16(
    decoded: &symphonia::core::audio::AudioBufferRef,
    out: &mut Vec<u8>,
) {
    use symphonia::core::audio::AudioBufferRef;

    let frames = decoded.frames();
    #[allow(clippy::cast_precision_loss)]
    let nch = decoded.spec().channels.count().max(1) as f32;
    match decoded {
        AudioBufferRef::F32(buf) => {
            let planes = buf.planes();
            let slices = planes.planes();
            for f in 0..frames {
                let sum: f32 = slices
                    .iter()
                    .map(|s| s.get(f).copied().unwrap_or(0.0))
                    .sum();
                push_mono_f32(out, sum / nch);
            }
        }
        AudioBufferRef::S16(buf) => {
            let planes = buf.planes();
            let slices = planes.planes();
            for f in 0..frames {
                let sum: i32 = slices
                    .iter()
                    .map(|s| s.get(f).copied().unwrap_or(0))
                    .map(i32::from)
                    .sum();
                #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                let avg = (sum as f32 / nch) as i16;
                out.extend_from_slice(&avg.to_le_bytes());
            }
        }
        _ => {}
    }
}

/// Incremental pull-decoder over a [`SharedPipe`]. Lazily probes the format
/// once enough bytes are buffered, then yields mono PCM16 bytes packet by
/// packet via `next_chunk`.
#[cfg(feature = "cloud")]
pub(crate) struct IncrementalDecoder {
    pipe: Arc<SharedPipe>,
    format: Option<Box<dyn symphonia::core::formats::FormatReader>>,
    decoder: Option<Box<dyn symphonia::core::codecs::Decoder>>,
    track_id: u32,
    sample_buf: Option<symphonia::core::audio::SampleBuffer<i16>>,
    /// Sample rate observed in the first decoded packet (None until then).
    sample_rate: Option<u32>,
}

#[cfg(feature = "cloud")]
impl IncrementalDecoder {
    pub(crate) fn new(pipe: Arc<SharedPipe>) -> Self {
        Self {
            pipe,
            format: None,
            decoder: None,
            track_id: 0,
            sample_buf: None,
            sample_rate: None,
        }
    }

    /// Sample rate observed so far (known after the first decoded packet).
    pub(crate) fn sample_rate(&self) -> Option<u32> {
        self.sample_rate
    }

    /// Hand the pipe to symphonia and set up the demuxer + decoder. The
    /// probe blocks through the pipe until enough header bytes arrive, so
    /// this returns only once the format is known (or genuinely undecodable).
    pub(crate) fn ensure_probed(&mut self) -> Result<(), String> {
        if self.format.is_some() {
            return Ok(());
        }
        let mss = symphonia::core::io::MediaSourceStream::new(
            Box::new(SharedPipeReader {
                pipe: Arc::clone(&self.pipe),
            }),
            symphonia::core::io::MediaSourceStreamOptions::default(),
        );
        let mut hint = symphonia::core::probe::Hint::new();
        hint.with_extension("mp3");
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &symphonia::core::formats::FormatOptions::default(),
                &symphonia::core::meta::MetadataOptions::default(),
            )
            .map_err(|e| format!("probe failed: {e}"))?;
        let format = probed.format;
        let track = format
            .default_track()
            .cloned()
            .ok_or_else(|| "no decodable track".to_string())?;
        self.track_id = track.id;
        let decoder = symphonia::default::get_codecs()
            .make(
                &track.codec_params,
                &symphonia::core::codecs::DecoderOptions::default(),
            )
            .map_err(|e| format!("decoder init failed: {e}"))?;
        self.decoder = Some(decoder);
        self.format = Some(format);
        Ok(())
    }

    /// Decode the next available packet. `Ok(None)` = stream complete.
    pub(crate) fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.ensure_probed()?;
        let format = self.format.as_mut().expect("ensure_probed guarantees Some");
        let decoder = self
            .decoder
            .as_mut()
            .expect("ensure_probed guarantees Some");
        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None); // clean EOF
                }
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::Other =>
                {
                    // Network failure surfaced through the pipe.
                    return Err(format!("network read failed: {e}"));
                }
                Err(symphonia::core::errors::Error::ResetRequired) => {
                    let track = format
                        .tracks()
                        .iter()
                        .find(|t| t.id == self.track_id)
                        .ok_or("track disappeared")?;
                    *decoder = symphonia::default::get_codecs()
                        .make(
                            &track.codec_params,
                            &symphonia::core::codecs::DecoderOptions::default(),
                        )
                        .map_err(|e| format!("decoder re-init failed: {e}"))?;
                    self.sample_buf = None;
                    continue;
                }
                Err(e) => return Err(format!("demux error: {e}")),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded_pkt = match decoder.decode(&packet) {
                Ok(d) => d,
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(format!("decode error: {e}")),
            };
            let spec = *decoded_pkt.spec();
            if self.sample_rate.is_none() {
                self.sample_rate = Some(spec.rate);
            }
            let capacity = decoded_pkt.capacity() as u64;
            let buf = self.sample_buf.get_or_insert_with(|| {
                symphonia::core::audio::SampleBuffer::<i16>::new(capacity, spec)
            });
            buf.copy_interleaved_ref(decoded_pkt);
            let samples = buf.samples();
            // Interleaved → mono mix (channels are averaged).
            let nch = spec.channels.count().max(1);
            let mut out = Vec::with_capacity(samples.len() / nch * 2);
            if nch == 1 {
                for s in samples {
                    out.extend_from_slice(&s.to_le_bytes());
                }
            } else {
                for frame in samples.chunks(nch) {
                    let sum: i32 = frame.iter().copied().map(i32::from).sum();
                    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                    let avg = (sum as f32 / nch as f32) as i16;
                    out.extend_from_slice(&avg.to_le_bytes());
                }
            }
            return Ok(Some(out));
        }
    }
}

/// `Read` adapter handing the pipe to symphonia while the decoder keeps
/// the `Arc` alive.
#[cfg(feature = "cloud")]
pub(crate) struct SharedPipeReader {
    pipe: Arc<SharedPipe>,
}

#[cfg(feature = "cloud")]
impl std::io::Read for SharedPipeReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.pipe.pull(out)
    }
}

#[cfg(feature = "cloud")]
impl std::io::Seek for SharedPipeReader {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "network stream is not seekable",
        ))
    }
}

#[cfg(feature = "cloud")]
impl symphonia::core::io::MediaSource for SharedPipeReader {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// Stream an HTTP response body to `on_audio` as mono PCM16, decoding
/// incrementally so audio reaches the caller while the body downloads.
///
/// * `is_pcm` — the body is already raw PCM16 mono; chunk it straight
///   through without spawning a decode thread.
///
/// Returns the total bytes delivered. Empty delivery (probe failure on a
/// zero-byte or undecodable body) is not an error, matching the buffered
/// path's behaviour.
///
/// A streamed event: audio bytes, or an estimated word boundary fired
/// progressively during streaming.
#[cfg(feature = "cloud")]
pub(crate) enum StreamEvt<'x> {
    Audio(&'x [u8]),
    Boundary(&'x str, f32, f32, i32, i32),
}

/// When `plan` is given, estimated word boundaries fire **progressively**
/// — estimate *i* fires once ≥ its start-time worth of audio has been
/// emitted — instead of all at once after the response completes, so
/// callers interleaving marks with playback (e.g. the VoiceGarden-SPD
/// speech-dispatcher module) can report them in sync.
#[cfg(feature = "cloud")]
#[allow(clippy::too_many_lines)]
pub(crate) fn stream_body_to_on_audio(
    mut body: impl std::io::Read + Send + 'static,
    is_pcm: bool,
    pcm_rate: u32,
    plan: Option<EstimatePlan>,
    on_event: &mut dyn FnMut(StreamEvt<'_>),
) -> Result<usize, String> {
    let mut firer = plan.map(|p| EstimateFirer::new(p, 1.0));

    if is_pcm {
        let mut buf = [0u8; STREAMING_CHUNK_SIZE];
        let mut total = 0usize;
        loop {
            let n = body
                .read(&mut buf)
                .map_err(|e| format!("network read failed: {e}"))?;
            if n == 0 {
                break;
            }
            on_event(StreamEvt::Audio(&buf[..n]));
            total += n;
            if let Some(f) = firer.as_mut() {
                // PCM16 mono: 2 bytes per sample.
                f.on_samples((n / 2) as u64, Some(pcm_rate), &mut |ev| {
                    on_event(StreamEvt::Boundary(
                        &ev.word,
                        ev.start_s,
                        ev.end_s,
                        ev.char_offset,
                        ev.char_len,
                    ));
                });
            }
        }
        if let Some(f) = firer.as_mut() {
            f.flush(&mut |ev| {
                on_event(StreamEvt::Boundary(
                    &ev.word,
                    ev.start_s,
                    ev.end_s,
                    ev.char_offset,
                    ev.char_len,
                ));
            });
        }
        return Ok(total);
    }

    let pipe = Arc::new(SharedPipe::new());
    let reader_pipe = Arc::clone(&pipe);
    let reader = std::thread::Builder::new()
        .name("cloud-audio-reader".into())
        .spawn(move || {
            let mut buf = [0u8; STREAMING_CHUNK_SIZE];
            loop {
                match body.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => reader_pipe.push(&buf[..n]),
                    Err(e) => {
                        reader_pipe.fail(format!("network read failed: {e}"));
                        return;
                    }
                }
            }
            reader_pipe.finish();
        })
        .map_err(|e| format!("failed to spawn reader thread: {e}"))?;

    let mut dec = IncrementalDecoder::new(Arc::clone(&pipe));
    let mut total = 0usize;
    loop {
        match dec.next_chunk() {
            Ok(Some(chunk)) => {
                if chunk.is_empty() {
                    continue;
                }
                on_event(StreamEvt::Audio(&chunk));
                total += chunk.len();
                if let Some(f) = firer.as_mut() {
                    // Decoded PCM16 mono: one i16 per sample.
                    f.on_samples(chunk.len() as u64, dec.sample_rate(), &mut |ev| {
                        on_event(StreamEvt::Boundary(
                            &ev.word,
                            ev.start_s,
                            ev.end_s,
                            ev.char_offset,
                            ev.char_len,
                        ));
                    });
                }
            }
            Ok(None) => break,
            Err(e) => {
                // Drain the reader thread, then surface the error — but
                // only if nothing was delivered (a decode hiccup after
                // valid audio matches the buffered path's tolerance).
                let _ = reader.join();
                if total == 0 {
                    return Err(e);
                }
                eprintln!("rust-tts-wrapper: streaming decode error after {total} bytes: {e}");
                if let Some(f) = firer.as_mut() {
                    f.flush(&mut |ev| {
                        on_event(StreamEvt::Boundary(
                            &ev.word,
                            ev.start_s,
                            ev.end_s,
                            ev.char_offset,
                            ev.char_len,
                        ));
                    });
                }
                return Ok(total);
            }
        }
    }
    let _ = reader.join();
    if let Some(f) = firer.as_mut() {
        f.flush(&mut |ev| {
            on_event(StreamEvt::Boundary(
                &ev.word,
                ev.start_s,
                ev.end_s,
                ev.char_offset,
                ev.char_len,
            ));
        });
    }
    Ok(total)
}

/// Sniff the first few bytes for an MP3 sync word or ID3 tag. Kept as a
/// diagnostic helper but not used for delivery routing — raw PCM16 audio
/// frequently contains 0xFF 0xE0+ byte pairs that false-positive, so format
/// routing uses the explicit `CloudConfig::response_is_pcm` flag instead.
#[cfg(test)]
pub(crate) fn looks_like_mp3(b: &[u8]) -> bool {
    if b.len() >= 3 && &b[..3] == b"ID3" {
        return true;
    }
    let scan = b.len().min(4096).saturating_sub(1);
    (0..scan).any(|i| b[i] == 0xFF && (b[i + 1] & 0xE0) == 0xE0)
}
