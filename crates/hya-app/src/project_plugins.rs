//! Per-Project plugin tier: the `plugin.toml` plugins of a registered Project.
//!
//! A Project's plugins come from `<root>/.hya/plugins/<name>/plugin.toml` of
//! every Project root, in root order. The first root to declare a plugin id
//! wins (later roots' manifests with that id are skipped with a warning), and
//! a plugin declared in `config.yaml` beats every manifest with its id: the
//! config plugin is process-wide, the manifest is skipped with a warning.
//!
//! Each plugin process starts in the root that holds its `.hya/plugins`
//! directory (so a relative `command` resolves against the root) the first
//! time the Project is bound, and is published as a Plugin-kind
//! [`RuntimeSource`] of the Project's scope overlay: its tools and hooks
//! (including the `permission.ask` interceptor) reach only bindings of that
//! Project. The process lives as long as the source: dropping or evicting the
//! overlay (and releasing every binding that retains it) stops it.
//!
//! Workspace adapters declared by a project plugin are not supported and are
//! ignored with a warning.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use hya_core::{RuntimeSource, RuntimeSourceId};
use hya_plugin::{HostInfo, PluginHost};

use crate::runtime_reconcile::prepared_plugin_host;

/// How a [`crate::ProjectScopeRefresh`] loads project plugins.
#[derive(Clone, Debug)]
pub struct ProjectPluginSettings {
    enabled: bool,
    configured: BTreeSet<String>,
    host: HostInfo,
}

impl Default for ProjectPluginSettings {
    fn default() -> Self {
        Self::new(crate::runtime::host_info())
    }
}

impl ProjectPluginSettings {
    /// Load project plugins, identifying the host as `host` to each plugin.
    #[must_use]
    pub fn new(host: HostInfo) -> Self {
        Self {
            enabled: true,
            configured: BTreeSet::new(),
            host,
        }
    }

    /// Load no project plugins (`--pure`).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Plugin ids declared in `config.yaml` (enabled or not): a project
    /// manifest with one of these ids is skipped.
    #[must_use]
    pub fn with_configured_ids(mut self, ids: impl IntoIterator<Item = String>) -> Self {
        self.configured.extend(ids);
        self
    }

    /// Whether project plugins load at all.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

#[derive(Default)]
pub(crate) struct ProjectPlugins {
    records: BTreeMap<PathBuf, PluginRecord>,
}

struct PluginRecord {
    digest: Option<[u8; 32]>,
    source: Option<RuntimeSourceId>,
    error: Option<String>,
}

impl ProjectPlugins {
    /// Reconcile each manifest independently, retaining failed sources by path
    /// even when their new manifest no longer parses to an id.
    pub(crate) async fn refresh(
        &mut self,
        roots: &[PathBuf],
        settings: &ProjectPluginSettings,
        retained: &[RuntimeSource],
        explicit: bool,
    ) -> (Vec<RuntimeSource>, Vec<String>, bool) {
        if !settings.enabled {
            self.records.clear();
            return (Vec::new(), Vec::new(), false);
        }
        let mut changed = false;
        let mut configured = settings.configured.clone();
        configured.extend(crate::config::load_configured_plugin_ids());
        let mut present = BTreeSet::new();
        let mut seen = configured;
        let mut sources = Vec::new();
        let mut errors = Vec::new();
        for root in roots {
            let inputs = match crate::plugins::project_plugin_inputs(root) {
                Ok(inputs) => inputs,
                Err(error) => {
                    errors.push(format!("project plugins {}: {error}", root.display()));
                    for (path, record) in &self.records {
                        if path.starts_with(crate::plugins::root_plugins_dir(root)) {
                            present.insert(path.clone());
                            if let Some(id) = &record.source
                                && let Some(source) =
                                    retained.iter().find(|source| source.id() == id)
                            {
                                seen.insert(id.configured_id().to_string());
                                sources.push(source.clone());
                            }
                        }
                    }
                    continue;
                }
            };
            for crate::plugins::ProjectPluginInput { path, digest, spec } in inputs {
                present.insert(path.clone());
                let previous = self.records.entry(path.clone()).or_insert(PluginRecord {
                    digest: None,
                    source: None,
                    error: None,
                });
                let mut source = previous
                    .source
                    .as_ref()
                    .and_then(|id| retained.iter().find(|source| source.id() == id))
                    .cloned();
                // An invalid manifest still claims its last published id.
                let id = match &spec {
                    Ok(Some(spec)) => Some(spec.id.as_str()),
                    Err(_) => previous
                        .source
                        .as_ref()
                        .map(|source| source.configured_id()),
                    Ok(None) => None,
                };
                if let Some(id) = id
                    && !seen.insert(id.to_string())
                {
                    tracing::warn!(plugin = %id, manifest = %path.display(), "skipping a project plugin shadowed by config or an earlier root");
                    changed |= previous.source.is_some();
                    previous.source = None;
                    previous.digest = None;
                    previous.error = None;
                    continue;
                }
                if previous.digest != Some(digest) || (explicit && previous.error.is_some()) {
                    previous.digest = Some(digest);
                    let prepared = match spec {
                        Err(error) => Err(error),
                        Ok(None) => Ok(None),
                        Ok(Some(spec)) => {
                            let id = spec.id.clone();
                            let (host, failures) = PluginHost::connect_all_observed(
                                vec![spec.in_project(root.clone())],
                                settings.host.clone(),
                            )
                            .await;
                            match failures.get(&id) {
                                Some(error) => Err(error.to_string()),
                                None => plugin_source(host, &id).map(Some),
                            }
                        }
                    };
                    match prepared {
                        Ok(prepared) => {
                            changed |= prepared.is_some() || previous.source.is_some();
                            previous.source = prepared.as_ref().map(|source| source.id().clone());
                            source = prepared;
                            previous.error = None;
                        }
                        Err(error) => {
                            previous.error = Some(error);
                        }
                    }
                }
                if let Some(error) = &previous.error {
                    errors.push(format!("{}: {error}", path.display()));
                }
                if let Some(source) = source {
                    sources.push(source);
                }
            }
        }
        self.records.retain(|path, record| {
            let keep = present.contains(path);
            changed |= !keep && record.source.is_some();
            keep
        });
        (sources, errors, changed)
    }
}

/// Publish one connected single-plugin host as a Plugin-kind source carrying
/// its tools, Skills, and (when it registers any) hooks.
fn plugin_source(host: PluginHost, id: &str) -> Result<RuntimeSource, String> {
    for plugin in host.prepared_plugins() {
        if !plugin.contributions().workspace_adapters.is_empty() {
            tracing::warn!(plugin = %id, "ignoring workspace adapters declared by a project plugin (unsupported)");
        }
    }
    prepared_plugin_host(host)
        .map(|prepared| prepared.into_runtime_source())
        .map_err(|error| error.to_string())
}
