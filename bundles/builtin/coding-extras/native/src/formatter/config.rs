//! Load optional formatter configuration into a [`hya_tool::FormatterConfig`].

use std::collections::BTreeMap;

use hya_tool::{FormatterConfig, FormatterEntry};
use serde::Deserialize;

#[cfg(test)]
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    formatter: RawFormatterConfig,
}

/// Raw YAML/JSON representation of the `formatter:` block in `hya/coding-extras`
/// `config.yml`.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum RawFormatterConfig {
    /// Enable all built-in formatters (`true`) or disable all (`false`).
    Bool(bool),
    /// Per-formatter overrides merged over built-in formatters.
    Map(BTreeMap<String, RawFormatterEntry>),
}

impl Default for RawFormatterConfig {
    fn default() -> Self {
        Self::Bool(false)
    }
}

/// One named formatter entry in [`RawFormatterConfig::Map`].
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawFormatterEntry {
    /// When `true`, disable this formatter.
    #[serde(default)]
    pub disabled: bool,
    /// Explicit command argv with optional `$FILE` placeholder.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Extra environment variables passed to the formatter command.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// File extensions owned by this formatter (with leading dots).
    #[serde(default)]
    pub extensions: Option<Vec<String>>,
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
fn parse_formatter_config(yaml: &str) -> Result<FormatterConfig, String> {
    if yaml.trim().is_empty() {
        return Ok(FormatterConfig::Disabled);
    }
    let file: FileConfig =
        serde_norway::from_str(yaml).map_err(|error| format!("parse formatter config: {error}"))?;
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
