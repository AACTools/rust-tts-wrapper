//! Typed engine setup schema exposed through the ABI and the .NET
//! bindings (GH #68).
//!
//! .NET speech settings screens (Dasher Windows) previously built their
//! provider tables by hand: names, credential fields, secret-ness
//! inferred from key *names* (so Google's `KeyFilePath` got a password
//! box). This module is the single source of truth the bindings can
//! render from:
//!
//! * [`SetupField`] — canonical key, label, required/default, and a
//!   typed [`SetupFieldKind`] (secret / file path / directory path /
//!   URL / choice / number / text) so the UI picks the right control:
//!   password box, file picker, folder picker, combo, spinner, or text.
//! * [`EngineCapabilityInfo`] — rate/pitch/volume/timing/SSML/streaming
//!   support and network requirements, including backend limitations in
//!   `notes` (e.g. "pitch ignored", "timings are estimates").
//! * [`engine_setup_schemas()` — one [`EngineSetup`] per engine in
//!   [`crate::factory::engine_list()`] (the same feature gating), so a
//!   settings UI renders *exactly* the engines in the installed native
//!   build — no provider switch needed.
//!
//! The legacy `credential_keys_json` string on [`crate::types::EngineDescriptor`]
//! is unchanged for compatibility; every schema field key is also listed
//! there (guarded by test).

use serde::Serialize;

use crate::factory;

/// The kind of control a UI should render for a setup field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupFieldKind {
    /// Secret value — render a password box, never log it.
    Secret,
    /// Path to a single file — render a file picker.
    FilePath,
    /// Path to a directory — render a folder picker.
    DirectoryPath,
    /// A URL — render a text box with URL validation.
    Url,
    /// One of a fixed set of suggested values — render a combo box.
    /// Free text is still accepted unless `required` and empty.
    Choice,
    /// Numeric value — render a spinner/slider.
    Number,
    /// Ordinary single/multi-line text.
    Text,
}

impl SetupFieldKind {
    /// Stable wire name (matches the serde serialization).
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::FilePath => "filePath",
            Self::DirectoryPath => "directoryPath",
            Self::Url => "url",
            Self::Choice => "choice",
            Self::Number => "number",
            Self::Text => "text",
        }
    }
}

/// One typed setup/credential field for an engine.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupField {
    /// Canonical credential key understood by `tts_create` (e.g.
    /// `subscriptionKey`). Stable — never rename.
    pub key: String,
    /// Human label for the settings row.
    pub label: String,
    /// Control kind (see [`SetupFieldKind`]).
    pub kind: SetupFieldKind,
    /// Whether the engine fails without this field.
    #[serde(default)]
    pub required: bool,
    /// Suggested default value (string form; numbers included).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    /// Suggested values for [`SetupFieldKind::Choice`] fields. Free text
    /// remains valid unless the engine says otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Placeholder text for empty inputs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// One-line help text for the settings row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help_text: Option<String>,
}

impl SetupField {
    /// Plain (non-secret, non-choice) field.
    #[must_use]
    pub fn text(key: &str, label: &str) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind: SetupFieldKind::Text,
            required: false,
            default_value: None,
            choices: Vec::new(),
            placeholder: None,
            help_text: None,
        }
    }

    /// Secret field (password box).
    #[must_use]
    pub fn secret(key: &str, label: &str) -> Self {
        Self {
            kind: SetupFieldKind::Secret,
            ..Self::text(key, label)
        }
    }

    /// File-path field (file picker).
    #[must_use]
    pub fn file(key: &str, label: &str) -> Self {
        Self {
            kind: SetupFieldKind::FilePath,
            ..Self::text(key, label)
        }
    }

    /// Directory-path field (folder picker).
    #[must_use]
    pub fn directory(key: &str, label: &str) -> Self {
        Self {
            kind: SetupFieldKind::DirectoryPath,
            ..Self::text(key, label)
        }
    }

    /// Numeric field (spinner/slider).
    #[must_use]
    pub fn number(key: &str, label: &str) -> Self {
        Self {
            kind: SetupFieldKind::Number,
            ..Self::text(key, label)
        }
    }

    /// Choice field (combo box; free text still accepted).
    #[must_use]
    pub fn choice(key: &str, label: &str) -> Self {
        Self {
            kind: SetupFieldKind::Choice,
            ..Self::text(key, label)
        }
    }

    /// URL field.
    #[must_use]
    pub fn url(key: &str, label: &str) -> Self {
        Self {
            kind: SetupFieldKind::Url,
            ..Self::text(key, label)
        }
    }

    /// Mark required.
    #[must_use]
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Attach a default value.
    #[must_use]
    pub fn default(mut self, v: impl Into<String>) -> Self {
        self.default_value = Some(v.into());
        self
    }

    /// Attach suggested choices.
    #[must_use]
    pub fn choices(mut self, c: &[&str]) -> Self {
        self.choices = c.iter().map(|s| (*s).to_string()).collect();
        self
    }

    /// Attach help text.
    #[must_use]
    pub fn help(mut self, h: impl Into<String>) -> Self {
        self.help_text = Some(h.into());
        self
    }

    /// Attach placeholder text.
    #[must_use]
    pub fn placeholder(mut self, p: impl Into<String>) -> Self {
        self.placeholder = Some(p.into());
        self
    }
}

