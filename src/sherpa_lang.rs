//! Model-language → VoiceGarden bundle resolution: the validated-language
//! allowlist for Piper G2P under sherpa-onnx 2.x (#82, English migration
//! #92).
//!
//! sherpa-onnx 2.0 removed the espeak-ng phonemizer, so non-English
//! piper voices synthesize silence unless the caller supplies a phonetic
//! front-end (English voices degrade instead: the #66/#67 auto-lexicon
//! drops OOV/number words). The front-end (later phases, see
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
//! [`LangBundle::archive`] AND fetch by [`LangBundle::key`], never the
//! raw lang_code — never the `expanded.or_else(base)` order
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
    /// Download-enablement flag; `false` until the language's bake-off
    /// and validation pass (German deu: passed). Does NOT gate
    /// front-end activation.
    pub validated: bool,
}

/// The allowlist. Order is preference order: the first row matching a
/// language is the default. **German default = the expanded `deu`
/// bundle, decided by the Phase 4 bake-off** (recorded in
/// `SHERPA_NONENGLISH_G2P_PLAN.md`, runtime-path numbers): on a
/// frequency-weighted 50k-type German corpus, only 1.79% of token
/// occurrences leave the dictionary tier with deu vs 7.75% with de
/// (runtime drops are ≈0 for both — the rule fallback speaks WFST
/// misses); the fallback-share × WFST-holdout-error heuristic favors
/// deu too (0.25% vs 0.50% — a comparison signal, not a measured
/// error rate). The base `de` row stays second for comparison and as
/// a smaller-disk alternative (32.5 MB WFST vs 50.2 MB).
pub const LANG_BUNDLES: &[LangBundle] = &[
    LangBundle {
        bcp47: "de",
        archive: Archive::Expanded,
        key: "deu",
        validated: true, // Phase 5: bake-off + ear-check + CI passed
    },
    LangBundle {
        bcp47: "de",
        archive: Archive::Base,
        key: "de",
        validated: false,
    },
    // English migration to the codepoints path (#92): the eng-US
    // bundle is what the A/B + whisper round-trip validated (lexicon
    // path drops "42"/"1,000"; codepoints speaks both through the
    // dictionary). Expanded mirrors the German deu decision: the
    // distilled additions shrink the WFST-fallback share. The
    // auto-lexicon still generates (compat artifact for bare-sherpa
    // consumers) — the bundle is an upgrade, not a requirement.
    LangBundle {
        bcp47: "en",
        archive: Archive::Expanded,
        key: "eng-US",
        validated: true, // A/B + whisper round-trip (piper-en_US-amy-low)
    },
    // ── v0.4.0 locale flips (#92 finale): A/B + whisper round-trips
    // against the voicegarden-lexicons v0.4.0 bundles — 0 words
    // dropped on every probe; whisper-small/medium confirm the
    // numbers spoken in-language (es/ru exact, fr/it/pt number words
    // audible). The RussianNormalizer lands with these rows (comma
    // decimals, paucal agreement).
    LangBundle {
        bcp47: "fr",
        archive: Archive::Expanded,
        key: "fra",
        validated: true, // piper-fr_FR-siwis-medium: quarante-deux + un virgule cinq
    },
    LangBundle {
        bcp47: "es",
        archive: Archive::Expanded,
        // spa-ES is the default for ALL es voices incl. es_MX/es_AR —
        // Castilian phonemes (θ, no seseo) for LatAm voices; primary-
        // subtag resolution cannot split variants; a per-variant row
        // needs region-aware resolution (follow-up)
        key: "spa-ES",
        validated: true, // piper-es_ES-sharvard-medium: "Tengo 42 casas" exact
    },
    LangBundle {
        bcp47: "pt",
        archive: Archive::Expanded,
        // por-BR is the default for ALL pt voices incl. pt_PT —
        // Brazilian phonemes for European voices (audible vowel /
        // final-s differences); same variant-resolution follow-up
        key: "por-BR",
        validated: true, // piper-pt_BR-faber-medium: "Eu tenho 42 casas" (medium, exact)
    },
    LangBundle {
        bcp47: "it",
        archive: Archive::Expanded,
        key: "ita",
        validated: true, // piper-it_IT-paola-medium: "ho 42 case" (medium, exact)
    },
    LangBundle {
        bcp47: "ru",
        archive: Archive::Base,
        key: "ru",
        validated: true, // piper-ru_RU-dmitri-medium: "У меня 42 дома" exact
    },
    // ── batch-1 campaign flips (#92): probe 0-drop + whisper
    // round-trip + number-vocabulary audit against the v0.4.0 bundles;
    // cs/nl/pl/sk/sv emit dictionary-clean words, ca/da/sl ride the
    // WFST fallback (probe + audio validated); da/sv split the
    // one-and-twenty compounds (their dictionaries lack the closed
    // forms; documented in the normalizer tables)
    LangBundle {
        bcp47: "ca",
        archive: Archive::Base,
        key: "ca",
        validated: true, // piper-ca_ES-upc_pau: numbers via WFST, "42 cases" heard
    },
    LangBundle {
        bcp47: "cs",
        archive: Archive::Base,
        key: "cs",
        validated: true, // piper-cs_CZ-jirka: "Dobrý den, mám 40 domů" (medium, exact)
    },
    LangBundle {
        bcp47: "da",
        archive: Archive::Expanded,
        key: "dan",
        validated: true, // piper-da_DK-talesyntese: "jeg har fyre huse" audible
    },
    LangBundle {
        bcp47: "nl",
        archive: Archive::Expanded,
        key: "nld",
        validated: true, // piper-nl_NL-dii: "ik heb ... 40 huizen" (medium)
    },
    LangBundle {
        bcp47: "pl",
        archive: Archive::Expanded,
        key: "pol",
        validated: true, // piper-pl_PL-mc_speech: "Mam 40 domów" (medium, exact)
    },
    LangBundle {
        bcp47: "sk",
        archive: Archive::Expanded,
        key: "slk",
        validated: true, // piper-sk_SK-lili: number words audible
    },
    LangBundle {
        bcp47: "sl",
        archive: Archive::Expanded,
        key: "slv",
        validated: true, // piper-sl_SI-artur: "imam dva in štiri ..." audible
    },
    LangBundle {
        bcp47: "sv",
        archive: Archive::Expanded,
        key: "swe",
        validated: true, // piper-sv_SE-nst: hej/världen/två exact (single words)
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
/// first). Empty when the language has no bundle (any locale outside
/// de/en/fr/es/pt/it/ru — pl has a published-but-unvalidated bundle,
/// Hungarian has none at all).
#[must_use]
pub fn bundles_for_language(lang_code: &str) -> Vec<&'static LangBundle> {
    let primary = primary_subtag(lang_code);
    LANG_BUNDLES.iter().filter(|b| b.bcp47 == primary).collect()
}

/// The default bundle row for a model: the first allowlist row for the
/// model's language. `None` for languages without a bundle, bilingual
/// `<lang>_en` compounds (checked on the id, the only
/// id-based decision here), models with no language metadata, and
/// MULTI-language models: resolution requires every language entry to
/// share one primary subtag. A 31-language supertonic voice or a zh_en
/// compound must not get a German-only (or any single-language)
/// front-end — that silence is the wrong-bundle failure class this
/// module exists to prevent.
#[must_use]
pub fn default_bundle_for_model(model: &SherpaModelInfo) -> Option<&'static LangBundle> {
    Some(&LANG_BUNDLES[default_row_in(LANG_BUNDLES, model)?])
}

