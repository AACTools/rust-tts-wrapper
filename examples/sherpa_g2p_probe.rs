//! #82 A/B probe: the codepoint contract ear-check.
//!
//! Generates the SAME German sentence through both front-ends so a
//! human can compare pause placement and prosody (non-empty audio
//! alone cannot catch a wrong contract — `SHERPA_NONENGLISH_G2P_PLAN.md`
//! Phase 0):
//!
//! - `ab_text.wav`    — sherpa's lexicon front-end: a converted
//!   `lexicon.txt` (Phase 2 compat) beside the model, plain text in.
//! - `ab_codepoints.wav` — the wrapper G2P front-end: dictionary +
//!   Phonetisaurus mapped through the vocabulary ladder, handed to
//!   sherpa as `phoneme_codepoints` (Phase 3).
//!
//! Also writes `ab_codepoints.txt` — the phoneme stream per sentence —
//! for inspection.
//!
//! ```text
//! cargo run --release --features sherpaonnx,sherpa-g2p --example sherpa_g2p_probe -- \
//!   <model-dir> <bundle-dir> <out-dir> ["Guten Morgen! ..."]
//! ```
//!
//! `<model-dir>` holds `*.onnx` + `tokens.txt` (espeak-ng-data ignored);
//! `<bundle-dir>` holds `de.fst`/`de.pho` (+ optional
//! `phonetisaurus.fst`) — i.e. the unpacked voicegarden `de` bundle.

use std::path::{Path, PathBuf};

use rust_tts_wrapper::sherpa_g2p::G2pFrontend;
use rust_tts_wrapper::sherpa_lexicon::ensure_compat_lexicon_from;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(model_dir), Some(bundle_dir), Some(out_dir)) = (
        args.next().map(PathBuf::from),
        args.next().map(PathBuf::from),
        args.next().map(PathBuf::from),
    ) else {
        eprintln!("usage: sherpa_g2p_probe <model-dir> <bundle-dir> <out-dir> [text]");
        std::process::exit(2);
    };
    let text = args.next().unwrap_or_else(|| {
        "Guten Morgen! Ich habe zweiundvierzig Häuser in Berlin gesehen.".to_string()
    });
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let tokens = model_dir.join("tokens.txt");
    let model = find_onnx(&model_dir).expect("an .onnx model in the model dir");
    println!("model:  {}", model.display());
    println!("tokens: {}", tokens.display());
    println!("bundle: {}", bundle_dir.display());
    println!("text:   {text}");

    // ── A: lexicon front-end (converted lexicon.txt beside the model) ──
    let a_lexicon = match ensure_compat_lexicon_from(
        &model_dir,
        &bundle_dir.join("lexicon.txt"),
        &out_dir,
        "probe",
    ) {
        Ok(Some(res)) => {
            println!(
                "A: generated compat lexicon — {} entries ({} skipped) at {}",
                res.written,
                res.skipped,
                res.path.display()
            );
            res.path
        }
        Ok(None) => {
            println!("A: model dir already has a lexicon");
            model_dir.join("lexicon.txt")
        }
        Err(e) => {
            eprintln!("A: lexicon generation failed: {e}");
            std::process::exit(1);
        }
    };
    // the lexicon path legitimately fails when every word is OOV for
    // it (sherpa cannot tokenize) — exactly the #82 case; print why
    // and still produce the B side, which is the point of the compare
    let (wav, rate) =
        synthesize(&model, &tokens, Some(a_lexicon), &text, None).unwrap_or_else(|| {
            println!("A: no audio from the lexicon path (all words OOV?) — see B");
            (Vec::new(), 22_050)
        });
    let a_path = out_dir.join("ab_text.wav");
    if !wav.is_empty() {
        write_wav(&a_path, &wav, rate);
        println!(
            "A: {} ({} samples @ {rate} Hz)",
            a_path.display(),
            wav.len()
        );
    }

    // ── B: wrapper G2P → phoneme_codepoints ──
    let mut frontend = G2pFrontend::open(&bundle_dir, "de", &tokens).expect("G2P front-end");
    let result = frontend.phonemize(&text);
    println!(
        "B: {} words, {} dropped, {} sentences",
        result.words,
        result.dropped,
        result.sentences.len()
    );
    let mut stream = String::new();
    for (i, sentence) in result.sentences.iter().enumerate() {
        let s: String = sentence
            .iter()
            .filter_map(|&cp| char::from_u32(cp.cast_unsigned()))
            .collect();
        println!("  sentence {i}: {s}");
        stream.push_str(&s);
        stream.push('\n');
    }
    std::fs::write(out_dir.join("ab_codepoints.txt"), &stream).expect("write stream");

    let (wav, rate) =
        synthesize(&model, &tokens, None, "", Some(result.sentences)).expect("B audio");
    let b_path = out_dir.join("ab_codepoints.wav");
    write_wav(&b_path, &wav, rate);
    println!(
        "B: {} ({} samples @ {rate} Hz)",
        b_path.display(),
        wav.len()
    );

    println!("\nListen to both; they should sound equivalent (B may differ slightly on OOV words — it SPEAKS them, A drops them).");
}

fn find_onnx(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "onnx"))
}

/// Returns `(samples, sample_rate)` — the rate comes from the model.
/// `None` when sherpa produced no audio (e.g. the lexicon path with
/// all-OOV text).
fn synthesize(
    model: &Path,
    tokens: &Path,
    lexicon: Option<PathBuf>,
    text: &str,
    codepoints: Option<Vec<Vec<i32>>>,
) -> Option<(Vec<f32>, i32)> {
    let config = sherpa_onnx::OfflineTtsConfig {
        model: sherpa_onnx::OfflineTtsModelConfig {
            vits: sherpa_onnx::OfflineTtsVitsModelConfig {
                model: Some(model.to_string_lossy().to_string()),
                tokens: Some(tokens.to_string_lossy().to_string()),
                lexicon: lexicon.map(|p| p.to_string_lossy().to_string()),
                ..Default::default()
            },
            num_threads: 2,
            debug: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let tts = sherpa_onnx::OfflineTts::create(&config).expect("create OfflineTts");
    let gen = sherpa_onnx::GenerationConfig {
        phoneme_codepoints: codepoints,
        ..Default::default()
    };
    let audio =
        tts.generate_with_config(text, &gen, None::<&mut dyn FnMut(&[f32], f32) -> bool>)?;
    Some((audio.samples().to_vec(), audio.sample_rate()))
}

/// Minimal 16-bit mono WAV writer (the probe's output shape).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn write_wav(path: &Path, samples: &[f32], sample_rate: i32) {
    use std::io::Write as _;
    let sample_rate = sample_rate.max(1) as u32; // wav header is u32
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        // clamped to ±1 first, so the truncating cast only rounds
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    let data_len = u32::try_from(pcm.len()).expect("wav under 4 GB");
    let mut f = std::fs::File::create(path).expect("create wav");
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap(); // fmt chunk size
    f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    f.write_all(&1u16.to_le_bytes()).unwrap(); // mono
    f.write_all(&sample_rate.to_le_bytes()).unwrap();
    f.write_all(&(sample_rate * 2).to_le_bytes()).unwrap(); // byte rate
    f.write_all(&2u16.to_le_bytes()).unwrap(); // block align
    f.write_all(&16u16.to_le_bytes()).unwrap(); // bits
    f.write_all(b"data").unwrap();
    f.write_all(&data_len.to_le_bytes()).unwrap();
    f.write_all(&pcm).unwrap();
}
