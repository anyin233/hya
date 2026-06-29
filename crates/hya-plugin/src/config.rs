//! `plugins:` config entries and the merged [`PluginSpec`] the host consumes.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::manifest::Manifest;
use crate::messages::{HookName, HookPosture, PluginKindWire};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginEntry {
    #[serde(default)]
    pub kind: PluginKindWire,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSpec {
    pub id: String,
    pub kind: PluginKindWire,
    pub command: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub env: BTreeMap<String, String>,
    pub posture_overrides: BTreeMap<HookName, HookPosture>,
}

#[must_use]
pub fn merge(config: BTreeMap<String, PluginEntry>, manifests: Vec<Manifest>) -> Vec<PluginSpec> {
    let mut specs = Vec::new();
    let mut seen = BTreeSet::new();

    for (id, entry) in config {
        seen.insert(id.clone());
        if !entry.enabled {
            continue;
        }
        specs.push(PluginSpec {
            id,
            kind: entry.kind,
            command: entry.command,
            timeout_ms: entry.timeout_ms,
            env: entry.env,
            posture_overrides: BTreeMap::new(),
        });
    }

    for manifest in manifests {
        if seen.contains(&manifest.id) || !manifest.enabled {
            continue;
        }
        let posture_overrides = manifest
            .resolved_hooks()
            .into_iter()
            .filter_map(|(name, posture)| posture.map(|p| (name, p)))
            .collect();
        specs.push(PluginSpec {
            id: manifest.id.clone(),
            kind: manifest.kind,
            command: manifest.command,
            timeout_ms: manifest.timeout_ms,
            env: BTreeMap::new(),
            posture_overrides,
        });
    }

    specs
}
