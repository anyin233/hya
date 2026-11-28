//! Per-bundle configuration and shared runtime state for `hya/network`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use hya_runtime_abi::{HostError, HostRuntime};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Canonical bundle identity for the built-in network bundle.
pub const BUNDLE_ID: &str = "hya/network";

/// Built-in search backends (MCP clients).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    /// Exa MCP at `mcp.exa.ai`.
    #[default]
    Exa,
    /// Parallel MCP at `search.parallel.ai`.
    Parallel,
}

impl WebSearchProvider {
    /// Stable provider id used in search result metadata.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Exa => "exa",
            Self::Parallel => "parallel",
        }
    }

    /// Human-readable provider label for search results.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Exa => "Exa Web Search",
            Self::Parallel => "Parallel Web Search",
        }
    }
}

/// Settings for the built-in `webfetch` tool in `hya/network`'s `config.yml`.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default)]
pub struct WebFetchConfig {
    /// When false, `webfetch` is hidden and refused.
    pub enabled: bool,
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Settings for the built-in `websearch` tool in `hya/network`'s `config.yml`.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default)]
pub struct WebSearchConfig {
    /// Search backend (`exa` or `parallel`).
    pub provider: WebSearchProvider,
    /// Optional MCP endpoint override.
    pub endpoint: Option<String>,
    /// API key (query param for Exa, bearer for Parallel).
    pub key: Option<String>,
    /// When false, `websearch` is hidden and refused.
    pub enabled: bool,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: WebSearchProvider::Exa,
            endpoint: None,
            key: None,
            enabled: true,
        }
    }
}

/// Top-level per-bundle configuration for `hya/network` (`config.yml`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default)]
pub struct NetworkConfig {
    /// `webfetch` tool exposure and settings.
    pub webfetch: WebFetchConfig,
    /// `websearch` tool exposure and search backend settings.
    pub websearch: WebSearchConfig,
}

impl NetworkConfig {
    /// Parse `hya/network` configuration from a JSON value supplied by
    /// `HostCatalogReader::bundle_config`.
    ///
    /// Missing or null values yield [`NetworkConfig::default`]. Supports both
    /// standard nested blocks (`webfetch:`, `websearch:`) and flat websearch
    /// configs for compatibility.
    ///
    /// # Errors
    /// Returns [`HostError::InvalidInput`] if a present `webfetch` or
    /// `websearch` section fails schema deserialization.
    pub fn from_json(value: &Value) -> Result<Self, HostError> {
        if value.is_null() {
            return Ok(Self::default());
        }
        let Some(map) = value.as_object() else {
            return Err(HostError::InvalidInput(
                "hya/network config must be a YAML/JSON mapping".to_string(),
            ));
        };
        if map.is_empty() {
            return Ok(Self::default());
        }
        let webfetch = match map.get("webfetch") {
            Some(section) if !section.is_null() => {
                serde_json::from_value::<WebFetchConfig>(section.clone()).map_err(|error| {
                    HostError::InvalidInput(format!("invalid webfetch config: {error}"))
                })?
            }
            _ => WebFetchConfig::default(),
        };
        let mut websearch = match map.get("websearch") {
            Some(section) if !section.is_null() => {
                serde_json::from_value::<WebSearchConfig>(section.clone()).map_err(|error| {
                    HostError::InvalidInput(format!("invalid websearch config: {error}"))
                })?
            }
            _ if map.contains_key("provider")
                || map.contains_key("endpoint")
                || map.contains_key("key") =>
            {
                serde_json::from_value::<WebSearchConfig>(value.clone()).map_err(|error| {
                    HostError::InvalidInput(format!("invalid websearch config: {error}"))
                })?
            }
            _ => WebSearchConfig::default(),
        };
        if let Some(key) = websearch.key.as_deref() {
            websearch.key = resolve_env_placeholder(key);
        }
        Ok(Self {
            webfetch,
            websearch,
        })
    }

