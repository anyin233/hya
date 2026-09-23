//! Web-search plane and provider configuration for the `websearch` tool.

use std::sync::Arc;

use serde::Deserialize;

/// Holds shared web-search configuration for tools.
#[derive(Clone)]
pub struct WebSearchPlane {
    config: Arc<WebSearchConfig>,
}

/// User/config settings for the built-in web search tool.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    /// Search backend.
    pub provider: WebSearchProvider,
    /// Optional endpoint override.
    pub endpoint: Option<String>,
    /// API key (query param for Exa, bearer for Parallel).
    pub key: Option<String>,
    /// When false, the tool should not be advertised.
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

impl Default for WebSearchPlane {
    fn default() -> Self {
        Self::configured(WebSearchConfig::default())
    }
}

impl WebSearchPlane {
    /// Convenience constructor with an explicit provider and endpoint URL.
    #[must_use]
    pub fn new(provider: WebSearchProvider, url: String) -> Self {
        Self::configured(WebSearchConfig {
            provider,
            endpoint: Some(url),
            ..WebSearchConfig::default()
        })
    }

    /// Wrap an arbitrary config.
    #[must_use]
    pub fn configured(config: WebSearchConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    /// Configuration captured for the current tool call.
    #[must_use]
    pub fn config(&self) -> &WebSearchConfig {
        &self.config
    }
}

/// Built-in search backends (MCP clients).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    /// Exa MCP at `mcp.exa.ai`.
    Exa,
    /// Parallel MCP at `search.parallel.ai`.
    Parallel,
}

impl WebSearchProvider {
    /// Stable provider id used in search result metadata.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            WebSearchProvider::Exa => "exa",
            WebSearchProvider::Parallel => "parallel",
        }
    }

    /// Human-readable provider label for search results.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            WebSearchProvider::Exa => "Exa Web Search",
            WebSearchProvider::Parallel => "Parallel Web Search",
        }
    }
}
