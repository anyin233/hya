use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

use crate::formatter_catalog::{BuiltinSpec, CheckKind, builtins};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FormatterStatus {
    name: String,
    extensions: Vec<String>,
    enabled: bool,
}

impl FormatterStatus {
    #[must_use]
    pub fn new(name: impl Into<String>, extensions: Vec<String>, enabled: bool) -> Self {
        Self {
            name: name.into(),
            extensions,
            enabled,
        }
    }
}

#[derive(Error, Debug)]
#[error("{0}")]
pub struct FormatterError(pub String);

#[async_trait]
pub trait FormatterProvider: Send + Sync {
    async fn status(&self, workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError>;
    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        Ok(false)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FormatterConfig {
    #[default]
    Disabled,
    Builtins,
    Custom(BTreeMap<String, FormatterEntry>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormatterEntry {
    pub disabled: bool,
    pub command: Option<Vec<String>>,
    pub environment: BTreeMap<String, String>,
    pub extensions: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
struct FormatterDefinition {
    name: String,
    extensions: Vec<String>,
    command: Option<Vec<String>>,
    check: Option<CheckKind>,
}

impl FormatterDefinition {
    fn from_builtin(spec: &BuiltinSpec) -> Self {
        Self {
            name: spec.name.to_string(),
            extensions: spec
                .extensions
                .iter()
                .map(|ext| (*ext).to_string())
                .collect(),
            command: None,
            check: Some(spec.check),
        }
    }

    fn custom(name: String, entry: FormatterEntry) -> Self {
        Self {
            name,
            extensions: entry.extensions.unwrap_or_default(),
            command: entry.command,
            check: None,
        }
    }

    fn merge(&mut self, entry: FormatterEntry) {
        if let Some(extensions) = entry.extensions {
            self.extensions = extensions;
        }
        if let Some(command) = entry.command {
            self.command = Some(command);
        }
    }
}

pub struct BuiltinFormatterProvider {
    config: FormatterConfig,
}

impl BuiltinFormatterProvider {
    #[must_use]
    pub fn new(config: FormatterConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl FormatterProvider for BuiltinFormatterProvider {
    async fn status(&self, workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError> {
        let definitions = definitions_for_config(&self.config);
        Ok(definitions
            .into_iter()
            .map(|item| {
                let enabled = item.command.is_some()
                    || item
                        .check
                        .is_some_and(|check| builtin_enabled(check, workdir));
                FormatterStatus::new(item.name, item.extensions, enabled)
            })
            .collect())
    }

    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        Ok(false)
    }
}

#[derive(Clone, Default)]
pub struct FormatterPlane {
    provider: Option<Arc<dyn FormatterProvider>>,
}

impl FormatterPlane {
    #[must_use]
    pub fn new(provider: Arc<dyn FormatterProvider>) -> Self {
        Self {
            provider: Some(provider),
        }
    }

    pub async fn status(&self, workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError> {
        match &self.provider {
            Some(provider) => provider.status(workdir).await,
            None => Ok(Vec::new()),
        }
    }

    pub async fn format_file(&self, workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        match &self.provider {
            Some(provider) => provider.format_file(workdir, file).await,
            None => Ok(false),
        }
    }
}

fn definitions_for_config(config: &FormatterConfig) -> Vec<FormatterDefinition> {
    match config {
        FormatterConfig::Disabled => Vec::new(),
        FormatterConfig::Builtins => builtin_definitions(),
        FormatterConfig::Custom(entries) => custom_definitions(entries),
    }
}

fn builtin_definitions() -> Vec<FormatterDefinition> {
    builtins()
        .iter()
        .map(FormatterDefinition::from_builtin)
        .collect()
}

fn custom_definitions(entries: &BTreeMap<String, FormatterEntry>) -> Vec<FormatterDefinition> {
    let mut definitions = builtin_definitions();
    let disable_python = entries
        .get("ruff")
        .or_else(|| entries.get("uv"))
        .is_some_and(|entry| entry.disabled);
    if disable_python {
        definitions.retain(|item| item.name != "ruff" && item.name != "uv");
    }
    for (name, entry) in entries {
        if disable_python && (name == "ruff" || name == "uv") {
            continue;
        }
        if entry.disabled {
            definitions.retain(|item| item.name != *name);
            continue;
        }
        if let Some(existing) = definitions.iter_mut().find(|item| item.name == *name) {
            existing.merge(entry.clone());
        } else {
            definitions.push(FormatterDefinition::custom(name.clone(), entry.clone()));
        }
    }
    definitions
}

fn builtin_enabled(kind: CheckKind, workdir: &Path) -> bool {
    match kind {
        CheckKind::Command(command) => command_exists(command),
        CheckKind::Prettier => package_mentions(workdir, "prettier") && command_exists("prettier"),
        CheckKind::Oxfmt => false,
        CheckKind::Biome => {
            find_up(workdir, &["biome.json", "biome.jsonc"]).is_some() && command_exists("biome")
        }
        CheckKind::Clang => {
            find_up(workdir, &[".clang-format"]).is_some() && command_exists("clang-format")
        }
        CheckKind::Ruff => ruff_enabled(workdir),
        CheckKind::Uv => !ruff_enabled(workdir) && command_exists("uv"),
        CheckKind::Air => command_exists("air"),
        CheckKind::Ocamlformat => {
            find_up(workdir, &[".ocamlformat"]).is_some() && command_exists("ocamlformat")
        }
        CheckKind::Pint => file_mentions(workdir, "composer.json", "laravel/pint"),
    }
}

fn ruff_enabled(workdir: &Path) -> bool {
    if !command_exists("ruff") {
        return false;
    }
    if file_mentions(workdir, "pyproject.toml", "[tool.ruff]")
        || find_up(workdir, &["ruff.toml", ".ruff.toml"]).is_some()
    {
        return true;
    }
    ["requirements.txt", "pyproject.toml", "Pipfile"]
        .iter()
        .any(|file| file_mentions(workdir, file, "ruff"))
}

fn package_mentions(workdir: &Path, package: &str) -> bool {
    file_mentions(workdir, "package.json", package)
}

fn file_mentions(workdir: &Path, name: &str, needle: &str) -> bool {
    find_up(workdir, &[name])
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|content| content.contains(needle))
}

fn find_up(start: &Path, names: &[&str]) -> Option<std::path::PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        for name in names {
            let path = dir.join(name);
            if path.exists() {
                return Some(path);
            }
        }
        current = dir.parent();
    }
    None
}

fn command_exists(command: &str) -> bool {
    if command.contains('/') {
        return Path::new(command).exists();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(command).exists())
}
