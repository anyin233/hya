//! `plugins:` config entries and the merged `PluginSpec` the host consumes.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::manifest::Manifest;
use crate::messages::{HookName, HookPosture, PluginKindWire};

fn default_true() -> bool {
    true
}

/// One entry under the user config `plugins:` map (keyed by plugin id).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginEntry {
    /// Implementation kind (default `rust`).
    #[serde(default)]
    pub kind: PluginKindWire,
    /// argv for the child process (`command[0]` is the program).
    #[serde(default)]
    pub command: Vec<String>,
    /// When false, the entry is skipped by [`merge`].
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional per-call timeout override in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Extra environment variables for the child process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Fully resolved plugin to spawn: config entry or scanned manifest, after merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSpec {
    /// Stable plugin id (config key or manifest `id`).
    pub id: String,
    /// Implementation kind.
    pub kind: PluginKindWire,
    /// Child argv.
    pub command: Vec<String>,
    /// Optional per-call timeout in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Child environment additions (config only; manifests start empty).
    pub env: BTreeMap<String, String>,
    /// Hook posture overrides from the manifest (empty for pure config entries).
    pub posture_overrides: BTreeMap<HookName, HookPosture>,
}

/// Merge config `plugins:` entries with directory-scanned manifests into host specs.
///
/// Config wins for a given id: manifests with an id already present in `config`
/// are skipped. Disabled entries/manifests are omitted. Manifest hooks with an
/// explicit posture become [`PluginSpec::posture_overrides`].
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