/// The index of the default row for `model` in `rows` — the shared
/// resolution shared by [`default_bundle_for_model`] and the
/// [`g2p_enabled_with`] seam (tests pin the `validated` half with
/// synthetic rows: the shipped unvalidated row is never a default, so
/// the live table cannot exercise it). Returns an index so callers
/// can bridge lifetimes however they need.
fn default_row_in(rows: &[LangBundle], model: &SherpaModelInfo) -> Option<usize> {
    if crate::sherpaonnx_engine::is_bilingual_en_compound(&model.id.to_ascii_lowercase()) {
        return None;
    }
    let first = model.language.first()?;
    let primary = primary_subtag(&first.lang_code);
    if model
        .language
        .iter()
        .any(|l| primary_subtag(&l.lang_code) != primary)
    {
        return None; // genuinely multi-language
    }
    rows.iter().position(|b| b.bcp47 == primary)
}

/// May a downloader offer this model for G2P-backed synthesis? True
/// when the model's language resolves to an allowlist row that has
/// PASSED validation — the Phase 5 download-enablement gate. This is
/// the one call downstream download surfaces should make (rather than
/// reaching into `LANG_BUNDLES`): front-end ACTIVATION still keys on
/// row presence, so a validated bundle also speaks when installed.
#[must_use]
pub fn g2p_enabled_for(model: &SherpaModelInfo) -> bool {
    g2p_enabled_with(LANG_BUNDLES, model)
}

