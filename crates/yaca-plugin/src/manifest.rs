//! `plugin.toml` manifest for directory-scanned plugins.

use serde::Deserialize;

use crate::messages::{HookName, HookPosture, PluginKindWire};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    #[serde(default)]
    pub kind: PluginKindWire,
    pub command: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub hooks: Vec<ManifestHook>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestHook {
    pub name: String,
    #[serde(default)]
    pub posture: Option<HookPosture>,
}

impl Manifest {
    /// # Errors
    /// Returns the TOML parse error string on malformed input.
    pub fn parse(toml_str: &str) -> Result<Self, String> {
        toml::from_str(toml_str).map_err(|e| e.to_string())
    }

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
