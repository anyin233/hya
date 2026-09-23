//! Resolve native, Bun extension, and Claude Code plugin specs for the plugin host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hya_plugin::config::{PluginEntry, PluginSpec};
use hya_plugin::manifest::Manifest;
use hya_plugin::messages::PluginKindWire;

/// Project-local plugin directory: `$CWD/.hya/plugins`, when cwd is available.
pub fn plugins_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(cwd.join(".hya/plugins"))
}

/// Merge config plugin entries with optional on-disk manifests into host specs.
///
/// Bun entries without a command are rewritten to the bundled Bun adapter and
/// `claude` entries without a command to the bundled Claude adapter when Bun
/// is on `PATH` (or `BUN`); otherwise they are skipped with a notice.
pub fn resolve(config: BTreeMap<String, PluginEntry>, dir: Option<&Path>) -> Vec<PluginSpec> {
    resolve_with_bun(config, dir, find_bun)
}

pub(crate) fn bundle_sidecar_command() -> Option<Vec<String>> {
    find_bun().map(|bun| bundled_bun_adapter_command(&bun))
}

/// Bun executable used to host the TypeScript adapters (env `BUN`, then `PATH`).
#[must_use]
pub fn find_bun() -> Option<PathBuf> {
    find_bun_path()
}

/// Resolve the bundled Claude adapter directory for spawn and install paths.
#[must_use]
pub fn claude_adapter_dir() -> PathBuf {
    let executable = std::env::current_exe().unwrap_or_default();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    resolve_claude_adapter_dir(
        non_empty_env_path("HYA_CLAUDE_ADAPTER_DIR"),
        &executable,
        workspace_root,
    )
}

fn resolve_with_bun(
    config: BTreeMap<String, PluginEntry>,
    dir: Option<&Path>,
    find_bun: impl Fn() -> Option<PathBuf>,
) -> Vec<PluginSpec> {
    let specs = raw_specs(config, dir);
    resolve_js_specs(specs, find_bun)
}

fn raw_specs(config: BTreeMap<String, PluginEntry>, dir: Option<&Path>) -> Vec<PluginSpec> {
    let Some(dir) = dir else {
        return specs_from_config(config);
    };
    let manifests = scan_manifests(dir);
    hya_plugin::config::merge(config, manifests)
}

fn resolve_js_specs(
    specs: Vec<PluginSpec>,
    find_bun: impl Fn() -> Option<PathBuf>,
) -> Vec<PluginSpec> {
    specs
        .into_iter()
        .filter_map(|mut spec| {
            if !spec.command.is_empty() {
                return Some(spec);
            }
            match spec.kind {
                PluginKindWire::Bun => {}
                PluginKindWire::Claude => {
                    let Some(plugin_dir) = spec.plugin_dir.clone() else {
                        // A Claude entry without a plugin source has nothing to
                        // discover or translate; skip rather than fail startup.
                        eprintln!(
                            "hya: skipping claude plugin '{}' — no `plugin_dir` configured \
                             (kind: claude entries need the Claude Code plugin directory)",
                            spec.id
                        );
                        return None;
                    };
                    let Some(bun) = find_bun() else {
                        spawn_without_bun_notice(&spec.id, "claude");
                        return None;
                    };
                    spec.command = bundled_claude_adapter_command(&bun, &spec.id, &plugin_dir);
                    return Some(spec);
                }
                _ => return Some(spec),
            }
            let Some(bun) = find_bun() else {
                // Bun is an OPTIONAL dependency, needed only to run JS extension
                // plugins. The core and native Rust plugins never require it, so a
                // missing Bun is a skip (with notice), not an error.
                spawn_without_bun_notice(&spec.id, "bun");
                return None;
            };
            spec.command = bundled_bun_adapter_command(&bun);
            Some(spec)
        })
        .collect()
}

fn spawn_without_bun_notice(id: &str, kind: &str) {
    eprintln!(
        "hya: skipping optional {kind} plugin '{id}' — Bun not found in PATH \
         (Bun is only needed for JS extension plugins; native Rust plugins work without it)",
    );
}

fn bundled_bun_adapter_command(bun: &Path) -> Vec<String> {
    vec![
        path_to_arg(bun),
        "run".to_string(),
        path_to_arg(&bundled_bun_adapter_dir().join("src/main.ts")),
    ]
}

/// Claude adapter argv: `bun run <adapter>/src/main.ts --plugin-dir <dir>
/// --plugin-id <id>`. The configured id is echoed on the initialize reply
/// because the host enforces the configured-id match.
fn bundled_claude_adapter_command(bun: &Path, id: &str, plugin_dir: &Path) -> Vec<String> {
    vec![
        path_to_arg(bun),
        "run".to_string(),
        path_to_arg(&claude_adapter_dir().join("src/main.ts")),
        "--plugin-dir".to_string(),
        path_to_arg(plugin_dir),
        "--plugin-id".to_string(),
        id.to_string(),
    ]
}