/// [`g2p_enabled_for`] against an explicit allowlist — the seam that
/// pins the `validated` half of the gate: the shipped table can never
/// return an unvalidated DEFAULT row (its only unvalidated row is the
/// base `de` alternative), so a synthetic unvalidated default is the
/// only way to test that download enablement stays off until a
/// language passes validation.
#[must_use]
pub fn g2p_enabled_with(rows: &[LangBundle], model: &SherpaModelInfo) -> bool {
    validated_bundle_in(rows, model).is_some()
}

#[cfg(feature = "sherpa-g2p")]
pub(crate) fn validated_bundle_for_model(model: &SherpaModelInfo) -> Option<&'static LangBundle> {
    validated_bundle_in(LANG_BUNDLES, model)
}

fn validated_bundle_in<'a>(
    rows: &'a [LangBundle],
    model: &SherpaModelInfo,
) -> Option<&'a LangBundle> {
    Some(&rows[default_row_in(rows, model)?]).filter(|row| row.validated)
}

#[cfg(test)]
mod tests {
    /// Pin: review deliberately on registry bumps (batch-1 flips).
    const EXPECTED_ENABLED_SINGLE: usize = 417;

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
    fn download_enablement_gates_on_validation() {
        let m = model("piper-de_DE-thorsten-medium", "de", "DE");
        assert!(g2p_enabled_for(&m));
        assert!(g2p_enabled_for(&model("piper-en_US-amy-low", "en", "US")));
        // pl flipped with the batch-1 campaign (#92)
        assert!(g2p_enabled_for(&model(
            "piper-pl_PL-gosia-medium",
            "pl",
            "PL"
        )));
        assert!(!g2p_enabled_for(&model("melo-zh_en-melo-tts", "zh", "CN")));

        // the seam: a synthetic UNVALIDATED default row must disable —
        // the shipped table can never exercise this (its only
        // unvalidated row is never a default), and it is exactly the
        // state a NEW language ships in
        let unvalidated = [LangBundle {
            bcp47: "de",
            archive: Archive::Expanded,
            key: "deu",
            validated: false,
        }];
        assert!(!g2p_enabled_with(&unvalidated, &m));
        assert!(validated_bundle_in(&unvalidated, &m).is_none());
        let validated = [LangBundle {
            bcp47: "de",
            archive: Archive::Expanded,
            key: "deu",
            validated: true,
        }];
        assert!(g2p_enabled_with(&validated, &m));
        assert_eq!(validated_bundle_in(&validated, &m).unwrap().key, "deu");
    }

