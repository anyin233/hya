//! The host's compaction settings, as `RewriteCtx::config` or per-bundle
//! `config.yml` (`HostCatalogReader::bundle_config("hya/compaction")`) carries them.

use hya_context::rewrite::ToolError;
use hya_context::tokens::TokenAccountingMode;
use serde::Deserialize;
use serde_json::Value;

const DEFAULT_KEEP_RECENT: usize = 6;

const fn default_keep_recent() -> usize {
    DEFAULT_KEEP_RECENT
}

/// The kept tail and the token-accounting mode.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub(crate) struct Config {
    /// Recent messages (and tool steps) whose tool outputs `shake` keeps.
    #[serde(default = "default_keep_recent")]
    pub(crate) keep_recent: usize,
    /// How window occupancy is measured for `ContextStatus`.
    #[serde(default)]
    pub(crate) token_accounting: TokenAccountingMode,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keep_recent: DEFAULT_KEEP_RECENT,
            token_accounting: TokenAccountingMode::default(),
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn config_reads_the_host_settings() {
        let config = Config::from_value(&serde_json::json!({
            "keep_recent": 2, "token_accounting": "estimate",
        }))
        .unwrap();
        assert_eq!(config.keep_recent, 2);
        assert_eq!(config.token_accounting, TokenAccountingMode::Estimate);

        let nested = Config::from_value(&serde_json::json!({
            "compaction": { "keep_recent": 4, "token_accounting": "provider" }
        }))
        .unwrap();
        assert_eq!(nested.keep_recent, 4);
        assert_eq!(nested.token_accounting, TokenAccountingMode::Provider);

        assert_eq!(Config::from_value(&Value::Null).unwrap(), Config::default());
    }
}
