//! Model-language → VoiceGarden bundle resolution: the validated-language
//! allowlist for non-English Piper G2P under sherpa-onnx 2.x (#82).
//!
//! sherpa-onnx 2.0 removed the espeak-ng phonemizer, so non-English
//! piper voices synthesize silence unless the caller supplies a phonetic
//! front-end. The front-end (later phases, see
//! `SHERPA_NONENGLISH_G2P_PLAN.md`) is built from the published
//! voicegarden-lexicons bundles; this module is the single gate that
//! decides which model languages get one.
//!
//! Resolution reads REGISTRY METADATA (`SherpaModelInfo::language`), never
//! the model id — the id is consulted only to exclude bilingual
//! `<lang>_en` compounds (`kokoro-zh_en-*`), whose multi-language
//! front-end is not served by any single-language bundle.
//!
//! # The archive trap
//!
//! The two German bundles live in DIFFERENT manifests: `de` only in the
//! base `lexicons.json`, `deu` only in `expanded.json`. The
//! voicegarden-lexicons crate matches entries on `lang` OR `bcp47`, so
//! asking the expanded archive for `"de"` silently resolves `deu`.
//! Consumers must construct exactly the archive named in
//! [`LangBundle::archive`] — never the `expanded.or_else(base)` order
//! `floravox_engine` uses for its own purposes.
//!
//! # Lifecycle
//!
//! Every row ships with `validated: false`; Phase 4's bundle bake-off
//! picks the default row (array order is the preference order until
//! then), and Phase 5 flips `validated` on the winner to enable download
//! surfacing. `validated` gates download enablement ONLY — the G2P
//! front-end activates on row presence so it can be exercised end-to-end
//! before validation completes.

use crate::types::SherpaModelInfo;

/// Which voicegarden-lexicons manifest a bundle lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archive {
    /// The base `lexicons.json` (14 languages, gruut-derived).
    Base,
    /// The expanded `expanded.json` (distilled additions; note its
    /// distilled entries carry `distill_agreement` but no held-out
    /// audit — see the plan's bake-off table).
    Expanded,
}

/// One validated-language row: a voicegarden-lexicons bundle a model
/// language may use as its G2P source.
#[derive(Debug)]
pub struct LangBundle {
    /// Minimal BCP-47 tag ("de"); matched against the PRIMARY subtag of
    /// the model's registry `lang_code`.
    pub bcp47: &'static str,
    /// The manifest the bundle lives in — fetch must construct exactly
    /// this archive (see the module's archive trap).
    pub archive: Archive,
    /// Bundle key inside that archive ("de" | "deu" — keys differ
    /// between archives).
    pub key: &'static str,
    /// Phase-5 download-enablement flag; false until the Phase 4 bake-off
    /// and validation pass. Does NOT gate front-end activation.
    pub validated: bool,
}

/// The allowlist. Order is preference order: the first row matching a
/// language is the default until Phase 4's bake-off reorders it.
///
/// German ships both rows (base `de`, expanded `deu`) so the bake-off can
/// run either; the numbers that motivated keeping both are recorded in
/// `SHERPA_NONENGLISH_G2P_PLAN.md` (base: 277,912 entries, Phonetisaurus
/// 93.6% holdout, 32.5 MB; expanded: 333,945 entries but distilled adds
/// at 0.711 agreement with no audit, Phonetisaurus 86.1%, 50.2 MB).
pub const LANG_BUNDLES: &[LangBundle] = &[
    LangBundle {
        bcp47: "de",
        archive: Archive::Base,
        key: "de",
        validated: false,
    },
    LangBundle {
        bcp47: "de",
        archive: Archive::Expanded,
        key: "deu",
        validated: false,
    },
];

/// Primary language subtag of a lang code, lowercased: `"de"` from
/// `"de"`, `"de-DE"`, or `"de_DE"`. Empty for empty/blank input.
fn primary_subtag(lang_code: &str) -> String {
    lang_code
        .trim()
        .split(['-', '_'])
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// All allowlist rows for a language, in preference order (default
/// first). Empty when the language has no bundle — English today, which
/// stays on the #66/#67 lexicon path.
#[must_use]
pub fn bundles_for_language(lang_code: &str) -> Vec<&'static LangBundle> {
    let primary = primary_subtag(lang_code);
    LANG_BUNDLES.iter().filter(|b| b.bcp47 == primary).collect()
}