    #[test]
    fn thorsten_resolves_to_default_expanded_row() {
        // registry spelling: piper ids embed de_DE, lang_code is bare "de";
        // the Phase 4 bake-off made deu (expanded) the default
        let m = model("piper-de_DE-thorsten-medium", "de", "DE");
        let b = default_bundle_for_model(&m).expect("thorsten resolves");
        assert_eq!((b.bcp47, b.key), ("de", "deu"));
        assert_eq!(b.archive, Archive::Expanded);
    }

    #[test]
    fn german_rows_preference_order_deu_validated() {
        let rows = bundles_for_language("de");
        assert_eq!(rows.len(), 2, "both German rows ship");
        assert_eq!((rows[0].key, rows[0].archive), ("deu", Archive::Expanded));
        assert_eq!((rows[1].key, rows[1].archive), ("de", Archive::Base));
        assert!(rows[0].validated, "deu validated: Phase 5");
        assert!(
            !rows[1].validated,
            "base de stays the unvalidated alternative"
        );
    }

    #[test]
    fn bilingual_compounds_are_excluded_by_id() {
        // real registry ids; these are also excluded by the metadata
        // guard (they list lang+en) — regression coverage for the shape
        let m = model("melo-zh_en-melo-tts", "zh", "CN");
        assert!(default_bundle_for_model(&m).is_none());
        let m = model("kokoro-zh_en-multi-lang", "zh", "CN");
        assert!(default_bundle_for_model(&m).is_none());
        // the real matcha-fa_en-khadijah lists fa+en; the synthetic
        // single-fa shape below isolates the ID check: no fa rows exist,
        // so without the id check this would still be None — it is the
        // de_en case underneath that makes the id branch decisive
        let m = model("matcha-fa_en-khadijah", "fa", "IR");
        assert!(default_bundle_for_model(&m).is_none());
        // DECISIVE id-check case: compound id + single language that HAS
        // rows — only is_bilingual_en_compound excludes this. If this
        // assertion is removed, delete the id check in
        // default_bundle_for_model and the suite stays green (mutation
        // hole); this fixture is what pins the branch.
        let m = model("piper-de_en-hybrid", "de", "DE");
        assert!(default_bundle_for_model(&m).is_none());
        // no false positive: `_en` followed by a letter is not a compound
        // (mms_eng's registry lang_code is "eng", ISO 639-2/B — which
        // also must NOT alias to a hypothetical "en" row: primary-subtag
        // matching is exact, not prefix)
        let m = model("mms_eng", "eng", "US");
        assert!(default_bundle_for_model(&m).is_none());
        assert!(bundles_for_language("eng").is_empty());
    }

    #[test]
    fn multilingual_non_compound_models_resolve_none() {
        // supertonic-3-multilingual lists 31 languages; with German first
        // it must STILL resolve None — a German-only front-end on a
        // 31-language voice is the wrong-bundle failure this module
        // exists to prevent
        let mut m = model("supertonic-3-multilingual", "de", "DE");
        m.language.push(SherpaLanguage {
            lang_code: "en".to_string(),
            language_name: "English".to_string(),
            country: "US".to_string(),
        });
        assert!(default_bundle_for_model(&m).is_none());
    }

    #[test]
    fn english_and_unknown_languages_resolve_correctly() {
        // English now has a validated row (the codepoints migration)
        let m = model("piper-en_US-amy-low", "en", "US");
        let b = default_bundle_for_model(&m).expect("english resolves");
        assert_eq!((b.key, b.archive), ("eng-US", Archive::Expanded));
        // Polish resolves since the batch-1 flips
        let m = model("piper-pl_PL-gosia-medium", "pl", "PL");
        let b = default_bundle_for_model(&m).expect("polish resolves");
        assert_eq!((b.key, b.archive), ("pol", Archive::Expanded));
        // Hungarian still has no bundle
        let m = model("piper-hu_HU-berta-medium", "hu", "HU");
        assert!(default_bundle_for_model(&m).is_none());
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
        // whitespace-only codes resolve nothing
        assert!(bundles_for_language("   ").is_empty());
    }

