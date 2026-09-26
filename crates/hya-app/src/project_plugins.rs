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

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use hya_core::RuntimeSource;
use hya_core::hooks::HookDispatcher;
use hya_plugin::config::PluginSpec;
use hya_plugin::{HostInfo, PluginHost};

use crate::runtime_reconcile::prepared_plugin_source;

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

/// Start the plugins of a Project with `roots` and return one Plugin-kind
/// source per plugin that started. Failures are logged and skipped.
pub(crate) async fn load_project_plugins(
    roots: &[PathBuf],
    settings: &ProjectPluginSettings,
) -> Vec<RuntimeSource> {
    if !settings.enabled {
        return Vec::new();
    }
    let roots = roots.to_vec();
    let configured = settings.configured.clone();
    let planned = match tokio::task::spawn_blocking(move || {
        plan_project_plugins(&roots, &configured)
    })
    .await
    {
        Ok(planned) => planned,
        Err(error) => {
            tracing::warn!(%error, "scanning the Project's plugin manifests failed");
            return Vec::new();
        }
    };
    let mut set = tokio::task::JoinSet::new();
    for (index, (root, spec)) in planned.into_iter().enumerate() {
        let host = settings.host.clone();
        set.spawn(async move {
            let id = spec.id.clone();
            let (connected, failures) =
                PluginHost::connect_all_observed_in(vec![spec], root.clone(), host).await;
            (index, id, root, connected, failures)
        });
    }
    let mut started = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((index, id, root, connected, failures)) => {
                if let Some(error) = failures.get(&id) {
                    tracing::warn!(
                        plugin = %id,
                        root = %root.display(),
                        %error,
                        "project plugin failed to start"
                    );
                    continue;
                }
                if let Some(source) = plugin_source(connected, &id) {
                    started.push((index, source));
                }
            }
            Err(error) => tracing::warn!(%error, "project plugin start task failed"),
        }
    }
    started.sort_by_key(|(index, _)| *index);
    started.into_iter().map(|(_, source)| source).collect()
}

/// The specs to start with the root each runs in: roots in order, first root
/// wins by plugin id, configured ids skip the manifest.
fn plan_project_plugins(
    roots: &[PathBuf],
    configured: &BTreeSet<String>,
) -> Vec<(PathBuf, PluginSpec)> {
    let mut seen = BTreeSet::new();
    let mut planned = Vec::new();
    for root in roots {
        for spec in crate::plugins::project_plugin_specs(root) {
            if configured.contains(&spec.id) {
                tracing::warn!(
                    plugin = %spec.id,
                    root = %root.display(),
                    "skipping a project plugin: config.yaml declares a plugin with this id"
                );
                continue;
            }
            if !seen.insert(spec.id.clone()) {
                tracing::warn!(
                    plugin = %spec.id,
                    root = %root.display(),
                    "skipping a project plugin: an earlier Project root declares this id"
                );
                continue;
            }
            planned.push((root.clone(), spec));
        }
    }
    planned
}

/// Publish one connected single-plugin host as a Plugin-kind source carrying
/// its tools, Skills, and (when it registers any) hooks.
fn plugin_source(host: PluginHost, id: &str) -> Option<RuntimeSource> {
    let plugin = host.prepared_plugins().into_iter().next()?;
    let contributions = plugin.contributions();
    if !contributions.workspace_adapters.is_empty() {
        tracing::warn!(
            plugin = %id,
            adapters = contributions.workspace_adapters.len(),
            "ignoring workspace adapters declared by a project plugin (unsupported)"
        );
    }
    let has_hooks = !contributions.hooks.is_empty();
    let source = match prepared_plugin_source(plugin) {
        Ok(prepared) => prepared.into_runtime_source(),
        Err(error) => {
            tracing::warn!(plugin = %id, %error, "project plugin contribution rejected");
            return None;
        }
    };
    Some(if has_hooks {
        source.with_hooks(Arc::new(host) as Arc<dyn HookDispatcher>)
    } else {
        source
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn write_manifest(root: &std::path::Path, dir: &str, id: &str) {
        let dir = root.join(".hya/plugins").join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            format!("id = \"{id}\"\ncommand = [\"true\"]\n"),
        )
        .unwrap();
    }

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hya-project-plugins-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn plan_takes_roots_in_order_first_wins_and_skips_configured_ids() {
        let first = temp_root("first");
        let second = temp_root("second");
        write_manifest(&first, "a", "shared");
        write_manifest(&first, "b", "configured");
        write_manifest(&second, "a", "shared");
        write_manifest(&second, "c", "only-second");
        let configured = BTreeSet::from(["configured".to_string()]);

        let planned = plan_project_plugins(&[first.clone(), second.clone()], &configured);
        let rows = planned
            .iter()
            .map(|(root, spec)| (root.clone(), spec.id.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                (first.clone(), "shared".to_string()),
                (second.clone(), "only-second".to_string()),
            ]
        );

        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }
}
