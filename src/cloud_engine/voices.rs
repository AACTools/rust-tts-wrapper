// The split modules resolve shared names through the parent glob.
#![allow(clippy::wildcard_imports)]

use super::*;

/// Map Azure voices JSON array to unified voices.
pub(crate) fn map_azure_voices(json: &[serde_json::Value]) -> Vec<Voice> {
    let mut voices = Vec::new();
    for v in json {
        let Some(short_name) = v.get("ShortName").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = v
            .get("DisplayName")
            .and_then(|v| v.as_str())
            .unwrap_or(short_name)
            .to_string();
        let gender_raw = v.get("Gender").and_then(|v| v.as_str()).unwrap_or("");
        let locale = v.get("Locale").and_then(|v| v.as_str()).unwrap_or("en-US");

        voices.push(Voice {
            id: short_name.to_string(),
            name,
            gender: normalize_gender(gender_raw),
            provider: "azure".to_string(),
            language_codes: vec![LanguageCode {
                bcp47: locale.to_string(),
                iso639_3: locale.split('-').next().unwrap_or("en").to_string(),
                display: v
                    .get("LocaleName")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| crate::types::locale_display_name(locale), String::from),
            }],
        });
    }
    voices
}

/// Map Google voices JSON array to unified voices.
pub(crate) fn map_google_voices(json: &[serde_json::Value]) -> Vec<Voice> {
    let mut voices = Vec::new();
    for v in json {
        let Some(name) = v.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        // Google returns bare named voices (e.g. "Algieba", "Aoede") for
        // Gemini/Chirp3-HD alongside the locale-prefixed duplicates (e.g.
        // "en-US-Chirp3-HD-Algieba"). The bare names fail at synthesis
        // ("requires a model name"). Skip them — the prefixed versions
        // work correctly.
        if !name.contains('-') {
            continue;
        }
        let gender_raw = v.get("ssmlGender").and_then(|v| v.as_str()).unwrap_or("");
        let lang_codes =
            v.get("languageCodes")
                .and_then(|v| v.as_array())
                .map_or(Vec::new(), |arr| {
                    arr.iter()
                        .filter_map(|c| {
                            let code = c.as_str()?;
                            Some(LanguageCode {
                                iso639_3: code.split('-').next()?.to_string(),
                                bcp47: code.to_string(),
                                display: code.to_string(),
                            })
                        })
                        .collect::<Vec<_>>()
                });

        voices.push(Voice {
            id: name.to_string(),
            name: name.to_string(),
            gender: normalize_gender(gender_raw),
            provider: "google".to_string(),
            language_codes: lang_codes,
        });
    }
    voices
}

/// Map Gemini Extended Voice Library JSON to unified voices.
///
/// `GET /v1beta/voices` returns `{ "voices": [ ... ] }` with rich metadata
/// per voice: `{ "id": "kore", "display_name": "Kore", "language_code":
/// "en-US", "accent": "...", "persona": "...", "gender": "..." }`. The
/// same voice IDs work in `speech_config` — including voice design IDs
/// (`voice_...`) and replication keys (`voicekey_...`) when present.
pub(crate) fn map_gemini_voices(json: &[serde_json::Value]) -> Vec<Voice> {
    let mut voices = Vec::new();
    for v in json {
        let Some(id) = v.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = v
            .get("display_name")
            .or_else(|| v.get("displayName"))
            .and_then(|v| v.as_str())
            .unwrap_or(id)
            .to_string();
        let gender_raw = v.get("gender").and_then(|v| v.as_str()).unwrap_or("");
        // Persona enriches the display name so a voice picker can
        // differentiate the 50+ prebuilt voices ("Kore — Firm, ... ").
        let persona = v.get("persona").and_then(|v| v.as_str()).unwrap_or("");
        let display = if persona.is_empty() {
            name.clone()
        } else {
            format!("{name} — {persona}")
        };
        let locale = v
            .get("language_code")
            .or_else(|| v.get("languageCode"))
            .and_then(|v| v.as_str())
            .unwrap_or("en-US");
        voices.push(Voice {
            id: id.to_string(),
            name: display,
            gender: normalize_gender(gender_raw),
            provider: "gemini".to_string(),
            language_codes: vec![LanguageCode {
                iso639_3: locale.split('-').next().unwrap_or("en").to_string(),
                bcp47: locale.to_string(),
                display: crate::types::locale_display_name(locale),
            }],
        });
    }
    voices
}

