//! Resolve native, Bun extension, and Claude Code plugin specs for the plugin host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hya_plugin::config::{PluginEntry, PluginSpec};
use hya_plugin::manifest::Manifest;
use hya_plugin::messages::PluginKindWire;

/// The project plugin directory of one Project root: `<root>/.hya/plugins`.
#[must_use]
pub fn root_plugins_dir(root: &Path) -> PathBuf {
    root.join(".hya/plugins")
}

/// Resolve the config-file (`config.yaml` `plugins:`) entries into host specs.
///
/// Bun entries without a command are rewritten to the bundled Bun adapter and
/// `claude` entries without a command to the bundled Claude adapter when Bun
/// is on `PATH` (or `BUN`); otherwise they are skipped with a notice.
/// Project plugins (`<root>/.hya/plugins`) are not part of this set: they load
/// per Project, see [`crate::ProjectScopeRefresh`].
pub fn resolve(config: BTreeMap<String, PluginEntry>) -> Vec<PluginSpec> {
    resolve_js_specs(specs_from_config(config), find_bun)
}

pub(crate) struct ProjectPluginInput {
    pub(crate) path: PathBuf,
    pub(crate) digest: [u8; 32],
    pub(crate) spec: Result<Option<PluginSpec>, String>,
}

/// Include failed declarations so a bad edit can retain its previous source.
pub(crate) fn project_plugin_inputs(root: &Path) -> std::io::Result<Vec<ProjectPluginInput>> {
    use sha2::{Digest as _, Sha256};
    let entries = match std::fs::read_dir(root_plugins_dir(root)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path().join("plugin.toml")))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut inputs = Vec::new();
    for path in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                inputs.push(ProjectPluginInput {
                    path,
                    digest: Sha256::digest(error.to_string()).into(),
                    spec: Err(error.to_string()),
                });
                continue;
            }
        };
        let spec = Manifest::parse(&text)
            .map_err(|error| error.to_string())
            .and_then(|manifest| {
                if !manifest.enabled {
                    return Ok(None);
                }
                let id = manifest.id.clone();
                resolve_js_specs(
                    hya_plugin::config::merge(BTreeMap::new(), vec![manifest]),
                    find_bun,
                )
                .into_iter()
                .next()
                .map(Some)
                .ok_or_else(|| format!("plugin {id}: adapter is unavailable"))
            });
        inputs.push(ProjectPluginInput {
            path,
            digest: Sha256::digest(text).into(),
            spec,
        });
    }
    Ok(inputs)
}

pub(crate) fn bundle_sidecar_command() -> Option<Vec<String>> {
    find_bun().map(|bun| bundled_bun_adapter_command(&bun))
}

