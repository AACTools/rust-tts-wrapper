//! Pure (non-FFI) helpers for the qwen3-local engine — ungated so they
//! compile and unit-test in every CI build, even without the C++ library
//! the engine itself links.

/// Speaker-embedding size reported by the C++ pipeline (ECAPA x-vector).
pub(crate) const EMBEDDING_SIZE: usize = 1024;

/// Qwen3-TTS's ten supported languages and their codec language token
/// IDs (from qwen3-tts.cpp's main.cpp — the IDs are model constants).
#[must_use]
pub fn supported_languages() -> &'static [(&'static str, &'static str, &'static str, i32)] {
    // (id, bcp47, display, language_id) — id is the voice string users
    // pass to set_voice/speak.
    &[
        ("en", "en", "English", 2050),
        ("zh", "zh-CN", "Chinese (Mandarin)", 2055),
        ("ja", "ja", "Japanese", 2058),
        ("ko", "ko", "Korean", 2064),
        ("de", "de", "German", 2053),
        ("fr", "fr", "French", 2061),
        ("ru", "ru", "Russian", 2069),
        ("es", "es", "Spanish", 2054),
        ("it", "it", "Italian", 2070),
        ("pt", "pt", "Portuguese", 2071),
    ]
}

#[must_use]
pub fn id_to_iso639_3(id: &str) -> String {
    match id {
        "zh" => "zho",
        "ja" => "jpn",
        "ko" => "kor",
        "de" => "deu",
        "fr" => "fra",
        "ru" => "rus",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        _ => "eng",
    }
    .to_string()
}

#[must_use]
pub fn language_id_for(voice_or_lang: &str) -> Option<i32> {
    let needle = voice_or_lang.trim().to_lowercase();
    let needle = needle.split(['-', '_']).next().unwrap_or(&needle);
    supported_languages()
        .iter()
        .find(|(id, _, _, _)| *id == needle)
        .map(|(_, _, _, lid)| *lid)
}

/// Decode an `emb:<base64>` voice string into raw embedding floats.
#[cfg(any(feature = "qwen3-local", feature = "cloud"))]
/// # Errors
/// When the payload is not valid base64 or its byte length is not a
/// multiple of 4 (f32 LE samples).
pub fn decode_embedding(v: &str) -> Result<Vec<f32>, String> {
    use base64::Engine as _;
    let b64 = v.strip_prefix("emb:").unwrap_or(v);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| e.to_string())?;
    if bytes.len() % 4 != 0 {
        return Err("embedding byte length not a multiple of 4".into());
    }
    let (chunks, _rem) = bytes.as_chunks::<4>();
    Ok(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// Encode raw embedding floats into an `emb:<base64>` voice string.
#[cfg(any(feature = "qwen3-local", feature = "cloud"))]
#[must_use]
pub fn encode_embedding(embedding: &[f32]) -> String {
    use base64::Engine as _;
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for f in embedding {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    format!(
        "emb:{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Linear PCM16 gain, clamped to the i16 range.
#[must_use]
pub fn apply_gain(pcm: &[u8], gain: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len());
    for s in pcm.as_chunks::<2>().0 {
        let sample = i16::from_le_bytes(*s);
        #[allow(clippy::cast_possible_truncation)]
        let scaled = (f32::from(sample) * gain).clamp(-32767.0, 32767.0) as i16;
        out.extend_from_slice(&scaled.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_table_is_complete_and_unique() {
        let langs = supported_languages();
        assert_eq!(langs.len(), 10);
        let mut ids: Vec<_> = langs.iter().map(|(id, _, _, _)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 10);
        // Qwen's documented set.
        for expected in ["en", "zh", "ja", "ko", "de", "fr", "ru", "es", "it", "pt"] {
            assert!(ids.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn language_lookup_accepts_locale_forms() {
        assert_eq!(language_id_for("fr"), Some(2061));
        assert_eq!(language_id_for("FR"), Some(2061));
        assert_eq!(language_id_for("pt-BR"), Some(2071));
        assert_eq!(language_id_for("en_US"), Some(2050));
        assert_eq!(language_id_for("english"), None); // ids only, per upstream CLI contract
        assert_eq!(id_to_iso639_3("zh"), "zho");
        assert_eq!(id_to_iso639_3("en"), "eng");
    }

    #[cfg(any(feature = "qwen3-local", feature = "cloud"))]
    #[test]
    fn embedding_codec_round_trips() {
        let emb: Vec<f32> = (0..EMBEDDING_SIZE)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let v = i as f32;
                v * 0.25 - 128.0
            })
            .collect();
        let encoded = encode_embedding(&emb);
        assert!(encoded.starts_with("emb:"));
        let decoded = decode_embedding(&encoded).expect("roundtrip");
        assert_eq!(decoded.len(), emb.len());
        for (a, b) in emb.iter().zip(&decoded) {
            assert!((a - b).abs() < f32::EPSILON);
        }
        assert!(decode_embedding("emb:!!!not-base64!!!").is_err());
        assert!(decode_embedding("emb:AAA").is_err()); // odd byte count
    }

    #[test]
    fn gain_scales_and_clamps() {
        let pcm = i16::from_le_bytes([0x00, 0x40]).to_le_bytes(); // 16384
        let out = apply_gain(&pcm, 0.5);
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), 8192);
        // Gain beyond i16 range clamps, never wraps.
        let loud = apply_gain(&pcm, 8.0);
        assert_eq!(i16::from_le_bytes([loud[0], loud[1]]), 32767);
        let neg = apply_gain(&i16::from_le_bytes([0x00, 0xC0]).to_le_bytes(), 0.0);
        assert_eq!(i16::from_le_bytes([neg[0], neg[1]]), 0);
    }
}