/// Generic voice-list parser used by every provider that doesn't have a
/// dedicated mapper (i.e. everything except Azure and Google).
///
/// Handles field-name variation across providers:
/// - `id` / `voice_id` / `VoiceId` / `name` / `Name` for the voice id
/// - `name` / `Name` (falling back to id) for the display name
/// - `gender` / `Gender` / `labels.gender` (ElevenLabs stores gender in a
///   `labels` object) for gender
/// - `language_code` / `LanguageCode` / `language` / `lang` /
///   `labels.language` for the primary language
///
/// Extracted from `get_voices()` so it can be unit-tested directly with
/// representative JSON samples from each provider.
pub(crate) fn map_generic_voices(provider: &str, json: &[serde_json::Value]) -> Vec<Voice> {
    json.iter()
        .filter_map(|v| {
            let id = v
                .get("id")
                .or_else(|| v.get("voice_id"))
                .or_else(|| v.get("VoiceId"))
                .or_else(|| v.get("Id"))
                .or_else(|| v.get("name"))
                .or_else(|| v.get("Name"))?
                .as_str()?;
            let name = v
                .get("name")
                .or_else(|| v.get("Name"))
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string();

            // Gender resolution order. ElevenLabs stores gender inside a
            // `labels` object — handle that explicitly.
            let gender_str = v
                .get("gender")
                .or_else(|| v.get("Gender"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    v.get("labels").and_then(|labels| {
                        if let Some(obj) = labels.as_object() {
                            obj.get("gender")?.as_str().map(str::to_string)
                        } else {
                            labels.as_str().map(std::string::ToString::to_string)
                        }
                    })
                })
                .unwrap_or_default();

            // Language code resolution. Polly uses `LanguageCode`; ElevenLabs
            // uses `labels.language`; others use `language` or `lang`.
            let lang = v
                .get("language_code")
                .or_else(|| v.get("LanguageCode"))
                .or_else(|| v.get("language"))
                .or_else(|| v.get("lang"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    v.get("labels").and_then(|labels| {
                        labels
                            .as_object()
                            .and_then(|o| o.get("language")?.as_str().map(str::to_string))
                    })
                })
                .unwrap_or_default();

            let language_codes = if lang.is_empty() {
                vec![]
            } else {
                vec![crate::types::LanguageCode {
                    bcp47: lang.clone(),
                    iso639_3: lang.split(['-', '_']).next().unwrap_or(&lang).to_string(),
                    display: lang,
                }]
            };

            Some(Voice {
                id: id.to_string(),
                name,
                gender: normalize_gender(&gender_str),
                provider: provider.to_string(),
                language_codes,
            })
        })
        .collect()
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::map_unwrap_or
)]
#[cfg(feature = "cloud")]
pub(crate) fn compute_durations(boundaries: &mut [WordBoundary]) {
    if boundaries.is_empty() {
        return;
    }
    if boundaries.len() == 1 {
        boundaries[0].duration = boundaries[0].duration.max(500);
        return;
    }
    let len = boundaries.len();
    for i in 0..(len - 1) {
        if boundaries[i].duration == 0 {
            boundaries[i].duration = boundaries[i + 1]
                .offset
                .saturating_sub(boundaries[i].offset);
        }
    }
    if boundaries[len - 1].duration == 0 {
        boundaries[len - 1].duration = 500;
    }
}

// ===== Azure WebSocket message parsing helpers =====
//
// Azure's TTS WebSocket protocol turns each event into a text frame whose
// first lines are HTTP-like headers (`X-RequestId:…`, `Path:…`, …) followed
// by a blank line and a JSON body. The helpers below lift the per-message
// parsing out of the speak() loop so they can be exercised independently
// with sample frames recorded from a real Azure session.

