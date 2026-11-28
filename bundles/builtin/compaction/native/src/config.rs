//! The bundle's settings: the `compaction:` block of its `config.yml`
//! (`HostCatalogReader::bundle_config("hya/compaction")`), then
//! `HYA_COMPACTION_KEEP_RECENT`.

use hya_context::rewrite::ToolError;
use serde::Deserialize;
use serde_json::Value;

/// Environment variable that overrides `keep_recent`.
pub(crate) const KEEP_RECENT_ENV: &str = "HYA_COMPACTION_KEEP_RECENT";

const DEFAULT_KEEP_RECENT: usize = 6;

const fn default_keep_recent() -> usize {
    DEFAULT_KEEP_RECENT
}

/// The kept tail.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub(crate) struct Config {
    /// Recent messages (and tool steps) whose tool outputs `shake` keeps.
    #[serde(default = "default_keep_recent")]
    pub(crate) keep_recent: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keep_recent: DEFAULT_KEEP_RECENT,
        }
    }
}

impl Config {
    pub(crate) fn from_value(value: &Value) -> Result<Self, ToolError> {
        if value.is_null() {
            return Ok(Self::default());
        }
        if let Some(nested) = value.get("compaction")
            && !nested.is_null()
        {
            return Ok(Self::deserialize(nested)?);
        }
        Ok(Self::deserialize(value)?)
    }

    /// Apply the `HYA_COMPACTION_KEEP_RECENT` value; an unparseable one keeps
    /// the file's setting.
    pub(crate) fn apply_env(mut self, keep_recent: Option<&str>) -> Self {
        if let Some(parsed) = keep_recent.and_then(|value| value.trim().parse().ok()) {
            self.keep_recent = parsed;
        }
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn config_reads_the_bundle_file_then_the_env_override() {
        let config = Config::from_value(&serde_json::json!({ "keep_recent": 2 })).unwrap();
        assert_eq!(config.keep_recent, 2);

        let nested = Config::from_value(&serde_json::json!({
            "compaction": { "keep_recent": 4, "summary_max_tokens": 8192 }
        }))
        .unwrap();
        assert_eq!(nested.keep_recent, 4);

        assert_eq!(Config::from_value(&Value::Null).unwrap(), Config::default());
        assert!(Config::from_value(&serde_json::json!({ "keep_recent": "many" })).is_err());

        assert_eq!(nested.apply_env(Some(" 9 ")).keep_recent, 9);
        assert_eq!(nested.apply_env(Some("banana")).keep_recent, 4);
        assert_eq!(nested.apply_env(None).keep_recent, 4);
    }
}
