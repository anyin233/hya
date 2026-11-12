//! Load optional formatter configuration into a `FormatterPlane`.

use std::collections::BTreeMap;

#[cfg(test)]
use anyhow::Context as _;
use hya_tool::{FormatterConfig, FormatterEntry};
use serde::Deserialize;

#[cfg(test)]
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    formatter: RawFormatterConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawFormatterConfig {
    Bool(bool),
    Map(BTreeMap<String, RawFormatterEntry>),
}

impl Default for RawFormatterConfig {
    fn default() -> Self {
        Self::Bool(false)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct RawFormatterEntry {
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

#[cfg(test)]
fn parse_formatter_config(yaml: &str) -> anyhow::Result<FormatterConfig> {
    if yaml.trim().is_empty() {
        return Ok(FormatterConfig::Disabled);
    }
    let file: FileConfig = serde_norway::from_str(yaml).context("parse formatter config")?;
    Ok(file.formatter.into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use super::parse_formatter_config;
    use hya_tool::{FormatterConfig, FormatterEntry};

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