/// Static voice lists for engines that don't expose a voice-list API.
/// Returns `None` for engines that *do* have a `voices_url` (those go
/// through the HTTP path in `get_voices`).
#[cfg(feature = "cloud")]
#[allow(clippy::too_many_lines)]
pub(crate) fn static_voices(provider: &str) -> Option<Vec<Voice>> {
    let en_us = || LanguageCode {
        bcp47: "en-US".to_string(),
        iso639_3: "eng".to_string(),
        display: "English (United States)".to_string(),
    };
    let lang = |bcp47: &str, iso: &str, display: &str| LanguageCode {
        bcp47: bcp47.to_string(),
        iso639_3: iso.to_string(),
        display: display.to_string(),
    };
    let voice =
        |id: &str, name: &str, gender: Gender, provider: &str, lcs: Vec<LanguageCode>| Voice {
            id: id.to_string(),
            name: name.to_string(),
            gender,
            provider: provider.to_string(),
            language_codes: lcs,
        };

    match provider {
        // Qwen cloud voices (DashScope has no list endpoint — static
        // table). Voice ids are STRICTLY model-scoped (411/418 on
        // mismatch, verified live — issue #149): bare longan*/loong*
        // ids belong to qwen-audio-3.0-tts-flash (default; the two
        // unsuffixed flagship entries to 3.0-plus), *_v3.1 ids to
        // qwen-audio-3.1-tts-{plus,flash}, the prefixed
        // qwen-audio-3.1-tts-flash-* ids to 3.1-flash minority-language
        // voices, and *_v3 / bare longanyang to cosyvoice-v3-flash
        // (requires modelId). With NO modelId credential the engine
        // infers the model from the voice (qwen_voice_inferred_model),
        // so any listed voice just works. Generated from the
        // Qwen-Audio-TTS voice list doc (2026-11-23) and the CosyVoice
        // voice list doc (2026-11-12).
        "qwen" => {
            let zh_en = || vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()];
            // 3.0 voices (the default model's list — plus voices marked below).
            let mut v = vec![
                voice(
                    "longanlingxin",
                    "Long An Ling Xin",
                    Gender::Female,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longanlufeng",
                    "Long An Lu Feng",
                    Gender::Male,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longanfengyue",
                    "Long An Feng Yue",
                    Gender::Female,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longanyuanfei",
                    "Long An Yuan Fei",
                    Gender::Female,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longanlingxi",
                    "Long An Ling Xi",
                    Gender::Female,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longanxiaoxin",
                    "Long An Xiao Xin",
                    Gender::Female,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longanhuan_v3.6",
                    "Long An Huan",
                    Gender::Female,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longjielidou_v3.6",
                    "Long Jie Li Dou",
                    Gender::Male,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longpaopao_v3.6",
                    "Long Pao Pao",
                    Gender::Female,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longhuohuo_v3.6",
                    "Long Huo Huo",
                    Gender::Male,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "longchuanshu_v3.6",
                    "Long Chuan Shu",
                    Gender::Male,
                    "qwen",
                    zh_en(),
                ),
                voice(
                    "loongmary",
                    "Loong Mary",
                    Gender::Female,
                    "qwen",
                    vec![en_us()],
                ),
                voice(
                    "loongeva_v3.6",
                    "Loong Eva",
                    Gender::Female,
                    "qwen",
                    vec![en_us()],
                ),
                voice(
                    "loongjohn",
                    "Loong John",
                    Gender::Male,
                    "qwen",
                    vec![en_us()],
                ),
            ];
            // 3.1 multilingual flagship (dialects via languageHints).
            v.extend([
                voice(
                    "longanlingxin_v3.1",
                    "Long An Ling Xin",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
                voice(
                    "longanhuan_v3.1",
                    "Long An Huan",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
                voice(
                    "longanfengyue_v3.1",
                    "Long An Feng Yue",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
                voice(
                    "xunanchuan_v3.1",
                    "Xunanchuan",
                    Gender::Male,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
            ]);
            // 3.1 premium Chinese — a CURATED subset of the doc's list
            // (Mandarin-only, plus the bilingual loongbella). The full
            // table: the Qwen-Audio-TTS voice-list doc. Curated entries
            // work identically (any listed voice synthesizes; see the
            // inference rules) — curation only affects discoverability.
            v.extend([
                voice(
                    "yuxiaoyun_v3.1",
                    "Yu Xiaoyun",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "qiaoxiaojiao_v3.1",
                    "Qiao Xiaojiao",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "xiaxiaochen_v3.1",
                    "Xia Xiaochen",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "anmingyuan_v3.1",
                    "An Mingyuan",
                    Gender::Male,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "wenhuaiqing_v3.1",
                    "Wen Huaiqing",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "anxiaolan_v3.1",
                    "An Xiaolan",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "xieshurou_v3.1",
                    "Xie Shurou",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "baiqinglan_v3.1",
                    "Bai Qinglan",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "xuyuyuan_v3.1",
                    "Xu Yuyuan",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "anruorou_v3.1",
                    "An Ruorou",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "wenhuaizhi_v3.1",
                    "Wen Huaizhi",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "xiaoxingzhi_v3.1",
                    "Xiao Xingzhi",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "guyunshu_v3.1",
                    "Gu Yunshu",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "huozhuoshi_v3.1",
                    "Huo Zhuoshi",
                    Gender::Male,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "yeqinghe_v3.1",
                    "Ye Qinghe",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
                voice(
                    "loongbella_v3.1",
                    "Bella (Mandarin + English)",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
                voice(
                    "xuyanchu_v3.1",
                    "Xu Yanchu",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)")],
                ),
            ]);
            // 3.1 premium English.
            v.extend([
                voice("Emily_v3.1", "Emily", Gender::Female, "qwen", vec![en_us()]),
                voice("Luna_v3.1", "Luna", Gender::Female, "qwen", vec![en_us()]),
                voice("Eric_v3.1", "Eric", Gender::Male, "qwen", vec![en_us()]),
                voice("Luca_v3.1", "Luca", Gender::Male, "qwen", vec![en_us()]),
                voice("Abby_v3.1", "Abby", Gender::Female, "qwen", vec![en_us()]),
                voice("Annie_v3.1", "Annie", Gender::Female, "qwen", vec![en_us()]),
                voice("Ava_v3.1", "Ava", Gender::Female, "qwen", vec![en_us()]),
                voice("Beth_v3.1", "Beth", Gender::Female, "qwen", vec![en_us()]),
                voice("Betty_v3.1", "Betty", Gender::Female, "qwen", vec![en_us()]),
                voice("Cally_v3.1", "Cally", Gender::Female, "qwen", vec![en_us()]),
                voice("Cindy_v3.1", "Cindy", Gender::Female, "qwen", vec![en_us()]),
                voice("Donna_v3.1", "Donna", Gender::Female, "qwen", vec![en_us()]),
                voice("Andy_v3.1", "Andy", Gender::Male, "qwen", vec![en_us()]),
                voice("Brian_v3.1", "Brian", Gender::Male, "qwen", vec![en_us()]),
                voice("David_v3.1", "David", Gender::Male, "qwen", vec![en_us()]),
            ]);
            // 3.1 minority-language system voices — the coverage win
            // (id embeds the model: qwen-audio-3.1-tts-flash-<name>_<lang>).
            v.extend([
                voice(
                    "qwen-audio-3.1-tts-flash-zora_bg",
                    "Zora (Bulgarian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("bg", "bul", "Bulgarian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-linde_nl",
                    "Linde (Dutch)",
                    Gender::Male,
                    "qwen",
                    vec![lang("nl", "nld", "Dutch")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-hajnal_hu",
                    "Hajnal (Hungarian)",
                    Gender::Female,
                    "qwen",
                    vec![lang("hu", "hun", "Hungarian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-senin_ro",
                    "Senin (Romanian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ro", "ron", "Romanian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-birk_da",
                    "Birk (Danish)",
                    Gender::Male,
                    "qwen",
                    vec![lang("da", "dan", "Danish")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-lipa_hr",
                    "Lipa (Croatian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("hr", "hrv", "Croatian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-avgi_el",
                    "Avgi (Greek)",
                    Gender::Male,
                    "qwen",
                    vec![lang("el", "ell", "Greek")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-jitrenka_cs",
                    "Jitřenka (Czech)",
                    Gender::Male,
                    "qwen",
                    vec![lang("cs", "ces", "Czech")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-solglimt_nn",
                    "Solglimt (Norwegian (Nynorsk))",
                    Gender::Male,
                    "qwen",
                    vec![lang("nn", "nno", "Norwegian (Nynorsk)")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-glimma_sv",
                    "Glimma (Swedish)",
                    Gender::Female,
                    "qwen",
                    vec![lang("sv", "swe", "Swedish")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-svar_hi",
                    "Svar (Hindi)",
                    Gender::Male,
                    "qwen",
                    vec![lang("hi", "hin", "Hindi")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-iskra_sk",
                    "Iskra (Slovak)",
                    Gender::Female,
                    "qwen",
                    vec![lang("sk", "slk", "Slovak")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-aava_fi",
                    "Aava (Finnish)",
                    Gender::Male,
                    "qwen",
                    vec![lang("fi", "fin", "Finnish")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-sahar_ur",
                    "Sahar (Urdu)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ur", "urd", "Urdu")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-jasna_sr",
                    "Jasna (Serbian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("sr", "srp", "Serbian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-brisa_es_mx",
                    "Brisa (Mexican Spanish)",
                    Gender::Female,
                    "qwen",
                    vec![lang("es-MX", "spa", "Mexican Spanish")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-ausra_lt",
                    "Aušra (Lithuanian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("lt", "lit", "Lithuanian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-rasa_lv",
                    "Rasa (Latvian)",
                    Gender::Female,
                    "qwen",
                    vec![lang("lv", "lav", "Latvian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-deniz_tr",
                    "Deniz (Turkish)",
                    Gender::Male,
                    "qwen",
                    vec![lang("tr", "tur", "Turkish")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-ava_fa",
                    "Ava (Persian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("fa", "pes", "Persian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-or_he",
                    "Or (Hebrew)",
                    Gender::Male,
                    "qwen",
                    vec![lang("he", "heb", "Hebrew")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-ayalguu_mn",
                    "Ayalguu (Mongolian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("mn", "mon", "Mongolian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-zarja_sl",
                    "Zarja (Slovenian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("sl", "slv", "Slovenian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-ishiq_az",
                    "Işıq (Azerbaijani)",
                    Gender::Male,
                    "qwen",
                    vec![lang("az", "aze", "Azerbaijani")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-zorza_pl",
                    "Zorza (Polish)",
                    Gender::Male,
                    "qwen",
                    vec![lang("pl", "pol", "Polish")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-alba_ca",
                    "Alba (Catalan)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ca", "cat", "Catalan")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-romduol_km",
                    "Romduol (Khmer)",
                    Gender::Male,
                    "qwen",
                    vec![lang("km", "khm", "Khmer")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-aurora_pt_br",
                    "Aurora (Brazilian Portuguese)",
                    Gender::Male,
                    "qwen",
                    vec![lang("pt-BR", "por", "Brazilian Portuguese")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-dao_lo",
                    "Dao (Lao)",
                    Gender::Male,
                    "qwen",
                    vec![lang("lo", "lao", "Lao")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-tsiskari_ka",
                    "Tsiskari (Georgian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ka", "kat", "Georgian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-lente_af",
                    "Lente (Afrikaans)",
                    Gender::Female,
                    "qwen",
                    vec![lang("af", "afr", "Afrikaans")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-kiran_ne",
                    "Kiran (Nepali)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ne", "nep", "Nepali")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-zorka_be",
                    "Zorka (Belarusian)",
                    Gender::Female,
                    "qwen",
                    vec![lang("be", "bel", "Belarusian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-nilaav_ml",
                    "Nilaav (Malayalam)",
                    Gender::Female,
                    "qwen",
                    vec![lang("ml", "mal", "Malayalam")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-staer_lb",
                    "Stär (Luxembourgish)",
                    Gender::Female,
                    "qwen",
                    vec![lang("lb", "ltz", "Luxembourgish")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-drita_sq",
                    "Drita (Albanian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("sq", "sqi", "Albanian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-nila_ta",
                    "Nila (Tamil)",
                    Gender::Female,
                    "qwen",
                    vec![lang("ta", "tam", "Tamil")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-aruna_si",
                    "Aruna (Sinhala)",
                    Gender::Male,
                    "qwen",
                    vec![lang("si", "sin", "Sinhala")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-vennela_te",
                    "Vennela (Telugu)",
                    Gender::Male,
                    "qwen",
                    vec![lang("te", "tel", "Telugu")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-sade_et",
                    "Säde (Estonian)",
                    Gender::Female,
                    "qwen",
                    vec![lang("et", "est", "Estonian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-bituon_ceb",
                    "Bituon (Cebuano)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ceb", "ceb", "Cebuano")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-vedrina_bs",
                    "Vedrina (Bosnian)",
                    Gender::Female,
                    "qwen",
                    vec![lang("bs", "bos", "Bosnian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-layaung_my",
                    "LaYaung (Burmese)",
                    Gender::Male,
                    "qwen",
                    vec![lang("my", "mya", "Burmese")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-luys_hy",
                    "Luys (Armenian)",
                    Gender::Female,
                    "qwen",
                    vec![lang("hy", "hye", "Armenian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-lumen_la",
                    "Lumen (Latin)",
                    Gender::Male,
                    "qwen",
                    vec![lang("la", "lat", "Latin")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-birta_is",
                    "Birta (Icelandic)",
                    Gender::Male,
                    "qwen",
                    vec![lang("is", "isl", "Icelandic")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-zerniq_mt",
                    "Żerniq (Maltese)",
                    Gender::Female,
                    "qwen",
                    vec![lang("mt", "mlt", "Maltese")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-nuru_sw",
                    "Nuru (Swahili)",
                    Gender::Female,
                    "qwen",
                    vec![lang("sw", "swa", "Swahili")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-arai_kk",
                    "Arai (Kazakh)",
                    Gender::Male,
                    "qwen",
                    vec![lang("kk", "kaz", "Kazakh")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-zetwal_crs",
                    "Zetwal (Seychellois Creole)",
                    Gender::Female,
                    "qwen",
                    vec![lang("crs", "crs", "Seychellois Creole")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-zoria_uk",
                    "Zoria (Ukrainian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("uk", "ukr", "Ukrainian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-nagi_ja",
                    "Nagi (Japanese)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ja", "jpn", "Japanese")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-haneul_ko",
                    "Haneul (Korean)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ko", "kor", "Korean")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-aube_fr",
                    "Aube (French)",
                    Gender::Female,
                    "qwen",
                    vec![lang("fr", "fra", "French")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-lerche_de",
                    "Lerche (German)",
                    Gender::Female,
                    "qwen",
                    vec![lang("de", "deu", "German")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-veter_ru",
                    "Veter (Russian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ru", "rus", "Russian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-rugiada_it",
                    "Rugiada (Italian)",
                    Gender::Female,
                    "qwen",
                    vec![lang("it", "ita", "Italian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-albor_es",
                    "Albor (Spanish)",
                    Gender::Male,
                    "qwen",
                    vec![lang("es", "spa", "Spanish")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-farol_pt",
                    "Farol (Portuguese)",
                    Gender::Male,
                    "qwen",
                    vec![lang("pt", "por", "Portuguese")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-naseem_ar",
                    "Naseem (Arabic)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ar", "ara", "Arabic")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-embun_id",
                    "Embun (Indonesian)",
                    Gender::Male,
                    "qwen",
                    vec![lang("id", "ind", "Indonesian")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-may_vi",
                    "Mây (Vietnamese)",
                    Gender::Male,
                    "qwen",
                    vec![lang("vi", "vie", "Vietnamese")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-bayu_ms",
                    "Bayu (Malay)",
                    Gender::Male,
                    "qwen",
                    vec![lang("ms", "msa", "Malay")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-sailom_th",
                    "Sailom (Thai)",
                    Gender::Male,
                    "qwen",
                    vec![lang("th", "tha", "Thai")],
                ),
                voice(
                    "qwen-audio-3.1-tts-flash-hiraya_tl",
                    "Hiraya (Filipino (Tagalog))",
                    Gender::Female,
                    "qwen",
                    vec![lang("tl", "tgl", "Filipino (Tagalog)")],
                ),
            ]);
            // CosyVoice v3 benchmark voices — REQUIRE modelId:
            // cosyvoice-v3-flash (bare long* names do not infer).
            v.extend([
                voice(
                    "longanyang",
                    "Long Anyang",
                    Gender::Male,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
                voice(
                    "longanhuan_v3",
                    "Long Anhuan (V3)",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
                voice(
                    "longhuhu_v3",
                    "Long Huhu",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
                voice(
                    "longwan_v3",
                    "Long Wan",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
                voice(
                    "longcheng_v3",
                    "Long Cheng",
                    Gender::Male,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
                voice(
                    "loongbella_v3",
                    "Bella 3.0",
                    Gender::Female,
                    "qwen",
                    vec![lang("zh-CN", "zho", "Chinese (Simplified)"), en_us()],
                ),
            ]);
            Some(v)
        }
        "openai" => Some(vec![
            voice(
                "alloy",
                "OpenAI alloy",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice("ash", "OpenAI ash", Gender::Male, "openai", vec![en_us()]),
            voice(
                "ballad",
                "OpenAI ballad",
                Gender::Male,
                "openai",
                vec![en_us()],
            ),
            voice(
                "coral",
                "OpenAI coral",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice("echo", "OpenAI echo", Gender::Male, "openai", vec![en_us()]),
            voice(
                "fable",
                "OpenAI fable",
                Gender::Male,
                "openai",
                vec![en_us()],
            ),
            voice(
                "nova",
                "OpenAI nova",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice("onyx", "OpenAI onyx", Gender::Male, "openai", vec![en_us()]),
            voice(
                "sage",
                "OpenAI sage",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice(
                "shimmer",
                "OpenAI shimmer",
                Gender::Female,
                "openai",
                vec![en_us()],
            ),
            voice(
                "verse",
                "OpenAI verse",
                Gender::Unknown,
                "openai",
                vec![en_us()],
            ),
        ]),
        "hume" => Some(vec![
            voice("ito", "Hume Ito", Gender::Unknown, "hume", vec![en_us()]),
            voice(
                "acantha",
                "Hume Acantha",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice(
                "ant ai gonus",
                "Hume Antigonos",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice("ari", "Hume Ari", Gender::Unknown, "hume", vec![en_us()]),
            voice(
                "brant",
                "Hume Brant",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice(
                "daniel",
                "Hume Daniel",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice("fin", "Hume Fin", Gender::Unknown, "hume", vec![en_us()]),
            voice("hype", "Hume Hype", Gender::Unknown, "hume", vec![en_us()]),
            voice("kora", "Hume Kora", Gender::Unknown, "hume", vec![en_us()]),
            voice(
                "mango",
                "Hume Mango",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice(
                "marek",
                "Hume Marek",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice("ogma", "Hume Ogma", Gender::Unknown, "hume", vec![en_us()]),
            voice("sora", "Hume Sora", Gender::Unknown, "hume", vec![en_us()]),
            voice(
                "terrence",
                "Hume Terrence",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice(
                "vitor",
                "Hume Vitor",
                Gender::Unknown,
                "hume",
                vec![en_us()],
            ),
            voice("zach", "Hume Zach", Gender::Unknown, "hume", vec![en_us()]),
        ]),
        "mistral" => Some(vec![
            voice(
                "Amalthea",
                "Mistral Amalthea",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Achan",
                "Mistral Achan",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Brave",
                "Mistral Brave",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Contessa",
                "Mistral Contessa",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Daintree",
                "Mistral Daintree",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Eugora",
                "Mistral Eugora",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Fornax",
                "Mistral Fornax",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Griffin",
                "Mistral Griffin",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Hestia",
                "Mistral Hestia",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Irving",
                "Mistral Irving",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Jasmine",
                "Mistral Jasmine",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Kestra",
                "Mistral Kestra",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Lorentz",
                "Mistral Lorentz",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Mara",
                "Mistral Mara",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Nettle",
                "Mistral Nettle",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Orin",
                "Mistral Orin",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Puck",
                "Mistral Puck",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Quinn",
                "Mistral Quinn",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Rune",
                "Mistral Rune",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Simbe",
                "Mistral Simbe",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Tertia",
                "Mistral Tertia",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Umbriel",
                "Mistral Umbriel",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Vesta",
                "Mistral Vesta",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Wystan",
                "Mistral Wystan",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Xeno",
                "Mistral Xeno",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Yara",
                "Mistral Yara",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
            voice(
                "Zephyr",
                "Mistral Zephyr",
                Gender::Unknown,
                "mistral",
                vec![en_us()],
            ),
        ]),
        "murf" => {
            let de = || lang("de-DE", "deu", "German (Germany)");
            let es = || lang("es-ES", "spa", "Spanish (Spain)");
            let fr = || lang("fr-FR", "fra", "French (France)");
            let pt = || lang("pt-BR", "por", "Portuguese (Brazil)");
            let it = || lang("it-IT", "ita", "Italian (Italy)");
            Some(vec![
                voice(
                    "en-US-natalie",
                    "Murf Natalie",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-owen",
                    "Murf Owen",
                    Gender::Male,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-amira",
                    "Murf Amira",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-daniel",
                    "Murf Daniel",
                    Gender::Male,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-taylor",
                    "Murf Taylor",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-alex",
                    "Murf Alex",
                    Gender::Male,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-emily",
                    "Murf Emily",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice("en-US-ben", "Murf Ben", Gender::Male, "murf", vec![en_us()]),
                voice(
                    "en-US-claire",
                    "Murf Claire",
                    Gender::Female,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "en-US-glen",
                    "Murf Glen",
                    Gender::Male,
                    "murf",
                    vec![en_us()],
                ),
                voice(
                    "de-DE-detlef",
                    "Murf Detlef",
                    Gender::Male,
                    "murf",
                    vec![de()],
                ),
                voice(
                    "es-ES-rosalyn",
                    "Murf Rosalyn",
                    Gender::Female,
                    "murf",
                    vec![es()],
                ),
                voice(
                    "fr-FR-henri",
                    "Murf Henri",
                    Gender::Male,
                    "murf",
                    vec![fr()],
                ),
                voice(
                    "pt-BR-thomas",
                    "Murf Thomas",
                    Gender::Male,
                    "murf",
                    vec![pt()],
                ),
                voice(
                    "it-IT-giulia",
                    "Murf Giulia",
                    Gender::Female,
                    "murf",
                    vec![it()],
                ),
            ])
        }
        "unrealspeech" => Some(vec![
            voice(
                "Sierra",
                "UnrealSpeech Sierra",
                Gender::Female,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Dan",
                "UnrealSpeech Dan",
                Gender::Male,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Will",
                "UnrealSpeech Will",
                Gender::Male,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Scarlett",
                "UnrealSpeech Scarlett",
                Gender::Female,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Liv",
                "UnrealSpeech Liv",
                Gender::Female,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Amy",
                "UnrealSpeech Amy",
                Gender::Female,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Eric",
                "UnrealSpeech Eric",
                Gender::Male,
                "unrealspeech",
                vec![en_us()],
            ),
            voice(
                "Brian",
                "UnrealSpeech Brian",
                Gender::Male,
                "unrealspeech",
                vec![en_us()],
            ),
        ]),
        "xai" => Some(vec![
            voice(
                "avalon-47",
                "xAI Avalon",
                Gender::Female,
                "xai",
                vec![en_us()],
            ),
            voice("orion-56", "xAI Orion", Gender::Male, "xai", vec![en_us()]),
            voice("luna-30", "xAI Luna", Gender::Female, "xai", vec![en_us()]),
            voice("atlas-84", "xAI Atlas", Gender::Male, "xai", vec![en_us()]),
            voice("aria-42", "xAI Aria", Gender::Female, "xai", vec![en_us()]),
            voice("cosmo-01", "xAI Cosmo", Gender::Male, "xai", vec![en_us()]),
        ]),
        "upliftai" => {
            let ur = || lang("ur-PK", "urd", "Urdu (Pakistan)");
            Some(vec![
                voice(
                    "v_8eelc901",
                    "UpliftAI Info/Education",
                    Gender::Unknown,
                    "upliftai",
                    vec![ur()],
                ),
                voice(
                    "v_30s70t3a",
                    "UpliftAI Nostalgic News",
                    Gender::Unknown,
                    "upliftai",
                    vec![ur()],
                ),
                voice(
                    "v_yypgzenx",
                    "UpliftAI Dada Jee",
                    Gender::Unknown,
                    "upliftai",
                    vec![ur()],
                ),
                voice(
                    "v_kwmp7zxt",
                    "UpliftAI Gen Z",
                    Gender::Unknown,
                    "upliftai",
                    vec![ur()],
                ),
            ])
        }
        "modelslab" => Some(vec![
            voice(
                "madison",
                "ModelsLab Madison",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "tara",
                "ModelsLab Tara",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "leah",
                "ModelsLab Leah",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "jess",
                "ModelsLab Jess",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "mia",
                "ModelsLab Mia",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "zoe",
                "ModelsLab Zoe",
                Gender::Female,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "leo",
                "ModelsLab Leo",
                Gender::Male,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "dan",
                "ModelsLab Dan",
                Gender::Male,
                "modelslab",
                vec![en_us()],
            ),
            voice(
                "zac",
                "ModelsLab Zac",
                Gender::Male,
                "modelslab",
                vec![en_us()],
            ),
        ]),
        _ => None,
    }
}
