use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use yaca_plugin::config::{PluginEntry, PluginSpec};
use yaca_plugin::manifest::Manifest;

pub fn plugins_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(cwd.join(".yaca/plugins"))
}

pub fn resolve(config: BTreeMap<String, PluginEntry>, dir: Option<&Path>) -> Vec<PluginSpec> {
    let Some(dir) = dir else {
        return specs_from_config(config);
    };
    let manifests = scan_manifests(dir);
    yaca_plugin::config::merge(config, manifests)
}

fn specs_from_config(config: BTreeMap<String, PluginEntry>) -> Vec<PluginSpec> {
    yaca_plugin::config::merge(config, Vec::new())
}

fn scan_manifests(dir: &Path) -> Vec<Manifest> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut manifests = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("plugin.toml");
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        match Manifest::parse(&contents) {
            Ok(manifest) => manifests.push(manifest),
            Err(error) => eprintln!(
                "yaca: skipping plugin manifest {} ({error})",
                path.display()
            ),
        }
    }
    manifests
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use yaca_plugin::config::PluginEntry;
    use yaca_plugin::messages::PluginKindWire;

    fn entry(enabled: bool, command: Vec<String>) -> PluginEntry {
        PluginEntry {
            kind: PluginKindWire::Rust,
            command,
            enabled,
            timeout_ms: None,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn specs_from_config_filters_disabled_plugins() {
        let mut config = BTreeMap::new();
        config.insert(
            "enabled".to_string(),
            entry(true, vec!["plugin-bin".to_string()]),
        );
        config.insert(
            "disabled".to_string(),
            entry(false, vec!["ignored".to_string()]),
        );

        let specs = super::specs_from_config(config);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].id, "enabled");
        assert_eq!(specs[0].command, vec!["plugin-bin"]);
    }
}