/// What an engine can do — drives UI affordances and capability gates.
// A capability table: eight flags IS the data; a builder would obscure
// the per-engine rows.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineCapabilityInfo {
    /// Rate multiplier honoured.
    pub supports_rate: bool,
    /// Pitch multiplier honoured (not just accepted and ignored).
    pub supports_pitch: bool,
    /// Volume honoured.
    pub supports_volume: bool,
    /// Word-boundary/timing events fire (see `notes` for estimated vs
    /// measured).
    pub supports_word_timing: bool,
    /// Engine parses SSML input.
    pub supports_ssml: bool,
    /// True incremental streaming audio (WebSocket/chunked); false =
    /// whole-buffer synthesis chunked after the fact.
    pub supports_streaming: bool,
    /// Needs an internet connection.
    pub requires_internet: bool,
    /// `get_voices()` succeeds without authentication (static catalog or
    /// unauthenticated listing) — voice enumeration alone must not be
    /// read as "credentials valid" (GH #69).
    pub static_voice_catalog: bool,
    /// Backend limitations worth showing next to the controls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl EngineCapabilityInfo {
    /// Engine with the given affordances (flags in the documented
    /// order — the catalogue table reads better positionally).
    #[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        rate: bool,
        pitch: bool,
        volume: bool,
        timing: bool,
        ssml: bool,
        streaming: bool,
        internet: bool,
        static_catalog: bool,
    ) -> Self {
        Self {
            supports_rate: rate,
            supports_pitch: pitch,
            supports_volume: volume,
            supports_word_timing: timing,
            supports_ssml: ssml,
            supports_streaming: streaming,
            requires_internet: internet,
            static_voice_catalog: static_catalog,
            notes: None,
        }
    }

    /// Attach a limitation note.
    #[must_use]
    pub fn note(mut self, n: impl Into<String>) -> Self {
        self.notes = Some(n.into());
        self
    }
}

/// Full setup schema for one engine.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineSetup {
    /// Engine id (matches `tts_create`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Whether any setup fields are required.
    pub needs_credentials: bool,
    /// Typed setup fields (empty for zero-config engines).
    pub fields: Vec<SetupField>,
    /// Capability flags.
    pub capabilities: EngineCapabilityInfo,
    /// Either/or requirement groups: at least ONE field in each group
    /// must be non-empty (e.g. google: `apiKey` OR `keyFilePath`).
    /// Empty when all requirements are expressed by field-level
    /// `required` flags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_any: Vec<Vec<String>>,
}

