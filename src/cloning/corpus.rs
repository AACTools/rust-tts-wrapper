//! Banked-voice corpora: import from an Apple Personal Voice export ZIP
//! or an LJSpeech-format directory, and convert to a `VoiceIdentity`.

use super::{AudioClip, VoiceIdentity};
use crate::types::{TtsError, TtsResult};

/// Canonical corpus sample rate: satisfies every instant cloner's floor
/// (Qwen ≥16 kHz, Murf ≥24 kHz) and keeps enrollment files small
/// (≤10 MB Qwen cap).
pub(crate) const CORPUS_SAMPLE_RATE: u32 = 24_000;

/// Per-file read cap for imports (zip entries and corpus wavs): real
/// clips are a few MB; a zip bomb or misdirected archive must not OOM
/// the process. Oversized entries are skipped and counted.
const MAX_IMPORT_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// A banked voice on disk: clips + (optional) transcripts. Two on-disk
/// shapes are understood:
///
/// - **Apple Personal Voice export ZIP** (`Will's Personal Voice 1 -
///   Recordings.zip`): one `TrainingData/` folder of
///   `{session5}_{NN}.caf` files — CAF/ALAC, 48 kHz mono. Audio-only;
///   transcripts are *not* included (see [`VoiceCorpus::phrases`] to
///   attach them from a known prompt list).
/// - **LJSpeech layout**: `metadata.csv` (`ID|Transcription` rows) + a
///   `wav/` folder of mono 16-bit wavs. The de facto cross-tool
///   convention and what the Piper ecosystem eats.
#[derive(Debug, Clone, Default)]
pub struct VoiceCorpus {
    /// Corpus name — from the ZIP title (`Will's Personal Voice 1`) or
    /// the directory name.
    pub name: String,
    /// Imported clips, canonical PCM16 LE mono at 24 kHz (CORPUS_SAMPLE_RATE).
    pub clips: Vec<AudioClip>,
}

impl VoiceCorpus {
    /// Import an Apple Personal Voice "Recordings" export ZIP. Decodes
    /// every `TrainingData/*.caf` (Core Audio Format / Apple Lossless) to
    /// canonical PCM; corrupt or unreadable entries are skipped and
    /// counted in the return message.
    ///
    /// # Errors
    /// When the zip cannot be opened/read, or no `TrainingData/*.caf`
    /// clip decodes.
    pub fn from_personal_voice_zip(path: &str) -> TtsResult<Self> {
        let file = std::fs::File::open(path)
            .map_err(|e| TtsError(format!("open Personal Voice zip {path}: {e}")))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| TtsError(format!("read zip {path}: {e}")))?;
        // Apple names the archive "{Voice Name} - Recordings.zip"; the
        // voice name is the stem minus that suffix.
        let stem = std::path::Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Personal Voice");
        let name = stem
            .strip_suffix(" - Recordings")
            .unwrap_or(stem)
            .to_string();