/// The default bundle row for a model: the first allowlist row for the
/// model's first registry language. `None` for languages without a
/// bundle, bilingual `<lang>_en` compounds (checked on the id, the only
/// id-based decision here), and models with no language metadata.
///
/// Multi-language models use their FIRST language — piper voices carry
/// exactly one; genuinely multi-language voices (zh_en, fa_en) are the
/// compounds excluded above.
#[must_use]
pub fn default_bundle_for_model(model: &SherpaModelInfo) -> Option<&'static LangBundle> {
    if crate::sherpaonnx_engine::is_bilingual_en_compound(&model.id.to_ascii_lowercase()) {
        return None;
    }
    let first = model.language.first()?;
    bundles_for_language(&first.lang_code).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SherpaLanguage;

    /// A registry-shaped model entry. Real registry shapes only: bare
    /// `lang_code`s (`"de"`) with the country as a separate field — the
    /// pinned registry has zero underscore-form lang_codes.
    fn model(id: &str, lang_code: &str, country: &str) -> SherpaModelInfo {
        SherpaModelInfo {
            id: id.to_string(),
            model_type: "vits".to_string(),
            engines: "sherpa-onnx".to_string(),
            name: id.to_string(),
            language: vec![SherpaLanguage {
                lang_code: lang_code.to_string(),
                language_name: lang_code.to_string(),
                country: country.to_string(),
            }],
            sample_rate: 22_050,
            num_speakers: 1,
            quality: "medium".to_string(),
            url: String::new(),
            compression: true,
            filesize_mb: 64.1,
            license: "MIT".to_string(),
            license_url: String::new(),
        }
    }

    #[test]
    fn thorsten_resolves_to_default_base_row() {
        // registry spelling: piper ids embed de_DE, lang_code is bare "de"
        let m = model("piper-de_DE-thorsten-medium", "de", "DE");
        let b = default_bundle_for_model(&m).expect("thorsten resolves");
        assert_eq!((b.bcp47, b.key), ("de", "de"));
        assert_eq!(b.archive, Archive::Base);
    }

    #[test]
    fn german_rows_ship_both_unvalidated_in_preference_order() {
        let rows = bundles_for_language("de");
        assert_eq!(rows.len(), 2, "both German rows ship for the bake-off");
        assert_eq!((rows[0].key, rows[0].archive), ("de", Archive::Base));
        assert_eq!((rows[1].key, rows[1].archive), ("deu", Archive::Expanded));
        assert!(
            rows.iter().all(|r| !r.validated),
            "Phase 1: nothing is validated until the Phase 4 bake-off"
        );
    }

    #[test]
    fn bilingual_compounds_are_excluded_by_id() {
        // zh_en compound: an English-only or single-bundle front-end would
        // be wrong even though the id contains "en" and the model lists
        // Chinese
        let m = model("vits-melo-tts-zh_en", "zh", "CN");
        assert!(default_bundle_for_model(&m).is_none());
        let m = model("kokoro-zh_en-v1_0", "zh", "CN");
        assert!(default_bundle_for_model(&m).is_none());
    }

    #[test]
    fn english_and_unknown_languages_have_no_rows() {
        // English stays on the #66/#67 lexicon path — no rows here
        let m = model("piper-en_US-amy-low", "en", "US");
        assert!(default_bundle_for_model(&m).is_none());
        // Polish piper voices exist but no pl bundle is published
        let m = model("piper-pl_PL-gosia-medium", "pl", "PL");
        assert!(default_bundle_for_model(&m).is_none());
        assert!(bundles_for_language("pl").is_empty());
    }

    #[test]
    fn no_language_metadata_resolves_none() {
        let mut m = model("piper-de_DE-thorsten-medium", "de", "DE");
        m.language.clear();
        assert!(default_bundle_for_model(&m).is_none());
    }

    #[test]
    fn primary_subtag_matching_is_tolerant() {
        // bare code (registry), region form, underscore form, casing
        for code in ["de", "de-DE", "de_DE", "DE"] {
            assert_eq!(
                bundles_for_language(code).len(),
                2,
                "lang_code {code:?} should resolve to the German rows"
            );
        }
        // primary subtag only: a pt-BR voice must not match a "pt" row
        // (none ships yet — the assertion documents the semantics)
        assert_eq!(primary_subtag("pt-BR"), "pt");
        assert_eq!(primary_subtag(""), "");
    }

    #[test]
    fn allowlist_rows_match_their_archive_key_pairs() {
        // the archive trap, stated as data: each key must belong to its
        // named archive (de↔base, deu↔expanded). If a row ever pairs a
        // key with the wrong archive, fetch would silently resolve the
        // other bundle via bcp47 matching.
        for b in LANG_BUNDLES {
            match b.archive {
                Archive::Base => assert!(!b.key.contains("deu"), "{:?}", b.key),
                Archive::Expanded => assert_eq!(b.key, "deu"),
            }
            assert_eq!(primary_subtag(b.bcp47), b.bcp47, "rows use minimal tags");
        }
    }
}
