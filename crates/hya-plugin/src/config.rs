//! `plugins:` config entries and the merged `PluginSpec` the host consumes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::Deserialize;

use crate::manifest::Manifest;
use crate::messages::{ActivationMetadata, PluginKindWire};

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
    /// Claude Code plugin source directory (required for `kind: claude`
    /// entries without an explicit command; ignored by other kinds).
    #[serde(default)]
    pub plugin_dir: Option<PathBuf>,
}

/// Launch policy retained by the connection owner for initial spawn and restart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProcessLaunch {
    /// Inherit the host directory and environment (config plugins).
    #[default]
    Config,
    /// Inherit the environment, using this Project root as cwd.
    Project(PathBuf),
    /// Clear the environment and use private packaged files as cwd.
    Bundle {
        /// Materialized package directory, retained by the source or activation.
        root: PathBuf,
        /// A private session activation; loss must reach its supervisor, not respawn.
        activation: Option<ActivationMetadata>,
    },
}

/// One process descriptor for config plugins, Project plugins and bundle processes.
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
    /// Claude Code plugin source directory (`kind: claude` only).
    pub plugin_dir: Option<PathBuf>,
    /// Directory, environment isolation and optional activation handshake.
    pub launch: ProcessLaunch,
}

impl PluginSpec {
    /// Resolve a Project plugin's launch directory before connecting it.
    #[must_use]
    pub fn in_project(mut self, root: PathBuf) -> Self {
        self.launch = ProcessLaunch::Project(root);
        self
    }

    /// Resolve a package process's private directory before connecting it.
    #[must_use]
    pub fn in_bundle(mut self, root: PathBuf) -> Self {
        self.launch = ProcessLaunch::Bundle {
            root,
            activation: None,
        };
        self
    }

    pub(crate) fn is_activation(&self) -> bool {
        matches!(
            self.launch,
            ProcessLaunch::Bundle {
                activation: Some(_),
                ..
            }
        )
    }

    pub(crate) fn has_capabilities(&self) -> bool {
        matches!(
            self.launch,
            ProcessLaunch::Bundle {
                activation: None,
                ..
            }
        )
    }
}

/// Merge config `plugins:` entries with directory-scanned manifests into host specs.
///
/// Config wins for a given id: manifests with an id already present in `config`
/// are skipped. Disabled entries/manifests are omitted.
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
            plugin_dir: entry.plugin_dir,
            launch: Default::default(),
        });
    }

    for manifest in manifests {
        if seen.contains(&manifest.id) || !manifest.enabled {
            continue;
        }
        specs.push(PluginSpec {
            id: manifest.id.clone(),
            kind: manifest.kind,
            command: manifest.command,
            timeout_ms: manifest.timeout_ms,
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: Default::default(),
        });
    }

    specs
}
