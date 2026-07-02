use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hya_plugin::config::{PluginEntry, PluginSpec};
use hya_plugin::manifest::Manifest;
use hya_plugin::messages::PluginKindWire;

pub fn plugins_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(cwd.join(".hya/plugins"))
}

pub fn resolve(config: BTreeMap<String, PluginEntry>, dir: Option<&Path>) -> Vec<PluginSpec> {
    resolve_with_bun(config, dir, find_bun)
}

fn resolve_with_bun(
    config: BTreeMap<String, PluginEntry>,
    dir: Option<&Path>,
    find_bun: impl Fn() -> Option<PathBuf>,
) -> Vec<PluginSpec> {
    let specs = raw_specs(config, dir);
    resolve_compat_specs(specs, find_bun)
}

fn raw_specs(config: BTreeMap<String, PluginEntry>, dir: Option<&Path>) -> Vec<PluginSpec> {
    let Some(dir) = dir else {
        return specs_from_config(config);
    };
    let manifests = scan_manifests(dir);
    hya_plugin::config::merge(config, manifests)
}

fn resolve_compat_specs(
    specs: Vec<PluginSpec>,
    find_bun: impl Fn() -> Option<PathBuf>,
) -> Vec<PluginSpec> {
    specs
        .into_iter()
        .filter_map(|mut spec| {
            if spec.kind != PluginKindWire::Compat || !spec.command.is_empty() {
                return Some(spec);
            }
            let Some(bun) = find_bun() else {
                // Bun is an OPTIONAL dependency, needed only to run Compat JS
                // plugins. The core and native Rust plugins never require it, so a
                // missing Bun is a skip (with notice), not an error.
                eprintln!(
                    "hya: skipping optional compat plugin '{}' — Bun not found in PATH \
                     (Bun is only needed for Compat JS plugins; native Rust plugins work without it)",
                    spec.id
                );
                return None;
            };
            spec.command = bundled_compat_adapter_command(&bun);
            Some(spec)
        })
        .collect()
}

fn bundled_compat_adapter_command(bun: &Path) -> Vec<String> {
    vec![
        path_to_arg(bun),
        "run".to_string(),
        path_to_arg(&bundled_compat_adapter_dir().join("src/main.ts")),
    ]
}

fn bundled_compat_adapter_dir() -> PathBuf {
    if let Some(dir) = non_empty_env_path("HYA_COMPAT_ADAPTER_DIR") {
        return dir;
    }
    let cli_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    cli_dir
        .parent()
        .map(|crates_dir| crates_dir.join("hya-plugin-compat/adapter"))
        .unwrap_or_else(|| cli_dir.join("../hya-plugin-compat/adapter"))
}

fn find_bun() -> Option<PathBuf> {
    if let Some(path) = non_empty_env_path("BUN") {
        return Some(path);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in bun_executable_names() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(windows)]
fn bun_executable_names() -> &'static [&'static str] {
    &["bun.exe", "bun.cmd", "bun.bat", "bun"]
}

#[cfg(not(windows))]
fn bun_executable_names() -> &'static [&'static str] {
    &["bun"]
}

fn path_to_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn specs_from_config(config: BTreeMap<String, PluginEntry>) -> Vec<PluginSpec> {
    hya_plugin::config::merge(config, Vec::new())
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
            Err(error) => eprintln!("hya: skipping plugin manifest {} ({error})", path.display()),
        }
    }
    manifests
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use hya_plugin::config::PluginEntry;
    use hya_plugin::messages::PluginKindWire;

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

    #[test]
    fn compat_entry_without_command_resolves_to_bundled_adapter() {
        let mut config = BTreeMap::new();
        config.insert(
            "compat".to_string(),
            PluginEntry {
                kind: PluginKindWire::Compat,
                command: Vec::new(),
                enabled: true,
                timeout_ms: Some(1000),
                env: BTreeMap::new(),
            },
        );

        let specs =
            super::resolve_with_bun(config, None, || Some(PathBuf::from("/usr/local/bin/bun")));

        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(spec.id, "compat");
        assert_eq!(spec.kind, PluginKindWire::Compat);
        assert_eq!(
            spec.command.first().map(String::as_str),
            Some("/usr/local/bin/bun")
        );
        assert_eq!(spec.command.get(1).map(String::as_str), Some("run"));
        assert!(
            spec.command
                .last()
                .is_some_and(|path| path.ends_with("src/main.ts"))
        );
        assert_eq!(spec.timeout_ms, Some(1000));
    }

    #[test]
    fn compat_entry_without_bun_is_skipped() {
        let mut config = BTreeMap::new();
        config.insert(
            "compat".to_string(),
            PluginEntry {
                kind: PluginKindWire::Compat,
                command: Vec::new(),
                enabled: true,
                timeout_ms: None,
                env: BTreeMap::new(),
            },
        );

        let specs = super::resolve_with_bun(config, None, || None);

        assert!(specs.is_empty());
    }

    #[test]
    fn rust_plugins_never_invoke_bun() {
        // The pure-Rust plugin path must not depend on Bun: resolving a `rust`
        // plugin must never call the bun locator.
        let mut config = BTreeMap::new();
        config.insert(
            "native".to_string(),
            PluginEntry {
                kind: PluginKindWire::Rust,
                command: vec!["my-plugin".to_string()],
                enabled: true,
                timeout_ms: None,
                env: BTreeMap::new(),
            },
        );
        let specs = super::resolve_with_bun(config, None, || panic!("bun must not be probed"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].command, vec!["my-plugin"]);
    }

    #[test]
    fn compat_entry_with_explicit_command_is_preserved() {
        let mut config = BTreeMap::new();
        config.insert(
            "compat".to_string(),
            PluginEntry {
                kind: PluginKindWire::Compat,
                command: vec!["custom-adapter".to_string(), "--stdio".to_string()],
                enabled: true,
                timeout_ms: None,
                env: BTreeMap::new(),
            },
        );

        let specs = super::resolve_with_bun(config, None, || None);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].command, vec!["custom-adapter", "--stdio"]);
    }
}