        let mut clips = Vec::new();
        let mut skipped = 0usize;
        for i in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(i) else {
                skipped += 1;
                continue;
            };
            let entry_name = entry.name().to_string();
            if !entry_name.to_ascii_lowercase().ends_with(".caf") || entry.is_dir() {
                continue;
            }
            if entry.size() > MAX_IMPORT_FILE_BYTES {
                skipped += 1;
                continue;
            }
            let mut bytes = Vec::new();
            if std::io::Read::read_to_end(&mut entry, &mut bytes).is_err()
                || bytes.len() as u64 > MAX_IMPORT_FILE_BYTES
            {
                skipped += 1;
                continue;
            }
            match decode_to_canonical_pcm(&bytes) {
                Ok(pcm) => clips.push(AudioClip {
                    name: entry_name
                        .rsplit('/')
                        .next()
                        .unwrap_or(&entry_name)
                        .to_string(),
                    pcm,
                    sample_rate: CORPUS_SAMPLE_RATE,
                    transcript: None,
                }),
                Err(_) => {
                    skipped += 1;
                }
            }
        }
        if clips.is_empty() {
            return Err(TtsError(format!(
                "no TrainingData/*.caf clips decoded from {path}"
            )));
        }
        if skipped > 0 {
            let plural = if skipped == 1 { "entry" } else { "entries" };
            eprintln!(
                "rust-tts-wrapper: Personal Voice import skipped {skipped} unreadable archive {plural}"
            );
        }
        Ok(Self { name, clips })
    }

    /// Import an LJSpeech-layout directory (`metadata.csv` + `wav/`).
    /// Transcripts are attached when present (2- or 3-field rows).
    ///
    /// # Errors
    /// When the directory, `metadata.csv`, or `wav/` cannot be read, a
    /// wav fails to decode, or no wavs are found.
    pub fn from_ljspeech_dir(dir: &str) -> TtsResult<Self> {
        let dir_path = std::path::Path::new(dir);
        let meta = std::fs::read_to_string(dir_path.join("metadata.csv"))
            .map_err(|e| TtsError(format!("read {dir}/metadata.csv: {e}")))?;
        let name = dir_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("corpus")
            .to_string();
        let mut transcripts = std::collections::HashMap::new();
        for line in meta.lines() {
            let mut fields = line.splitn(3, '|');
            let (Some(id), Some(text)) = (fields.next(), fields.next()) else {
                continue;
            };
            let text = text.trim();
            if !text.is_empty() {
                transcripts.insert(id.trim().to_string(), text.to_string());
            }
        }
        let wav_dir = dir_path.join("wav");
        let mut clips = Vec::new();
        let mut skipped = 0usize;
        for entry in std::fs::read_dir(&wav_dir)
            .map_err(|e| TtsError(format!("read {dir}/wav: {e}")))?
            .flatten()
        {
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if !ext.eq_ignore_ascii_case("wav") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Same hygiene as the zip importer: cap reads, skip broken
            // wavs instead of failing the whole corpus.
            let Ok(meta) = entry.metadata() else {
                skipped += 1;
                continue;
            };
            if meta.len() > MAX_IMPORT_FILE_BYTES {
                skipped += 1;
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                skipped += 1;
                continue;
            };
            match decode_to_canonical_pcm(&bytes) {
                Ok(pcm) => clips.push(AudioClip {
                    name: stem.to_string(),
                    pcm,
                    sample_rate: CORPUS_SAMPLE_RATE,
                    transcript: transcripts.get(stem).cloned(),
                }),
                Err(_) => skipped += 1,
            }
        }
        if clips.is_empty() {
            return Err(TtsError(format!(
                "no wavs decoded from {dir}/wav ({skipped} skipped)"
            )));
        }
        if skipped > 0 {
            eprintln!("rust-tts-wrapper: LJSpeech import skipped {skipped} unreadable wav(s)");
        }
        Ok(Self { name, clips })
    }

    /// Attach transcripts from a Personal Voice prompt list, mapped by
    /// the `{NN}` index in each clip's filename. Prompt lists are fixed
    /// per iOS version and not shipped here (licensing); index bases
    /// vary by iOS build, so pairing is best-effort: a phrase is only
    /// attached when `phrases` has an entry for that index.
    #[must_use]
    pub fn phrases(mut self, phrases: &[(u64, String)]) -> Self {
        let map: std::collections::HashMap<u64, &str> =
            phrases.iter().map(|(i, p)| (*i, p.as_str())).collect();
        for clip in &mut self.clips {
            let Some((_, idx)) = clip.name.rsplit_once('_') else {
                continue;
            };
            let Ok(idx) = idx.trim_end_matches(".caf").parse::<u64>() else {
                continue;
            };
            if let Some(p) = map.get(&idx) {
                clip.transcript = Some((*p).to_string());
            }
        }
        self
    }

    /// Total audio duration across all clips, in seconds.
    #[must_use]
    pub fn total_duration_secs(&self) -> u32 {
        self.clips.iter().map(AudioClip::duration_secs).sum()
    }

    /// Convert to a `VoiceIdentity` for enrollment.
    #[must_use]
    pub fn to_identity(&self, language: Option<&str>) -> VoiceIdentity {
        VoiceIdentity {
            name: self.name.clone(),
            clips: self.clips.clone(),
            language: language.map(str::to_string),
        }
    }
}

