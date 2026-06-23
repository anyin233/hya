use std::collections::BTreeMap;

use crate::formatter_catalog::{BuiltinSpec, CheckKind, builtins};
use crate::formatter_command::builtin_environment;

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
    pub(crate) environment: BTreeMap<String, String>,
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
            environment: builtin_environment(spec.check),
            check: Some(spec.check),
        }
    }

    fn custom(name: String, entry: FormatterEntry) -> Self {
        Self {
            name,
            extensions: entry.extensions.unwrap_or_default(),
            command: entry.command,
            environment: entry.environment,
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
        self.environment.extend(entry.environment);
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
