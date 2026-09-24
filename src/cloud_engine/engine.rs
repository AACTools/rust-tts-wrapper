use super::*;

pub(crate) type VisemeFn = Box<dyn FnMut(i32, f32)>;

thread_local! {
    pub(crate) static VISEME_CB: std::cell::RefCell<Option<VisemeFn>> =
        const { std::cell::RefCell::new(None) };
}

/// Set the thread-local viseme callback. Called by the FFI layer before speak().
pub fn set_viseme_callback(cb: Option<Box<dyn FnMut(i32, f32)>>) {
    VISEME_CB.with(|cell| *cell.borrow_mut() = cb);
}

/// A TTS engine that synthesises speech by calling a cloud HTTP API.
#[derive(Debug)]
pub struct CloudEngine {
    pub(crate) config: CloudConfig,
    pub(crate) api_key: String,
    pub(crate) credentials: HashMap<String, String>,
    pub(crate) client: reqwest::blocking::Client,
}

impl CloudEngine {
    /// Create a cloud engine for the given provider `id`.
    ///
    /// Returns `None` if `id` is not a recognised cloud provider.
    ///
    /// Credential `synthUrl` (optional) overrides the provider's default
    /// synthesis endpoint. This is primarily useful for tests pointing at
    /// a deterministic local server, but also lets users target a proxy
    /// or self-hosted gateway.
    pub fn new(id: &str, credentials: &HashMap<String, String>) -> Option<Self> {
        let mut config = build_config(id, credentials)?;
        if let Some(url_override) = credentials.get("synthUrl") {
            if !url_override.is_empty() {
                config.synth_url.clone_from(url_override);
            }
        }
        let api_key = credentials
            .get("apiKey")
            .or_else(|| credentials.get("subscriptionKey"))
            .or_else(|| credentials.get("token"))
            .cloned()
            .unwrap_or_default();
        Some(CloudEngine {
            config,
            api_key,
            credentials: credentials.clone(),
            client: reqwest::blocking::Client::new(),
        })
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// Create a cloud engine from a JSON credentials string.
pub(crate) fn create_cloud_engine(id: &str, credentials_json: &str) -> Option<Arc<dyn TtsEngine>> {
    let creds: HashMap<String, String> = if credentials_json.is_empty() {
        HashMap::new()
    } else {
        serde_json::from_str(credentials_json).unwrap_or_default()
    };
    CloudEngine::new(id, &creds).map(|e| Arc::new(e) as Arc<dyn TtsEngine>)
}