/// Fields + capabilities per engine id (the catalogue; feature gating is
/// applied by [`engine_setup_schemas`] via `engine_list()`).
// One flat per-engine table: splitting it would hurt the very
// readability the schema exists for.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
fn catalogue_entry(id: &str) -> Option<(Vec<SetupField>, EngineCapabilityInfo)> {
    let azure_regions: &[&str] = &[
        "australiaeast",
        "brazilsouth",
        "canadacentral",
        "centralindia",
        "centralus",
        "eastasia",
        "eastus",
        "eastus2",
        "francecentral",
        "germanywestcentral",
        "japaneast",
        "koreacentral",
        "northcentralus",
        "northeurope",
        "norwayeast",
        "southafricanorth",
        "southcentralus",
        "southeastasia",
        "southindia",
        "swedencentral",
        "switzerlandnorth",
        "uaenorth",
        "uksouth",
        "usgovarizona",
        "westcentralus",
        "westeurope",
        "westindia",
        "westus",
        "westus2",
        "westus3",
        "qatarcentral",
        "italynorth",
        "polandcentral",
        "spaincentral",
        "mexicocentral",
        "taiwannorth",
        "israelcentral",
    ];
    match id {
        // ── Zero-config system engines ───────────────────────────────
        "system" => Some((
            vec![],
            EngineCapabilityInfo::new(true, false, true, true, false, false, false, true)
                .note("Linux Speech Dispatcher; word timings are proportional estimates; pitch is not applied"),
        )),
        "avsynth" => Some((
            vec![],
            EngineCapabilityInfo::new(true, true, true, false, false, false, false, true)
                .note("macOS AVSpeechSynthesizer"),
        )),
        "sapi" => Some((
            vec![],
            EngineCapabilityInfo::new(true, true, true, true, true, false, false, true)
                .note("Boundary offsets are UTF-16 code units (ISpVoice events)"),
        )),

        // ── Local model engines ──────────────────────────────────────
        "sherpaonnx" => Some((
            vec![
                SetupField::directory("modelPath", "Model directory")
                    .help("Directory (or registry id) holding the sherpa-onnx voice; empty = bundled default"),
                SetupField::text("modelId", "Voice / model id"),
                SetupField::number("numThreads", "Threads").default("2"),
                SetupField::number("numSteps", "Steps").default("8"),
                SetupField::choice("provider", "Provider")
                    .choices(&["cpu", "coreml", "cuda", "directml"]),
                SetupField::file("referenceAudio", "Reference audio (cloning)")
                    .help("WAV used as the zero-shot cloning reference"),
                SetupField::text("referenceText", "Reference transcript"),
            ],
            EngineCapabilityInfo::new(true, true, true, true, false, false, false, true)
                .note("Word timings are estimates; pitch is a crude resampling shift"),
        )),
        "qwen3-local" => Some((
            vec![
                SetupField::directory("modelsDir", "Models directory")
                    .required()
                    .help("Directory with qwen3-tts-0.6b-f16.gguf and the tokenizer GGUF"),
                SetupField::number("threads", "Threads").default("4"),
                SetupField::number("temperature", "Temperature").default("0.9"),
                SetupField::number("topK", "Top-K").default("50"),
                SetupField::number("maxTokens", "Max audio tokens").default("4096"),
            ],
            EngineCapabilityInfo::new(true, false, true, true, false, false, false, true)
                .note("Word timings are estimates; pitch is ignored"),
        )),
        "omnivoice-local" => Some((
            vec![
                SetupField::file("modelPath", "Model GGUF")
                    .required()
                    .help("omnivoice-base-*.gguf from OmniVoice-GGUF"),
                SetupField::file("codecPath", "Codec GGUF")
                    .required()
                    .help("omnivoice-tokenizer-*.gguf from OmniVoice-GGUF"),
                SetupField::number("numSteps", "MaskGIT steps").default("32"),
                SetupField::number("seed", "Seed").default("42"),
                SetupField::text("instruct", "Voice design")
                    .help("Voice-design attributes, e.g. \"female, child, british accent\""),
                SetupField::text("language", "Language")
                    .help("ISO code or name; empty = auto-detect"),
            ],
            EngineCapabilityInfo::new(true, false, true, true, false, false, false, true).note(
                "Pitch is ignored; volume is PCM gain; timings are estimates; \
                      the advertised language list comes from the runtime's own table (GH #64)",
            ),
        )),
        "floravox" => Some((
            vec![
                SetupField::directory("modelsDir", "Models directory"),
                SetupField::text("modelId", "Voice / model id"),
                SetupField::text("misaki", "G2P (misaki) variant"),
                SetupField::text("chars", "Character frontend")
                    .help("\"true\" or an ISO 639-3 code (e.g. \"hin\") to force per-language G2P"),
                SetupField::text("speaker", "Speaker"),
            ],
            EngineCapabilityInfo::new(true, false, true, true, false, false, false, true)
                .note("Measured word timings from the duration tensor; pitch is not applied"),
        )),
        "pocket" => Some((
            vec![
                SetupField::directory("bundleDir", "Model bundle directory").required(),
                SetupField::file("reference", "Reference WAV (cloning)"),
            ],
            EngineCapabilityInfo::new(true, false, true, true, false, false, false, true)
                .note("Attention-aligned (measured) word timings. Factory creation (tts_create) not yet wired — construct PocketEngine directly; see GH follow-up"),
        )),

        // ── Cloud providers ──────────────────────────────────────────
        "openai" => Some((
            vec![SetupField::secret("apiKey", "API key")
                .required()
                .placeholder("sk-…")],
            EngineCapabilityInfo::new(true, false, true, false, true, false, true, true)
                .note("Static voice catalog: voice listing does not verify the key"),
        )),
        "elevenlabs" => Some((
            vec![
                SetupField::secret("apiKey", "API key").required(),
                SetupField::text("modelId", "Model").default("eleven_v4"),
                SetupField::text("voiceId", "Voice id"),
                SetupField::text("language", "Language code"),
            ],
            EngineCapabilityInfo::new(true, false, true, true, false, false, true, false),
        )),
        "azure" => Some((
            vec![
                SetupField::secret("subscriptionKey", "Subscription key").required(),
                SetupField::choice("region", "Region")
                    .required()
                    .choices(azure_regions)
                    .placeholder("eastus"),
            ],
            EngineCapabilityInfo::new(true, true, true, true, true, true, true, false),
        )),
        "edge" => Some((
            vec![],
            EngineCapabilityInfo::new(true, true, true, true, true, true, true, true)
                .note("Unauthenticated Microsoft Edge Read Aloud endpoint — voice listing needs no credentials"),
        )),
        "google" => Some((
            vec![
                SetupField::secret("apiKey", "API key")
                    .help("Either apiKey or keyFilePath is required"),
                SetupField::file("keyFilePath", "API key file")
                    .help("File whose first non-empty line is the API key; used when apiKey is not set"),
            ],
            EngineCapabilityInfo::new(true, true, true, true, true, false, true, false),
        )),
        "gemini" => Some((
            vec![
                SetupField::secret("apiKey", "API key").required(),
                SetupField::text("modelId", "Model").default("gemini-3.8-flash-tts"),
                SetupField::text("voice", "Voice"),
                SetupField::text("style", "Style instruction"),
            ],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
        )),
        "qwen" => Some((
            vec![
                SetupField::secret("apiKey", "API key").required(),
                SetupField::text("modelId", "Model"),
                SetupField::choice("region", "Region")
                    .choices(&["qwencloud", "intl", "beijing"])
                    .default("qwencloud")
                    .help("Regional endpoint; wsUrl overrides"),
                SetupField::text("instruction", "Instruction"),
                SetupField::url("wsUrl", "WebSocket URL")
                    .help("Optional override; empty routes by region"),
                SetupField::url("customizationUrl", "Customization URL"),
            ],
            EngineCapabilityInfo::new(true, false, true, true, false, true, true, true)
                .note("Static voice catalog: voice listing does not verify the key"),
        )),
        "polly" => Some((
            vec![
                SetupField::secret("accessKeyId", "Access key id").required(),
                SetupField::secret("secretAccessKey", "Secret access key").required(),
                SetupField::choice("region", "Region")
                    .choices(&[
                        "us-east-1", "us-east-2", "us-west-1", "us-west-2", "eu-west-1",
                        "eu-west-2", "eu-central-1", "ap-southeast-1", "ap-southeast-2",
                        "ap-northeast-1", "ap-south-1", "ca-central-1", "sa-east-1",
                    ])
                    .default("us-east-1"),
            ],
            EngineCapabilityInfo::new(true, true, true, false, true, false, true, false),
        )),
        "cartesia" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, true, true, true, false, false, true, false)
                .note("Alignment-based measured word timings"),
        )),
        "deepgram" | "fishaudio" | "murf" | "resemble" | "unrealspeech" | "upliftai"
        | "modelslab" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
        )),
        "playht" => Some((
            vec![
                SetupField::secret("apiKey", "API key").required(),
                SetupField::text("userId", "User id").required(),
            ],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
        )),
        "hume" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, true)
                .note("Static voice catalog: voice listing does not verify the key"),
        )),
        "mistral" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, true)
                .note("Static voice catalog: voice listing does not verify the key"),
        )),
        "watson" => Some((
            vec![
                SetupField::secret("apiKey", "API key").required(),
                SetupField::choice("region", "Region")
                    .choices(&[
                        "us-south", "us-east", "eu-de", "eu-gb", "jp-tok", "au-syd", "ca-tor",
                    ])
                    .default("us-south"),
                SetupField::text("instanceId", "Instance id"),
            ],
            EngineCapabilityInfo::new(true, false, true, false, true, false, true, true)
                .note("Static voice catalog: voice listing does not verify the key"),
        )),
        "witai" => Some((
            vec![SetupField::secret("token", "Server token").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
        )),
        "xai" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, true)
                .note("Static voice catalog: voice listing does not verify the key"),
        )),
        _ => None,
    }
}

