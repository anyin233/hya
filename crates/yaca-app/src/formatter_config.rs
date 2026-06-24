use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use serde::Deserialize;
use yaca_tool::{BuiltinFormatterProvider, FormatterConfig, FormatterEntry, FormatterPlane};

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    formatter: RawFormatterConfig,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawFormatterConfig {
    Bool(bool),
    Map(BTreeMap<String, RawFormatterEntry>),
}

impl Default for RawFormatterConfig {
    fn default() -> Self {
        Self::Bool(false)
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawFormatterEntry {
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    command: Option<Vec<String>>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default)]
    extensions: Option<Vec<String>>,
}

impl From<RawFormatterConfig> for FormatterConfig {
    fn from(value: RawFormatterConfig) -> Self {
        match value {
            RawFormatterConfig::Bool(false) => Self::Disabled,
            RawFormatterConfig::Bool(true) => Self::Builtins,
            RawFormatterConfig::Map(entries) => Self::Custom(
                entries
                    .into_iter()
                    .map(|(name, entry)| (name, entry.into()))
                    .collect(),
            ),
        }
    }
}

impl From<RawFormatterEntry> for FormatterEntry {
    fn from(value: RawFormatterEntry) -> Self {
        Self {
            disabled: value.disabled,
            command: value.command,
            environment: value.environment,
            extensions: value.extensions,
        }
    }
}

pub fn load_plane() -> FormatterPlane {
    FormatterPlane::new(Arc::new(BuiltinFormatterProvider::new(load_config())))
}

fn load_config() -> FormatterConfig {
    let Some(path) = config_path() else {
        return FormatterConfig::Disabled;
    };
    let Ok(yaml) = std::fs::read_to_string(&path) else {
        return FormatterConfig::Disabled;
    };
    match parse_formatter_config(&yaml) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("yaca: formatter config error ({error:#}); formatter status disabled");
            FormatterConfig::Disabled
        }
    }
}

fn parse_formatter_config(yaml: &str) -> anyhow::Result<FormatterConfig> {
    if yaml.trim().is_empty() {
        return Ok(FormatterConfig::Disabled);
    }
    let file: FileConfig = serde_norway::from_str(yaml).context("parse formatter config")?;
    Ok(file.formatter.into())
}

fn config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(dir).join("yaca/config.yaml");
        if path.exists() {
            return Some(path);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".config/yaca/config.yaml");
    path.exists().then_some(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use super::parse_formatter_config;
    use yaca_tool::{FormatterConfig, FormatterEntry};

    #[test]
    fn parses_disabled_formatter_config() {
        let parsed = parse_formatter_config("providers: {}\n").unwrap();

        assert_eq!(parsed, FormatterConfig::Disabled);
    }

    #[test]
    fn parses_builtin_formatter_config() {
        let parsed = parse_formatter_config("formatter: true\n").unwrap();

        assert_eq!(parsed, FormatterConfig::Builtins);
    }

    #[test]
    fn parses_custom_formatter_config() {
        let parsed = parse_formatter_config(
            r#"
formatter:
  treefmt:
    command: [treefmt, "$FILE"]
    extensions: [.nix]
  gofmt:
    disabled: true
"#,
        )
        .unwrap();

        let mut expected = BTreeMap::new();
        expected.insert(
            "treefmt".to_string(),
            FormatterEntry {
                command: Some(vec!["treefmt".to_string(), "$FILE".to_string()]),
                extensions: Some(vec![".nix".to_string()]),
                ..FormatterEntry::default()
            },
        );
        expected.insert(
            "gofmt".to_string(),
            FormatterEntry {
                disabled: true,
                ..FormatterEntry::default()
            },
        );
        assert_eq!(parsed, FormatterConfig::Custom(expected));
    }
}