    /// Parse `hya/network` configuration directly from YAML (`config.yml`).
    ///
    /// # Errors
    /// Returns [`HostError::InvalidInput`] on invalid YAML or schema mismatch.
    pub fn from_yaml(yaml: &str) -> Result<Self, HostError> {
        if yaml.trim().is_empty() {
            return Ok(Self::default());
        }
        let value: Value = serde_norway::from_str(yaml).map_err(|error| {
            HostError::InvalidInput(format!("parse network config.yml: {error}"))
        })?;
        Self::from_json(&value)
    }
}

/// Resolve `${env:VAR}` or `{env:VAR}` references in configured API keys.
fn resolve_env_placeholder(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let var_name = trimmed
        .strip_prefix("${env:")
        .and_then(|rest| rest.strip_suffix('}'))
        .or_else(|| {
            trimmed
                .strip_prefix("{env:")
                .and_then(|rest| rest.strip_suffix('}'))
        });
    match var_name {
        Some(name) => std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        None => Some(trimmed.to_string()),
    }
}

/// Shared runtime configuration store managed by [`crate::NetworkService`] and
/// consulted by `webfetch`, `websearch`, and the visibility hook.
pub struct NetworkPlane {
    bundle_id: String,
    config: RwLock<NetworkConfig>,
    loaded: AtomicBool,
    running: AtomicBool,
}

impl Default for NetworkPlane {
    fn default() -> Self {
        Self::new(BUNDLE_ID, NetworkConfig::default())
    }
}

impl NetworkPlane {
    /// Create a new plane bound to `bundle_id` with an initial `config`.
    #[must_use]
    pub fn new(bundle_id: impl Into<String>, config: NetworkConfig) -> Self {
        Self {
            bundle_id: bundle_id.into(),
            config: RwLock::new(config),
            loaded: AtomicBool::new(false),
            running: AtomicBool::new(false),
        }
    }

    /// Create a pre-configured plane (marked loaded) for direct tool construction or tests.
    #[must_use]
    pub fn configured(config: NetworkConfig) -> Arc<Self> {
        let plane = Self::new(BUNDLE_ID, config);
        plane.loaded.store(true, Ordering::Release);
        Arc::new(plane)
    }

    /// Owning bundle identity.
    #[must_use]
    pub fn bundle_id(&self) -> &str {
        &self.bundle_id
    }

    /// Snapshot the current [`NetworkConfig`].
    #[must_use]
    pub fn config(&self) -> NetworkConfig {
        self.config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replace the active [`NetworkConfig`] and mark it loaded.
    pub fn set_config(&self, config: NetworkConfig) {
        *self
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config;
        self.loaded.store(true, Ordering::Release);
    }

    /// Whether [`crate::NetworkService::start`] has started this plane.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Update the running lifecycle flag.
    pub(crate) fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::Release);
    }

    /// Reload configuration from `runtime.catalog_reader().bundle_config(...)`.
    ///
    /// # Errors
    /// Propagates invalid configuration or host read failures.
    pub async fn reload_from_host(
        &self,
        runtime: &dyn HostRuntime,
    ) -> Result<NetworkConfig, HostError> {
        let raw = runtime
            .catalog_reader()
            .bundle_config(&self.bundle_id)
            .await?;
        let parsed = NetworkConfig::from_json(&raw)?;
        self.set_config(parsed.clone());
        Ok(parsed)
    }

    /// Return the active configuration, lazily loading from `runtime` if the
    /// service has not yet populated it.
    pub(crate) async fn effective_config(
        &self,
        runtime: Option<&dyn HostRuntime>,
    ) -> NetworkConfig {
        if self.loaded.load(Ordering::Acquire) {
            return self.config();
        }
        if let Some(runtime) = runtime
            && let Ok(parsed) = self.reload_from_host(runtime).await
        {
            return parsed;
        }
        self.config()
    }
}
