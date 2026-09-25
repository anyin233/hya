//! Application-owned provider configuration behind the v1 setup route.

/// Non-secret provider declaration to persist on the backend host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSetupSpec {
    /// Provider id, also used to resolve a saved auth credential.
    pub provider_id: String,
    /// Hya provider kind.
    pub kind: String,
    /// Upstream API root.
    pub base_url: String,
    /// Explicit provider-local model ids.
    pub model_ids: Vec<String>,
    /// Select the first model for new sessions after restart.
    pub make_default: bool,
}

/// Backend composition callback that persists a provider declaration.
pub type ProviderSetupControl = dyn Fn(ProviderSetupSpec) -> Result<(), String> + Send + Sync;