/// Resolve the production Bun adapter for configured plugins and bundle sidecars.
fn bundled_bun_adapter_dir() -> PathBuf {
    let executable = std::env::current_exe().unwrap_or_default();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    resolve_bun_adapter_dir(
        non_empty_env_path("HYA_BUN_ADAPTER_DIR"),
        &executable,
        workspace_root,
    )
}

/// Resolve the Bun adapter in override, installed-adjacent, then workspace order.
fn resolve_bun_adapter_dir(
    override_dir: Option<PathBuf>,
    executable: &Path,
    workspace_root: &Path,
) -> PathBuf {
    if let Some(override_dir) = override_dir {
        return override_dir;
    }
    let installed = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("../lib/hya/bun-adapter");
    if installed.join("src/main.ts").is_file() {
        return installed;
    }
    workspace_root.join("crates/hya-plugin-bun/adapter")
}

/// Resolve the Claude adapter in override (`HYA_CLAUDE_ADAPTER_DIR`),
/// installed-adjacent, then workspace order.
fn resolve_claude_adapter_dir(
    override_dir: Option<PathBuf>,
    executable: &Path,
    workspace_root: &Path,
) -> PathBuf {
    if let Some(override_dir) = override_dir {
        return override_dir;
    }
    let installed = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("../lib/hya/claude-adapter");
    if installed.join("src/main.ts").is_file() {
        return installed;
    }
    workspace_root.join("crates/hya-plugin-claude/adapter")
}

