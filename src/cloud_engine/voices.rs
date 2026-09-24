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
        let lang_codes = v
            .get("languageCodes")
            .and_then(|v| v.as_array())
            .map(|arr| {
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
            })
            .unwrap_or_default();

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
