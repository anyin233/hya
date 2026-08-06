//! `plugin.toml` manifest for directory-scanned plugins.

use serde::Deserialize;

use crate::messages::{HookName, HookPosture, PluginKindWire};

fn default_true() -> bool {
    true
}

/// On-disk plugin declaration (`plugin.toml`).
#[derive(Debug, Clone, Deserialize)]
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
    /// Declared hooks (unknown wire names are dropped with a warning).
    #[serde(default)]
    pub hooks: Vec<ManifestHook>,
}

/// One hook row inside a [`Manifest`].
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestHook {
    /// Wire hook name (for example `tool.execute.before`).
    pub name: String,
    /// Optional posture override for this hook.
    #[serde(default)]
    pub posture: Option<HookPosture>,
}

impl Manifest {
    /// # Errors
    /// Returns the TOML parse error string on malformed input.
    pub fn parse(toml_str: &str) -> Result<Self, String> {
        toml::from_str(toml_str).map_err(|e| e.to_string())
    }

    /// Resolve manifest hook rows into typed names, dropping unknown wire names.
    #[must_use]
    pub fn resolved_hooks(&self) -> Vec<(HookName, Option<HookPosture>)> {
        let mut out = Vec::new();
        for hook in &self.hooks {
            match HookName::from_wire(&hook.name) {
                Some(name) => out.push((name, hook.posture)),
                None => {
                    tracing::warn!(plugin = %self.id, hook = %hook.name, "unknown hook in plugin.toml; dropped");
                }
            }
        }
        out
    }
}