fn find_bun_path() -> Option<PathBuf> {
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
            plugin_dir: None,
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
    fn bun_entry_without_command_resolves_to_bundled_adapter() {
        let mut config = BTreeMap::new();
        config.insert(
            "ext".to_string(),
            PluginEntry {
                kind: PluginKindWire::Bun,
                command: Vec::new(),
                enabled: true,
                timeout_ms: Some(1000),
                env: BTreeMap::new(),
                plugin_dir: None,
            },
        );

        let specs =
            super::resolve_with_bun(config, None, || Some(PathBuf::from("/usr/local/bin/bun")));

        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(spec.id, "ext");
        assert_eq!(spec.kind, PluginKindWire::Bun);
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

    /// Installed binaries resolve the adjacent production adapter before workspace source.
    #[test]
    fn bun_adapter_resolution_prefers_override_then_installed_then_workspace()
    -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("hya-bun-adapter-resolution-{}", std::process::id()));
        let executable = root.join("prefix/bin/hya");
        let installed = root.join("prefix/lib/hya/bun-adapter");
        let workspace = root.join("workspace");
        let executable_parent = executable
            .parent()
            .ok_or_else(|| std::io::Error::other("installed executable has no parent"))?;
        std::fs::create_dir_all(executable_parent)?;
        std::fs::create_dir_all(installed.join("src"))?;
        std::fs::write(installed.join("src/main.ts"), "export {}\n")?;

        assert_eq!(
            super::resolve_bun_adapter_dir(None, &executable, &workspace),
            executable_parent.join("../lib/hya/bun-adapter")
        );
        let explicit = root.join("explicit");
        assert_eq!(
            super::resolve_bun_adapter_dir(Some(explicit.clone()), &executable, &workspace),
            explicit
        );

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn bun_entry_without_bun_is_skipped() {
        let mut config = BTreeMap::new();
        config.insert(
            "ext".to_string(),
            PluginEntry {
                kind: PluginKindWire::Bun,
                command: Vec::new(),
                enabled: true,
                timeout_ms: None,
                env: BTreeMap::new(),
                plugin_dir: None,
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
                plugin_dir: None,
            },
        );
        let specs = super::resolve_with_bun(config, None, || panic!("bun must not be probed"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].command, vec!["my-plugin"]);
    }

    #[test]
    fn bun_entry_with_explicit_command_is_preserved() {
        let mut config = BTreeMap::new();
        config.insert(
            "ext".to_string(),
            PluginEntry {
                kind: PluginKindWire::Bun,
                command: vec!["custom-adapter".to_string(), "--stdio".to_string()],
                enabled: true,
                timeout_ms: None,
                env: BTreeMap::new(),
                plugin_dir: None,
            },
        );

        let specs = super::resolve_with_bun(config, None, || None);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].command, vec!["custom-adapter", "--stdio"]);
    }

    fn claude_entry(plugin_dir: Option<PathBuf>) -> PluginEntry {
        PluginEntry {
            kind: PluginKindWire::Claude,
            command: Vec::new(),
            enabled: true,
            timeout_ms: None,
            env: BTreeMap::new(),
            plugin_dir,
        }
    }

    #[test]
    fn claude_entry_with_plugin_dir_resolves_to_bundled_adapter() {
        let mut config = BTreeMap::new();
        config.insert(
            "cc".to_string(),
            claude_entry(Some(PathBuf::from("/plugins/cc-demo"))),
        );

        let specs =
            super::resolve_with_bun(config, None, || Some(PathBuf::from("/usr/local/bin/bun")));

        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(spec.id, "cc");
        assert_eq!(spec.kind, PluginKindWire::Claude);
        // [bun, run, <adapter>/src/main.ts, --plugin-dir, <plugin>, --plugin-id, <id>]
        assert_eq!(
            spec.command.first().map(String::as_str),
            Some("/usr/local/bin/bun")
        );
        assert_eq!(spec.command.get(1).map(String::as_str), Some("run"));
        assert!(
            spec.command
                .get(2)
                .is_some_and(|path| path.ends_with("src/main.ts"))
        );
        assert_eq!(
            spec.command.get(3).map(String::as_str),
            Some("--plugin-dir")
        );
        assert_eq!(
            spec.command.get(4).map(String::as_str),
            Some("/plugins/cc-demo")
        );
        assert_eq!(spec.command.get(5).map(String::as_str), Some("--plugin-id"));
        assert_eq!(spec.command.get(6).map(String::as_str), Some("cc"));
    }

    #[test]
    fn claude_entry_without_plugin_dir_is_skipped_with_a_notice() {
        let mut config = BTreeMap::new();
        config.insert("cc".to_string(), claude_entry(None));

        let specs =
            super::resolve_with_bun(config, None, || Some(PathBuf::from("/usr/local/bin/bun")));

        assert!(
            specs.is_empty(),
            "claude entries without plugin_dir must be skipped"
        );
    }

    #[test]
    fn claude_entry_without_bun_is_skipped() {
        let mut config = BTreeMap::new();
        config.insert(
            "cc".to_string(),
            claude_entry(Some(PathBuf::from("/plugins/cc-demo"))),
        );

        let specs = super::resolve_with_bun(config, None, || None);

        assert!(specs.is_empty());
    }

    #[test]
    fn claude_entry_with_explicit_command_is_preserved() {
        let mut config = BTreeMap::new();
        config.insert(
            "cc".to_string(),
            PluginEntry {
                kind: PluginKindWire::Claude,
                command: vec!["custom-claude-adapter".to_string()],
                enabled: true,
                timeout_ms: None,
                env: BTreeMap::new(),
                plugin_dir: None,
            },
        );

        let specs = super::resolve_with_bun(config, None, || None);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].command, vec!["custom-claude-adapter"]);
    }

    /// The Claude adapter resolves override, installed-adjacent, then
    /// workspace order, mirroring the Bun adapter layout.
    #[test]
    fn claude_adapter_resolution_prefers_override_then_installed_then_workspace()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "hya-claude-adapter-resolution-{}",
            std::process::id()
        ));
        let executable = root.join("prefix/bin/hya");
        let installed = root.join("prefix/lib/hya/claude-adapter");
        let workspace = root.join("workspace");
        let executable_parent = executable
            .parent()
            .ok_or_else(|| std::io::Error::other("installed executable has no parent"))?;
        std::fs::create_dir_all(executable_parent)?;
        std::fs::create_dir_all(installed.join("src"))?;
        std::fs::write(installed.join("src/main.ts"), "export {}\n")?;

        assert_eq!(
            super::resolve_claude_adapter_dir(None, &executable, &workspace),
            executable_parent.join("../lib/hya/claude-adapter")
        );
        let explicit = root.join("explicit");
        assert_eq!(
            super::resolve_claude_adapter_dir(Some(explicit.clone()), &executable, &workspace),
            explicit
        );
        // Without an installed adapter the workspace source wins.
        std::fs::remove_dir_all(&installed)?;
        assert_eq!(
            super::resolve_claude_adapter_dir(None, &executable, &workspace),
            workspace.join("crates/hya-plugin-claude/adapter")
        );

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn config_plugin_dir_flows_into_merged_specs() {
        let mut config = BTreeMap::new();
        config.insert(
            "cc".to_string(),
            claude_entry(Some(PathBuf::from("/plugins/cc-demo"))),
        );

        let specs = super::specs_from_config(config);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].plugin_dir, Some(PathBuf::from("/plugins/cc-demo")));
    }
}