/// One [`EngineSetup`] per engine compiled into this build (mirrors
/// [`factory::engine_list()`] exactly).
#[must_use]
pub fn engine_setup_schemas() -> Vec<EngineSetup> {
    factory::engine_list()
        .into_iter()
        .map(|desc| {
            let (fields, capabilities) = catalogue_entry(&desc.id).unwrap_or_else(|| {
                // Every engine_list() id must have an entry — guarded by
                // test `every_listed_engine_has_a_schema`. The fallback
                // keeps the FFI total (never panics) if a new engine is
                // added without schema metadata.
                eprintln!(
                    "engine_schema: no catalogue entry for engine '{}'; \
                     exposing empty schema",
                    desc.id
                );
                (
                    Vec::new(),
                    EngineCapabilityInfo::new(true, false, true, false, false, false, true, true),
                )
            });
            EngineSetup {
                required_any: required_any_groups(&desc.id),
                id: desc.id,
                name: desc.name,
                needs_credentials: desc.needs_credentials,
                fields,
                capabilities,
            }
        })
        .collect()
}

/// The full schema list serialized as a JSON array (for
/// `tts_get_engine_setups_json`).
#[must_use]
pub fn engine_setups_json() -> String {
    serde_json::to_string(&engine_setup_schemas()).unwrap_or_else(|_| "[]".into())
}

