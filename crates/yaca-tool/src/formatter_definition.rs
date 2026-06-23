use std::collections::BTreeMap;
use std::path::Path;

use crate::formatter_catalog::{BuiltinSpec, CheckKind, builtins};

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
pub(crate) struct FormatterDefinition {
    pub(crate) name: String,
    pub(crate) extensions: Vec<String>,
    pub(crate) command: Option<Vec<String>>,
    pub(crate) check: Option<CheckKind>,
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

pub(crate) fn definitions_for_config(config: &FormatterConfig) -> Vec<FormatterDefinition> {
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

pub(crate) fn builtin_enabled(kind: CheckKind, workdir: &Path) -> bool {
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
