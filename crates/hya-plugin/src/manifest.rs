//! `plugin.toml` manifest for directory-scanned plugins.

use serde::Deserialize;

use crate::messages::PluginKindWire;

fn default_true() -> bool {
    true
}

/// On-disk plugin declaration (`plugin.toml`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Stable plugin id (must match initialize `plugin.id`).
    pub id: String,
    /// Implementation kind (default `rust`).
    #[serde(default)]
    pub kind: PluginKindWire,
    /// Child process argv.
    pub command: Vec<String>,
    /// When false, [`crate::config::merge`] skips this manifest.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional per-call timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl Manifest {
    /// # Errors
    /// Returns the TOML parse error string on malformed input.
    pub fn parse(toml_str: &str) -> Result<Self, String> {
        toml::from_str(toml_str).map_err(|e| e.to_string())
    }
}