    /// The full pinned registry, the same convention as
    /// `test_lexicon_gate_against_full_registry`: pins the resolution
    /// contract against the real corpus (1780 models) so a registry bump
    /// that introduces a multilingual German voice (or a new compound
    /// spelling) fails here instead of surprising a user.
    #[test]
    fn resolution_against_full_registry() {
        // English vits models now resolve to the en bundle row
        let mut de_single = 0;
        for (id, m) in sherpa_onnx_models::models() {
            let info = SherpaModelInfo {
                id: id.clone(),
                model_type: m.model_type.clone(),
                engines: String::new(),
                name: String::new(),
                language: m
                    .language
                    .iter()
                    .map(|l| SherpaLanguage {
                        lang_code: l.lang_code.clone(),
                        language_name: l.language_name.clone(),
                        country: l.country.clone(),
                    })
                    .collect(),
                sample_rate: m.sample_rate,
                num_speakers: m.num_speakers,
                quality: String::new(),
                url: String::new(),
                compression: false,
                filesize_mb: 0.0,
                license: String::new(),
                license_url: String::new(),
            };
            let resolved = default_bundle_for_model(&info);
            let single_enabled = info.language.len() == 1
                && matches!(
                    primary_subtag(&info.language[0].lang_code).as_str(),
                    "de" | "en"
                        | "fr"
                        | "es"
                        | "pt"
                        | "it"
                        | "ru"
                        | "ca"
                        | "cs"
                        | "da"
                        | "nl"
                        | "pl"
                        | "sk"
                        | "sl"
                        | "sv"
                )
                && !crate::sherpaonnx_engine::is_bilingual_en_compound(&id.to_ascii_lowercase());
            if single_enabled {
                de_single += 1;
                let b = resolved.expect("single-language enabled model resolves");
                assert!(
                    matches!(
                        (b.key, b.archive),
                        (
                            "deu"
                                | "eng-US"
                                | "fra"
                                | "spa-ES"
                                | "por-BR"
                                | "ita"
                                | "dan"
                                | "nld"
                                | "pol"
                                | "slk"
                                | "slv"
                                | "swe",
                            Archive::Expanded
                        ) | ("ru" | "ca" | "cs", Archive::Base)
                    ),
                    "id {id}"
                );
            } else {
                assert!(resolved.is_none(), "id {id} must not resolve");
            }
        }
        // the validated-locale fleets (batch-1 flips added
        // ca/cs/da/nl/pl/sk/sl/sv). Resolution is NOT family-gated —
        // the engine's compat-lexicon pin covers the vits-family
        // subset separately. Review on registry bumps.
        assert_eq!(
            de_single, EXPECTED_ENABLED_SINGLE,
            "validated-locale single-language model count moved"
        );
    }

    #[test]
    fn allowlist_rows_match_their_archive_key_pairs() {
        // the archive trap, stated as data: each key must belong to its
        // named archive (de↔base, deu/eng-US↔expanded). If a row ever
        // pairs a key with the wrong archive, fetch would silently
        // resolve the other bundle via bcp47 matching.
        for b in LANG_BUNDLES {
            match b.archive {
                Archive::Base => {
                    assert!(matches!(b.key, "de" | "ru" | "ca" | "cs"), "{}", b.key);
                }
                Archive::Expanded => assert!(
                    matches!(
                        b.key,
                        "deu"
                            | "eng-US"
                            | "fra"
                            | "spa-ES"
                            | "por-BR"
                            | "ita"
                            | "dan"
                            | "nld"
                            | "pol"
                            | "slk"
                            | "slv"
                            | "swe"
                    ),
                    "{}",
                    b.key
                ),
            }
            assert_eq!(primary_subtag(b.bcp47), b.bcp47, "rows use minimal tags");
        }
    }
}