/// Decode any supported container (CAF/ALAC, WAV/PCM) to canonical
/// PCM16 LE mono 24 kHz. Uses symphonia; resamples by integer
/// decimation when the source rate is a multiple of 24 kHz (Personal
/// Voice exports are 48 kHz → 2:1), naive linear interpolation
/// otherwise, and averages channels to mono.
pub(crate) fn decode_to_canonical_pcm(bytes: &[u8]) -> TtsResult<Vec<u8>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mss = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes.to_vec())),
        MediaSourceStreamOptions::default(),
    );
    let hint = Hint::new();
    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| TtsError(format!("probe audio: {e}")))?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| TtsError("no audio track".into()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| TtsError(format!("decoder: {e}")))?;
    // CodecParameters carries rate/channels as Options; CAF/ALAC Personal
    // Voice recordings are 48 kHz mono. Defaults cover malformed headers.
    let src_rate = track.codec_params.sample_rate.unwrap_or(CORPUS_SAMPLE_RATE);
    let channels = track
        .codec_params
        .channels
        .unwrap_or(symphonia::core::audio::Channels::FRONT_LEFT)
        .count();

    let mut samples: Vec<f32> = Vec::new();
    loop {
        let packet = match probed.format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(e) => return Err(TtsError(format!("decode: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                let spec = *audio_buf.spec();
                let mut buf = SampleBuffer::<f32>::new(audio_buf.capacity() as u64, spec);
                buf.copy_interleaved_ref(audio_buf);
                samples.extend_from_slice(buf.samples());
            }
            // Malformed packets are skipped (corrupt ALAC frames exist in
            // real exports); the surrounding stream still decodes.
            Err(symphonia::core::errors::Error::DecodeError(_)) => {}
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(TtsError(format!("decode: {e}"))),
        }
    }

    // Interleaved f32 → mono (average channels).
    #[allow(clippy::cast_precision_loss)]
    let mono: Vec<f32> = if channels <= 1 {
        samples
    } else {
        samples
            .chunks(channels)
            .map(|c| c.iter().sum::<f32>() / c.len() as f32)
            .collect()
    };

    // Resample to canonical rate.
    let resampled = resample(&mono, src_rate, CORPUS_SAMPLE_RATE);

    // f32 → PCM16 LE.
    let mut pcm = Vec::with_capacity(resampled.len() * 2);
    for s in resampled {
        let v = (s.clamp(-1.0, 1.0) * 32_767.0) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    Ok(pcm)
}

/// Integer decimation when the source is a multiple of the target
/// (48k→24k Personal Voice case), linear interpolation when the target
/// is a multiple of the source, nearest-neighbour for relatively-prime
/// ratios (rare). Voice-grade quality; providers re-process anyway.
#[allow(clippy::cast_precision_loss)]
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    if from > to && from.is_multiple_of(to) {
        let factor = (from / to) as usize;
        return input
            .chunks(factor)
            .map(|c| c.iter().sum::<f32>() / c.len() as f32)
            .collect();
    }
    if to > from && to.is_multiple_of(from) {
        let factor = (to / from) as usize;
        let mut out = Vec::with_capacity(input.len() * factor);
        for pair in input.windows(2) {
            for i in 0..factor {
                let t = i as f32 / factor as f32;
                out.push(pair[0] + (pair[1] - pair[0]) * t);
            }
        }
        if let Some(&last) = input.last() {
            out.extend(std::iter::repeat_n(last, factor));
        }
        return out;
    }
    // Relatively prime rates: nearest-neighbour — rare path, kept simple.
    let ratio = f64::from(to) / f64::from(from);
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    (0..out_len)
        .map(|i| {
            let src = ((i as f64) / ratio).round() as usize;
            input[src.min(input.len() - 1)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_integer_downsample_averages() {
        let out = resample(&[0.0, 1.0, 0.0, 1.0], 48_000, 24_000);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn resample_passthrough_and_upsample() {
        assert_eq!(resample(&[1.0, 2.0], 24_000, 24_000), vec![1.0, 2.0]);
        // 2 samples × factor 2 → 4 interpolated samples.
        let up = resample(&[0.0, 1.0], 24_000, 48_000);
        assert_eq!(up.len(), 4);
    }

    #[test]
    fn ljspeech_roundtrip_in_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let wav_dir = dir.path().join("wav");
        std::fs::create_dir_all(&wav_dir).unwrap();
        // 0.1 s of silence @ 24 kHz mono.
        let wav = super::super::wav_bytes(&vec![0u8; 4_800], CORPUS_SAMPLE_RATE);
        std::fs::write(wav_dir.join("lj0001.wav"), &wav).unwrap();
        std::fs::write(dir.path().join("metadata.csv"), "lj0001|Hello world.\n").unwrap();
        let corpus = VoiceCorpus::from_ljspeech_dir(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(corpus.clips.len(), 1);
        assert_eq!(corpus.clips[0].transcript.as_deref(), Some("Hello world."));
        assert_eq!(corpus.clips[0].sample_rate, CORPUS_SAMPLE_RATE);
        assert_eq!(corpus.total_duration_secs(), 0); // 0.1 s floors to 0
        let identity = corpus.to_identity(Some("en"));
        assert_eq!(identity.language.as_deref(), Some("en"));
        assert!(!identity.clips.is_empty());
    }
}
