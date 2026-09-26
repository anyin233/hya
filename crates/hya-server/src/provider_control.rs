//! Dependency-inverted control port for provider management: saved keys,
//! provider upsert, remote model refresh, and config model overrides.
//!
//! The server owns the wire contract and route-facing error codes. The
//! application runtime (`hya-app`) supplies the implementation, which owns
//! `config.yaml`, the auth directory, the model cache, and the live
//! route/catalog rebuild on the shared engine. Every mutating call returns
//! only after the rebuilt router and catalog are published on the engine, so
//! the handler can answer from `engine.provider_catalog_snapshot()`.

use futures::future::BoxFuture;

/// Stable code: the application did not install a provider control.
pub const PROVIDER_CONTROL_UNAVAILABLE: &str = "PROVIDER_CONTROL_UNAVAILABLE";
/// Stable code: malformed id, kind, base URL, key, or model metadata.
pub const PROVIDER_INVALID_REQUEST: &str = "PROVIDER_INVALID_REQUEST";
/// Stable code: the provider (or model entry) is not in `config.yaml`.
pub const PROVIDER_NOT_FOUND: &str = "PROVIDER_NOT_FOUND";
/// Stable code: config, credential, cache, or rebuild failure.
pub const PROVIDER_CONTROL_FAILURE: &str = "PROVIDER_CONTROL_FAILURE";

/// Where one provider's credential comes from (never the secret itself).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderKeySource {
    /// An API key saved in `auth/<id>.yaml`.
    Saved,
    /// A saved OAuth bundle in `auth/<id>.yaml`.
    Oauth,
    /// An inline `api_key` in `config.yaml`.
    Config,
    /// No credential.
    None,
}

impl ProviderKeySource {
    /// Wire label: `saved`, `oauth`, `config`, or `none`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Saved => "saved",
            Self::Oauth => "oauth",
            Self::Config => "config",
            Self::None => "none",
        }
    }
}

/// Non-secret declaration of one configured provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSettings {
    /// Provider id (the `providers.<id>` key).
    pub id: String,
    /// Config `kind` label as written (`openai`, `anthropic`, …).
    pub kind: String,
    /// Config `base_url`.
    pub base_url: String,
    /// Where the credential comes from.
    pub key_source: ProviderKeySource,
}

/// Request to add or update one provider declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderUpsert {
    /// Provider id.
    pub id: String,
    /// Config `kind` label.
    pub kind: String,
    /// Config `base_url`.
    pub base_url: String,
    /// API key to save; `None` keeps the current credential.
    pub api_key: Option<String>,
}

/// Metadata written into one model's `models:` entry (replace semantics:
/// `None` removes that field from the entry).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderModelOverride {
    /// Entry `name`.
    pub display_name: Option<String>,
    /// Entry `limit.context`.
    pub context_limit: Option<u32>,
    /// Entry `limit.output`.
    pub output_limit: Option<u32>,
    /// Entry `reasoning: true|false`.
    pub reasoning: Option<bool>,
}

/// Outcome of one remote model-list fetch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDiscoveryReport {
    /// True when the list was fetched and parsed (possibly empty).
    pub ok: bool,
    /// `models`, `empty`, `auth_required`, `auth_rejected`, `unavailable`,
    /// `invalid`, or `unsupported`.
    pub result: String,
    /// Bounded, non-secret failure description.
    pub error_message: Option<String>,
    /// Remote models fetched and cached.
    pub model_count: usize,
}

/// Result of a mutating provider call, after the live rebuild.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderChange {
    /// Whether the provider exists in `config.yaml` after the call.
    pub configured: bool,
    /// Remote fetch outcome when the call fetched.
    pub discovery: Option<ProviderDiscoveryReport>,
}

/// Bounded structured failure returned by the provider control port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderControlError {
    /// Machine-readable stable code.
    pub code: String,
    /// Bounded human-readable diagnostic.
    pub message: String,
}

