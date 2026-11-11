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
    /// Offline engine with the given affordances.
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
}

/// Fields + capabilities per engine id (the catalogue; feature gating is
/// applied by [`engine_setup_schemas`] via `engine_list()`).
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
        "southafricanouth",
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
    ];
    match id {
        // ── Zero-config system engines ───────────────────────────────
        "system" => Some((
            vec![],
            EngineCapabilityInfo::new(true, false, true, false, false, false, false, true)
                .note("Linux Speech Dispatcher; word timings are proportional estimates"),
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
                    .help("Directory (or registry id) holding the sherpa-onnx voice")
                    .default("bundled default"),
                SetupField::text("modelId", "Voice / model id"),
                SetupField::text("numThreads", "Threads").default("2"),
                SetupField::text("numSteps", "Steps").default("8"),
                SetupField::text("provider", "Provider")
                    .choices(&["cpu", "coreml", "cuda", "directml"]),
                SetupField::file("referenceAudio", "Reference audio (cloning)")
                    .help("WAV used as the zero-shot cloning reference"),
                SetupField::text("referenceText", "Reference transcript"),
            ],
            EngineCapabilityInfo::new(true, false, true, true, false, false, false, true)
                .note("Word timings are estimates; pitch is not applied"),
        )),
        "qwen3-local" => Some((
            vec![
                SetupField::directory("modelsDir", "Models directory")
                    .required()
                    .help("Directory with qwen3-tts-0.6b-f16.gguf and the tokenizer GGUF"),
                SetupField::text("threads", "Threads").default("4"),
                SetupField::text("temperature", "Temperature").default("0.9"),
                SetupField::text("topK", "Top-K").default("50"),
                SetupField::text("maxTokens", "Max audio tokens").default("4096"),
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
                SetupField::text("numSteps", "MaskGIT steps").default("32"),
                SetupField::text("seed", "Seed").default("42"),
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
                SetupField::text("chars", "Characters").default("5"),
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
                .note("Attention-aligned (measured) word timings"),
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
                SetupField::text("modelId", "Model").default("eleven_multilingual_v2"),
                SetupField::text("voiceId", "Voice id"),
                SetupField::text("language", "Language code"),
            ],
            EngineCapabilityInfo::new(true, false, true, true, false, false, true, false),
        )),
        "azure" => Some((
            vec![
                SetupField::secret("subscriptionKey", "Subscription key").required(),
                SetupField::text("region", "Region")
                    .required()
                    .choices(azure_regions)
                    .placeholder("eastus"),
            ],
            EngineCapabilityInfo::new(true, true, true, true, true, true, true, false),
        )),
        "edge" => Some((
            vec![],
            EngineCapabilityInfo::new(true, true, true, true, true, true, true, false)
                .note("Unauthenticated Microsoft Edge Read Aloud endpoint"),
        )),
        "google" => Some((
            vec![
                SetupField::secret("apiKey", "API key").required(),
                SetupField::file("keyFilePath", "API key file")
                    .help("File containing the API key on its first line; used when no key is set"),
            ],
            EngineCapabilityInfo::new(true, true, true, true, true, false, true, false),
        )),
        "gemini" => Some((
            vec![
                SetupField::secret("apiKey", "API key").required(),
                SetupField::text("modelId", "Model").default("gemini-3.1-flash-tts-preview"),
                SetupField::text("voice", "Voice"),
                SetupField::text("style", "Style instruction"),
            ],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
        )),
        "qwen" => Some((
            vec![
                SetupField::secret("apiKey", "API key").required(),
                SetupField::text("modelId", "Model"),
                SetupField::text("region", "Region"),
                SetupField::text("instruction", "Instruction"),
                SetupField::text("wsUrl", "WebSocket URL").default("wss://dashscope.aliyuncs.com"),
                SetupField::text("customizationUrl", "Customization URL"),
            ],
            EngineCapabilityInfo::new(true, false, true, true, false, true, true, true)
                .note("Static voice catalog: voice listing does not verify the key"),
        )),
        "polly" => Some((
            vec![
                SetupField::secret("accessKeyId", "Access key id").required(),
                SetupField::secret("secretAccessKey", "Secret access key").required(),
                SetupField::text("region", "Region").default("us-east-1"),
            ],
            EngineCapabilityInfo::new(true, true, true, false, true, false, true, false),
        )),
        "cartesia" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, true, true, true, false, false, true, false)
                .note("Alignment-based measured word timings"),
        )),
        "deepgram" => Some((
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
        "fishaudio" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
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
        "murf" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
        )),
        "resemble" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
        )),
        "unrealspeech" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
        )),
        "upliftai" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
        )),
        "watson" => Some((
            vec![
                SetupField::secret("apiKey", "API key").required(),
                SetupField::text("region", "Region")
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
        "modelslab" => Some((
            vec![SetupField::secret("apiKey", "API key").required()],
            EngineCapabilityInfo::new(true, false, true, false, false, false, true, false),
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

/// First missing required field for `engine_id` given the parsed
/// credentials — used by `tts_check_credentials` to report
/// `MissingConfiguration` with the offending field key (GH #69).
#[must_use]
pub fn first_missing_required_field(
    engine_id: &str,
    credentials: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let (fields, _) = catalogue_entry(engine_id)?;
    fields
        .iter()
        .find(|f| f.required && credentials.get(&f.key).map_or(true, String::is_empty))
        .map(|f| f.key.clone())
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
        assert!(!region.choices.is_empty());
        assert!(azure.capabilities.supports_streaming);
        assert!(!azure.capabilities.static_voice_catalog);
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
