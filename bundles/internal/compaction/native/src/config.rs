//! The host's compaction settings, as `RewriteCtx::config` carries them.

use hya_tool::ToolError;
use hya_tool::tokens::TokenAccountingMode;
use serde::Deserialize;
use serde_json::Value;

/// The kept tail and the token-accounting mode.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Config {
    /// Recent messages (and tool steps) whose tool outputs `shake` keeps.
    pub(crate) keep_recent: usize,
    /// How window occupancy is measured for `ContextStatus`.
    pub(crate) token_accounting: TokenAccountingMode,
}

impl Config {
    pub(crate) fn from_value(value: &Value) -> Result<Self, ToolError> {
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
    }
}