/// Bun executable used to host the TypeScript adapters: env `BUN`, then the
/// Bun a release ships at `<prefix>/lib/hya/bin/bun`, then `PATH`.
#[must_use]
pub fn find_bun() -> Option<PathBuf> {
    resolve_bun(
        non_empty_env_path("BUN"),
        std::env::current_exe().ok().as_deref(),
        std::env::var_os("PATH"),
    )
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

#[cfg(test)]
fn resolve_with_bun(
    config: BTreeMap<String, PluginEntry>,
    find_bun: impl Fn() -> Option<PathBuf>,
) -> Vec<PluginSpec> {
    resolve_js_specs(specs_from_config(config), find_bun)
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

/// The release archive's Bun beside `<prefix>/bin/hya`, also through the
/// resolved executable when `hya` is a symlink into the prefix.
fn installed_bun(executable: &Path) -> Option<PathBuf> {
    let beside = |exe: &Path| {
        exe.parent()
            .unwrap_or_else(|| Path::new("."))
            .join("../lib/hya/bin")
            .join(bun_executable_names()[0])
    };
    std::iter::once(beside(executable))
        .chain(executable.canonicalize().ok().map(|real| beside(&real)))
        .find(|candidate| candidate.is_file())
}

fn resolve_bun(
    env_bun: Option<PathBuf>,
    executable: Option<&Path>,
    path: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(bun) = env_bun {
        return Some(bun);
    }
    if let Some(bun) = executable.and_then(installed_bun) {
        return Some(bun);
    }
    for dir in std::env::split_paths(&path?) {
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

/// Fold one root's project plugin inputs into `hasher`: the plugin
/// directory listing and every `plugin.toml`'s bytes. Returns whether the
/// root has any `plugin.toml`.
pub(crate) fn project_plugins_digest(root: &Path, hasher: &mut sha2::Sha256) -> bool {
    use sha2::Digest as _;
    let entries = match std::fs::read_dir(root_plugins_dir(root)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            hasher.update(b"no-plugins\0");
            return false;
        }
        Err(error) => {
            hasher.update(error.to_string());
            return true;
        }
    };
    let mut dirs = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    dirs.sort();
    let mut any = false;
    for dir in dirs {
        hasher.update(b"plugin-entry\0");
        hasher.update(dir.as_os_str().as_encoded_bytes());
        match std::fs::read(dir.join("plugin.toml")) {
            Ok(bytes) => {
                any = true;
                hasher.update(b"manifest\0");
                hasher.update(sha2::Sha256::digest(bytes));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hasher.update(b"no-manifest\0")
            }
            Err(error) => {
                any = true;
                hasher.update(error.to_string());
            }
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use crate::test_env::StableEnvGuard;

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
    fn specs_from_config_filters_disabled_preserves_rust_and_propagates_plugin_dir() {
        let _env = StableEnvGuard::new();
        let config = BTreeMap::from([
            ("enabled".into(), entry(true, vec!["plugin-bin".into()])),
            ("disabled".into(), entry(false, vec!["ignored".into()])),
            (
                "cc".into(),
                claude_entry(Some(PathBuf::from("/plugins/cc-demo"))),
            ),
        ]);
        let specs = super::specs_from_config(config);
        assert_eq!(specs.len(), 2);
        let Some(enabled) = specs.iter().find(|s| s.id == "enabled") else {
            panic!("missing enabled plugin");
        };
        assert_eq!(enabled.command, vec!["plugin-bin"]);
        let Some(cc) = specs.iter().find(|s| s.id == "cc") else {
            panic!("missing cc plugin");
        };
        assert_eq!(cc.plugin_dir, Some(PathBuf::from("/plugins/cc-demo")));

        // Rust plugins never probe Bun.
        let rust_only = BTreeMap::from([("native".into(), entry(true, vec!["my-plugin".into()]))]);
        let specs = super::resolve_with_bun(rust_only, || panic!("bun must not be probed"));
        assert_eq!(specs[0].command, vec!["my-plugin"]);
    }

    #[test]
    fn bun_and_claude_entries_resolve_adapter_explicit_command_or_skip() {
        let _env = StableEnvGuard::new();
        let bun = || Some(PathBuf::from("/usr/local/bin/bun"));

        // Bun: bundled adapter, explicit command preserved, missing bun skipped.
        let bun_cfg = BTreeMap::from([(
            "ext".into(),
            PluginEntry {
                kind: PluginKindWire::Bun,
                command: Vec::new(),
                enabled: true,
                timeout_ms: Some(1000),
                env: BTreeMap::new(),
                plugin_dir: None,
            },
        )]);
        let specs = super::resolve_with_bun(bun_cfg.clone(), bun);
        assert_eq!(specs.len(), 1);
        assert_eq!(
            (specs[0].command[0].as_str(), specs[0].command[1].as_str()),
            ("/usr/local/bin/bun", "run")
        );
        assert!(
            specs[0]
                .command
                .last()
                .is_some_and(|p| p.ends_with("src/main.ts"))
        );
        assert_eq!(specs[0].timeout_ms, Some(1000));
        assert!(super::resolve_with_bun(bun_cfg, || None).is_empty());

        let custom_bun = BTreeMap::from([(
            "ext".into(),
            PluginEntry {
                kind: PluginKindWire::Bun,
                command: vec!["custom-adapter".into(), "--stdio".into()],
                enabled: true,
                timeout_ms: None,
                env: BTreeMap::new(),
                plugin_dir: None,
            },
        )]);
        assert_eq!(
            super::resolve_with_bun(custom_bun, || None)[0].command,
            vec!["custom-adapter", "--stdio"]
        );

        // Claude: bundled adapter with plugin_dir, skipped without plugin_dir or bun, explicit command preserved.
        let cc_cfg = BTreeMap::from([(
            "cc".into(),
            claude_entry(Some(PathBuf::from("/plugins/cc-demo"))),
        )]);
        let specs = super::resolve_with_bun(cc_cfg.clone(), bun);
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].command[3..]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["--plugin-dir", "/plugins/cc-demo", "--plugin-id", "cc"]
        );
        assert!(super::resolve_with_bun(cc_cfg, || None).is_empty());
        assert!(
            super::resolve_with_bun(BTreeMap::from([("cc".into(), claude_entry(None))]), bun)
                .is_empty()
        );

        let custom_cc = BTreeMap::from([(
            "cc".into(),
            PluginEntry {
                kind: PluginKindWire::Claude,
                command: vec!["custom-claude-adapter".into()],
                enabled: true,
                timeout_ms: None,
                env: BTreeMap::new(),
                plugin_dir: None,
            },
        )]);
        assert_eq!(
            super::resolve_with_bun(custom_cc, || None)[0].command,
            vec!["custom-claude-adapter"]
        );
    }

    #[test]
    fn bun_and_adapter_resolution_precedence() -> Result<(), Box<dyn std::error::Error>> {
        let _env = StableEnvGuard::new();
        let root = std::env::temp_dir().join(format!("hya-plugin-res-{}", std::process::id()));
        let write = |rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap_or(&root)).unwrap_or_default();
            std::fs::write(&p, "").unwrap_or_default();
            p
        };
        let exe = write("prefix/bin/hya");
        let path_bun = write("path/bun");
        let path = Some(std::ffi::OsString::from(root.join("path")));
        assert_eq!(
            super::resolve_bun(None, Some(&exe), path.clone()),
            Some(path_bun)
        );
        let installed_bun = write("prefix/lib/hya/bin/bun");
        assert_eq!(
            super::resolve_bun(None, Some(&exe), path.clone()).and_then(|p| p.canonicalize().ok()),
            installed_bun.canonicalize().ok()
        );
        assert_eq!(
            super::resolve_bun(Some(PathBuf::from("/opt/bun")), Some(&exe), path),
            Some(PathBuf::from("/opt/bun"))
        );

        // Bun & Claude adapter dir precedence: override > installed > workspace.
        let workspace = root.join("workspace");
        let exe_parent = exe.parent().ok_or_else(|| anyhow::anyhow!("exe parent"))?;
        let installed_bun_ad = root.join("prefix/lib/hya/bun-adapter");
        let installed_cc_ad = root.join("prefix/lib/hya/claude-adapter");
        for d in [&installed_bun_ad, &installed_cc_ad] {
            std::fs::create_dir_all(d.join("src"))?;
            std::fs::write(d.join("src/main.ts"), "export {}\n")?;
        }
        assert_eq!(
            super::resolve_bun_adapter_dir(None, &exe, &workspace),
            exe_parent.join("../lib/hya/bun-adapter")
        );
        assert_eq!(
            super::resolve_bun_adapter_dir(Some(root.join("exp")), &exe, &workspace),
            root.join("exp")
        );
        assert_eq!(
            super::resolve_claude_adapter_dir(None, &exe, &workspace),
            exe_parent.join("../lib/hya/claude-adapter")
        );
        assert_eq!(
            super::resolve_claude_adapter_dir(Some(root.join("exp")), &exe, &workspace),
            root.join("exp")
        );
        std::fs::remove_dir_all(&installed_cc_ad)?;
        assert_eq!(
            super::resolve_claude_adapter_dir(None, &exe, &workspace),
            workspace.join("crates/hya-plugin-claude/adapter")
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
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
}