impl ProviderControlError {
    /// Construct one bounded failure (code ≤ 128, message ≤ 2,048 scalars).
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: bounded(code.into(), 128),
            message: bounded(message.into(), 2_048),
        }
    }

    /// The canonical unavailable-control failure.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::new(
            PROVIDER_CONTROL_UNAVAILABLE,
            "provider control is unavailable",
        )
    }
}

impl std::fmt::Display for ProviderControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProviderControlError {}

/// Boxed asynchronous result returned by a [`ProviderControl`] operation.
pub type ProviderControlFuture<'a, T> = BoxFuture<'a, Result<T, ProviderControlError>>;

/// Server-owned control port for provider keys, declarations, remote model
/// refresh, and config model overrides. Implementations serialize mutating
/// calls and publish the rebuilt route/catalog on the engine before
/// resolving.
pub trait ProviderControl: Send + Sync {
    /// Whether a real application-owned control is installed.
    fn available(&self) -> bool;

    /// Sorted provider ids with a saved credential file.
    fn list_saved_keys(&self) -> ProviderControlFuture<'_, Vec<String>>;

    /// Non-secret declarations of every configured provider.
    fn list_settings(&self) -> ProviderControlFuture<'_, Vec<ProviderSettings>>;

    /// Save an API key and rebuild the provider live (fetching its remote
    /// list when the model cache has no rows for it).
    fn set_key(
        &self,
        provider_id: String,
        key: String,
    ) -> ProviderControlFuture<'_, ProviderChange>;

    /// Delete the saved credential and rebuild the provider live. Returns
    /// whether a credential file existed.
    fn remove_key(&self, provider_id: String) -> ProviderControlFuture<'_, (bool, ProviderChange)>;

    /// Add or update a provider declaration (and key), fetch its remote
    /// list, and apply it live.
    fn upsert_provider(&self, request: ProviderUpsert)
    -> ProviderControlFuture<'_, ProviderChange>;

    /// Fetch one configured provider's remote list and apply it live.
    fn refresh_provider(&self, provider_id: String) -> ProviderControlFuture<'_, ProviderChange>;

    /// Write one model's config entry and apply it live.
    fn set_model(
        &self,
        provider_id: String,
        model_id: String,
        metadata: ProviderModelOverride,
    ) -> ProviderControlFuture<'_, ProviderChange>;

    /// Remove one model's config entry and apply it live.
    fn remove_model(
        &self,
        provider_id: String,
        model_id: String,
    ) -> ProviderControlFuture<'_, ProviderChange>;
}

/// Default control used by callers that do not install an application runtime.
pub(crate) struct EmptyProviderControl;

impl ProviderControl for EmptyProviderControl {
    fn available(&self) -> bool {
        false
    }

    fn list_saved_keys(&self) -> ProviderControlFuture<'_, Vec<String>> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }

    fn list_settings(&self) -> ProviderControlFuture<'_, Vec<ProviderSettings>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn set_key(&self, _: String, _: String) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }

    fn remove_key(&self, _: String) -> ProviderControlFuture<'_, (bool, ProviderChange)> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }

    fn upsert_provider(&self, _: ProviderUpsert) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }

    fn refresh_provider(&self, _: String) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }

    fn set_model(
        &self,
        _: String,
        _: String,
        _: ProviderModelOverride,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }

    fn remove_model(&self, _: String, _: String) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }
}

/// Validate a provider id: 1-64 ASCII letters, digits, `-`, or `_`.
#[must_use]
pub fn valid_provider_id(provider_id: &str) -> bool {
    !provider_id.is_empty()
        && provider_id.len() <= 64
        && provider_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Truncate a string by Unicode scalar count only when it exceeds the bound.
fn bounded(value: String, limit: usize) -> String {
    let Some((end, _)) = value.char_indices().nth(limit) else {
        return value;
    };
    value[..end].to_string()
}
