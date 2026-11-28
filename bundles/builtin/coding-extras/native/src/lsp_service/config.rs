//! Optional process LSP configuration; no language server is installed implicitly.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// Single language server process definition.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Command argv starting the language server on stdio.
    #[serde(default)]
    pub command: Vec<String>,
    /// File extensions served by this language server (e.g. `.rs`).
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Workspace root marker files/directories (e.g. `Cargo.toml`, `.git`).
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// Extra environment variables for the spawned server process.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// LSP `initializationOptions` JSON payload.
    #[serde(default)]
    pub initialization_options: Value,
    /// Workspace configuration settings JSON sent via `workspace/didChangeConfiguration`.
    #[serde(default)]
    pub settings: Value,
    /// When `true`, disable this server entry.
    #[serde(default)]
    pub disabled: bool,
}

/// Top-level `lsp:` configuration selection (`bool` or named server map).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Selection {
    /// Enable auto-detection of built-in servers (`true`) or disable LSP (`false`).
    Enabled(bool),
    /// Per-server overrides and custom server definitions.
    Servers(BTreeMap<String, ServerConfig>),
}

/// Resolve effective language servers for `selection`.
///
/// # Errors
/// Returns a diagnostic message when a configured server omits a command or extensions.
pub fn resolve(selection: Option<Selection>) -> Result<BTreeMap<String, ServerConfig>, String> {
    if matches!(selection, Some(Selection::Enabled(false))) {
        return Ok(BTreeMap::new());
    }
    let mut servers = builtins();
    servers.retain(|id, config| {
        matches!(&selection, Some(Selection::Servers(overrides)) if overrides.contains_key(id))
            || executable_on_path(&config.command[0])
    });
    if let Some(Selection::Servers(overrides)) = selection {
        for (name, mut config) in overrides {
            if config.disabled {
                servers.remove(&name);
                continue;
            }
            if let Some(base) = servers.get(&name) {
                if config.command.is_empty() {
                    config.command.clone_from(&base.command);
                }
                if config.extensions.is_empty() {
                    config.extensions.clone_from(&base.extensions);
                }
                if config.root_markers.is_empty() {
                    config.root_markers.clone_from(&base.root_markers);
                }
            }
            if config.command.is_empty()
                || config.command[0].is_empty()
                || config.extensions.is_empty()
            {
                return Err(format!(
                    "LSP server {name} requires a nonempty command and extensions"
                ));
            }
            for extension in &mut config.extensions {
                if !extension.starts_with('.') {
                    extension.insert(0, '.');
                }
            }
            servers.insert(name, config);
        }
    }
    Ok(servers)
}

fn builtins() -> BTreeMap<String, ServerConfig> {
    type Definition = (
        &'static str,
        &'static [&'static str],
        &'static [&'static str],
        &'static [&'static str],
    );
    let definitions: &[Definition] = &[
        (
            "typescript",
            &["typescript-language-server", "--stdio"],
            &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"],
            &["tsconfig.json", "jsconfig.json", "package.json", ".git"],
        ),
        (
            "rust",
            &["rust-analyzer"],
            &[".rs"],
            &["Cargo.toml", ".git"],
        ),
        (
            "python",
            &["pyright-langserver", "--stdio"],
            &[".py", ".pyi"],
            &["pyproject.toml", "pyrightconfig.json", ".git"],
        ),
        (
            "bash",
            &["bash-language-server", "start"],
            &[".sh", ".bash"],
            &[".git"],
        ),
        ("go", &["gopls"], &[".go"], &["go.work", "go.mod", ".git"]),
        (
            "clangd",
            &["clangd"],
            &[".c", ".h", ".cc", ".cpp", ".hpp"],
            &["compile_commands.json", "CMakeLists.txt", ".git"],
        ),
    ];
    definitions
        .iter()
        .map(|(name, command, extensions, markers)| {
            (
                (*name).to_string(),
                ServerConfig {
                    command: command.iter().map(|s| (*s).to_string()).collect(),
                    extensions: extensions.iter().map(|s| (*s).to_string()).collect(),
                    root_markers: markers.iter().map(|s| (*s).to_string()).collect(),
                    ..ServerConfig::default()
                },
            )
        })
        .collect()
}

fn executable_on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| {
            let candidate = directory.join(program);
            if executable_file(&candidate) {
                return true;
            }
            cfg!(windows)
                && ["exe", "cmd", "bat"]
                    .iter()
                    .any(|ext| executable_file(&candidate.with_extension(ext)))
        })
    })
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

impl ServerConfig {
    pub(super) fn matches(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| {
                self.extensions
                    .iter()
                    .any(|known| known.trim_start_matches('.').eq_ignore_ascii_case(ext))
            })
    }

    pub(super) fn root(&self, file: &Path) -> PathBuf {
        let directory = if file.is_dir() {
            file
        } else {
            file.parent().unwrap_or(file)
        };
        directory
            .ancestors()
            .find(|candidate| {
                self.root_markers
                    .iter()
                    .any(|marker| candidate.join(marker).exists())
            })
            .unwrap_or(directory)
            .to_path_buf()
    }

    pub(super) fn applies_to_directory(&self, directory: &Path) -> bool {
        self.root_markers
            .iter()
            .any(|marker| marker != ".git" && directory.join(marker).exists())
            || std::fs::read_dir(directory).is_ok_and(|entries| {
                entries
                    .filter_map(Result::ok)
                    .any(|entry| self.matches(&entry.path()))
            })
    }
}