/// Either/or requirement groups per engine (at least one field in each
/// group must be non-empty). Keyed by engine id.
fn required_any_groups(engine_id: &str) -> Vec<Vec<String>> {
    match engine_id {
        // GH #68: a Google setup is complete with EITHER an inline key
        // or a key file (DotNetTtsWrapper GoogleCredentials migration).
        "google" => vec![vec!["apiKey".into(), "keyFilePath".into()]],
        _ => Vec::new(),
    }
}

/// First missing required setting for `engine_id` given the parsed
/// credentials — used by `tts_check_credentials` to report
/// `MissingConfiguration` with the offending field key (GH #69).
/// Covers both field-level `required` flags and the either/or
/// `required_any` groups (reports the first member of an unsatisfied
/// group).
#[allow(clippy::implicit_hasher)] // the parse_credentials pipeline is std-hashmap end to end
#[must_use]
pub fn first_missing_required_field(
    engine_id: &str,
    credentials: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let (fields, _) = catalogue_entry(engine_id)?;
    let missing = |v: Option<&String>| v.is_none_or(String::is_empty);
    if let Some(field) = fields
        .iter()
        .find(|f| f.required && missing(credentials.get(&f.key)))
    {
        return Some(field.key.clone());
    }
    for group in required_any_groups(engine_id) {
        let satisfied = group
            .iter()
            .any(|key| credentials.get(key).is_some_and(|v| !v.is_empty()));
        if !satisfied {
            return Some(group[0].clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_engine_has_a_schema() {
        for setup in engine_setup_schemas() {
            // The fallback path would produce zero fields for a
            // credential engine — catch additions without metadata.
            if setup.needs_credentials {
                assert!(
                    !setup.fields.is_empty(),
                    "engine '{}' needs credentials but has no schema fields",
                    setup.id
                );
            }
        }
    }

    #[test]
    fn schema_ids_match_engine_list_exactly() {
        let listed: Vec<String> = factory::engine_list()
            .iter()
            .map(|e| e.id.clone())
            .collect();
        let schema_ids: Vec<String> = engine_setup_schemas()
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert_eq!(listed, schema_ids);
    }

    #[test]
    fn schema_field_keys_stay_within_credential_keys_json() {
        // Compatibility guard: legacy `credential_keys_json` must keep
        // listing every schema key — older bindings read that field.
        for setup in engine_setup_schemas() {
            let desc = factory::engine_list()
                .into_iter()
                .find(|d| d.id == setup.id)
                .expect("schema id in engine_list");
            let keys: Vec<String> =
                serde_json::from_str::<Vec<String>>(&desc.credential_keys_json).unwrap_or_default();
            for field in &setup.fields {
                assert!(
                    keys.contains(&field.key),
                    "engine '{}': schema field '{}' missing from credential_keys_json",
                    setup.id,
                    field.key
                );
            }
        }
    }

    #[test]
    fn azure_renders_key_and_region() {
        let azure = engine_setup_schemas()
            .into_iter()
            .find(|s| s.id == "azure")
            .expect("azure in schema");
        let key = azure
            .fields
            .iter()
            .find(|f| f.key == "subscriptionKey")
            .expect("subscriptionKey field");
        assert_eq!(key.kind, SetupFieldKind::Secret);
        assert!(key.required);
        let region = azure
            .fields
            .iter()
            .find(|f| f.key == "region")
            .expect("region field");
        assert!(region.required);
        assert_eq!(region.kind, SetupFieldKind::Choice);
        assert!(!region.choices.is_empty());
        assert!(azure.capabilities.supports_streaming);
        assert!(!azure.capabilities.static_voice_catalog);
    }

    #[test]
    fn google_either_key_or_key_file() {
        let google = engine_setup_schemas()
            .into_iter()
            .find(|s| s.id == "google")
            .expect("google in schema");
        let file = google
            .fields
            .iter()
            .find(|f| f.key == "keyFilePath")
            .expect("keyFilePath field");
        assert_eq!(file.kind, SetupFieldKind::FilePath);
        assert!(!file.required);
        // Either/or is expressed as a required_any group, not a
        // required flag on apiKey (a keyFile-only setup is valid).
        assert!(google
            .required_any
            .contains(&vec!["apiKey".to_string(), "keyFilePath".to_string()]));
        // Validation honours both halves.
        let mut creds = std::collections::HashMap::new();
        creds.insert("keyFilePath".to_string(), "/keys/g.txt".to_string());
        assert_eq!(first_missing_required_field("google", &creds), None);
        creds.clear();
        assert_eq!(
            first_missing_required_field("google", &creds),
            Some("apiKey".to_string())
        );
    }

    #[test]
    fn google_has_credential_file_picker() {
        let google = engine_setup_schemas()
            .into_iter()
            .find(|s| s.id == "google")
            .expect("google in schema");
        let file = google
            .fields
            .iter()
            .find(|f| f.key == "keyFilePath")
            .expect("keyFilePath field");
        assert_eq!(file.kind, SetupFieldKind::FilePath);
        assert!(!file.required);
        let key = google
            .fields
            .iter()
            .find(|f| f.key == "apiKey")
            .expect("apiKey field");
        assert_eq!(key.kind, SetupFieldKind::Secret);
    }

    #[test]
    fn offline_model_directories_are_directory_pickers() {
        // GH #68 acceptance: "an offline model directory picker".
        #[cfg(feature = "qwen3-local")]
        {
            let qwen = engine_setup_schemas()
                .into_iter()
                .find(|s| s.id == "qwen3-local")
                .expect("qwen3-local in schema");
            let dir = qwen
                .fields
                .iter()
                .find(|f| f.key == "modelsDir")
                .expect("modelsDir field");
            assert_eq!(dir.kind, SetupFieldKind::DirectoryPath);
            assert!(dir.required);
        }
        #[cfg(feature = "sherpaonnx")]
        {
            let sherpa = engine_setup_schemas()
                .into_iter()
                .find(|s| s.id == "sherpaonnx")
                .expect("sherpaonnx in schema");
            let dir = sherpa
                .fields
                .iter()
                .find(|f| f.key == "modelPath")
                .expect("modelPath field");
            assert_eq!(dir.kind, SetupFieldKind::DirectoryPath);
        }
    }

    #[test]
    #[cfg(feature = "omnivoice-local")]
    fn numeric_fields_use_the_number_kind() {
        // Kind-driven UIs render spinners for these, not free text.
        let omni = engine_setup_schemas()
            .into_iter()
            .find(|s| s.id == "omnivoice-local")
            .expect("omnivoice-local in schema");
        let steps = omni
            .fields
            .iter()
            .find(|f| f.key == "numSteps")
            .expect("numSteps field");
        assert_eq!(steps.kind, SetupFieldKind::Number);
    }

    #[test]
    fn schema_defaults_match_engine_defaults() {
        // The registry must not silently downgrade users to deprecated
        // models: defaults mirror cloud_engine's own choices.
        let setups = engine_setup_schemas();
        let find = |engine: &str, key: &str| -> Option<String> {
            setups
                .iter()
                .find(|s| s.id == engine)?
                .fields
                .iter()
                .find(|f| f.key == key)?
                .default_value
                .clone()
        };
        assert_eq!(
            find("elevenlabs", "modelId").as_deref(),
            Some("eleven_v4"),
            "must match config.rs elevenlabs default"
        );
        assert_eq!(
            find("gemini", "modelId").as_deref(),
            Some("gemini-3.8-flash-tts"),
            "must match config.rs gemini default"
        );
    }

    #[test]
    fn omnivoice_models_are_file_paths() {
        #[cfg(feature = "omnivoice-local")]
        {
            let omni = engine_setup_schemas()
                .into_iter()
                .find(|s| s.id == "omnivoice-local")
                .expect("omnivoice-local in schema");
            for key in ["modelPath", "codecPath"] {
                let f = omni
                    .fields
                    .iter()
                    .find(|f| f.key == key)
                    .unwrap_or_else(|| panic!("{key} field"));
                assert_eq!(f.kind, SetupFieldKind::FilePath);
                assert!(f.required);
            }
        }
    }

    #[test]
    fn offline_local_engines_do_not_require_internet() {
        #[cfg(feature = "omnivoice-local")]
        assert!(
            !engine_setup_schemas()
                .into_iter()
                .find(|s| s.id == "omnivoice-local")
                .unwrap()
                .capabilities
                .requires_internet
        );
        #[cfg(feature = "qwen3-local")]
        assert!(
            !engine_setup_schemas()
                .into_iter()
                .find(|s| s.id == "qwen3-local")
                .unwrap()
                .capabilities
                .requires_internet
        );
    }

    #[test]
    fn missing_required_field_detection() {
        let mut creds = std::collections::HashMap::new();
        creds.insert("region".to_string(), "eastus".to_string());
        assert_eq!(
            first_missing_required_field("azure", &creds),
            Some("subscriptionKey".to_string())
        );
        creds.insert("subscriptionKey".to_string(), "k".to_string());
        assert_eq!(first_missing_required_field("azure", &creds), None);
        // Empty-string values count as missing.
        creds.insert("subscriptionKey".to_string(), String::new());
        assert_eq!(
            first_missing_required_field("azure", &creds),
            Some("subscriptionKey".to_string())
        );
        // Zero-config engines never report a missing field.
        assert_eq!(
            first_missing_required_field("system", &std::collections::HashMap::new()),
            None
        );
    }

    #[test]
    fn json_round_trip_shape() {
        let json = engine_setups_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        let arr = parsed.as_array().expect("array");
        assert!(!arr.is_empty());
        let azure = arr
            .iter()
            .find(|v| v["id"] == "azure")
            .expect("azure present");
        assert_eq!(azure["needsCredentials"], serde_json::Value::Bool(true));
        assert_eq!(azure["fields"][0]["kind"], "secret");
        assert_eq!(
            azure["capabilities"]["supportsStreaming"],
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn openai_is_flagged_static() {
        // GH #69: enumerating a static catalog must not read as
        // "credentials verified".
        let openai = engine_setup_schemas()
            .into_iter()
            .find(|s| s.id == "openai")
            .expect("openai in schema");
        assert!(openai.capabilities.static_voice_catalog);
        assert!(openai.capabilities.requires_internet);
    }
}
