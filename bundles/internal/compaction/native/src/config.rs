//! The host's compaction settings, as `RewriteCtx::config` carries them.

use hya_tool::ToolError;
use hya_tool::tokens::TokenAccountingMode;
use serde::Deserialize;
use serde_json::Value;

/// One of the five reduction mechanisms, by its oh-my-pi
/// `compaction.methodOrder` name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum Rung {
    /// `shake`: move stale tool-output bodies to the artifact store.
    #[serde(rename = "shake")]
    SpillToolOutputs,
    /// `remote`: the route folds its own window.
    #[serde(rename = "remote")]
    ProviderCompact,
    /// `snapcompact`: a local deterministic dense archive of the prefix.
    #[serde(rename = "snapcompact")]
    SnapCompact,
    /// `handoff`: a model-written handoff document over the whole transcript.
    #[serde(rename = "handoff")]
    Handoff,
    /// `soft`: a structured summary of the prefix.
    #[serde(rename = "soft")]
    Summarize,
}

/// Thresholds, ladder order, and token-accounting mode.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Config {
    /// Threshold when the route advertises no window.
    pub(crate) token_threshold: usize,
    /// Recent messages (and tool steps) kept out of a fold.
    pub(crate) keep_recent: usize,
    /// Share of the advertised window at which to compact; `null` when the
    /// configured value is not finite.
    pub(crate) context_fraction: Option<f32>,
    /// Tokens of the window held back for the reply.
    pub(crate) reserve_tokens: usize,
    /// Output cap of a summarizer call.
    pub(crate) summary_max_tokens: u32,
    /// The order the rungs are tried in; a permutation of all five.
    pub(crate) method_order: [Rung; 5],
    /// How window occupancy is measured.
    pub(crate) token_accounting: TokenAccountingMode,
}

/// Smallest threshold [`Config::threshold`] returns: near zero would compact
/// on every turn.
pub(crate) const MIN_RESOLVED_THRESHOLD: usize = 1_000;

impl Config {
    pub(crate) fn from_value(value: &Value) -> Result<Self, ToolError> {
        Ok(Self::deserialize(value)?)
    }

    /// Token threshold for a route with `max_context`: the tighter of a share
    /// of the window and the window minus the reply reserve. No window (`None`
    /// or `0`), or a fraction that is not in `(0, 1]`, keeps `token_threshold`.
    pub(crate) fn threshold(&self, max_context: Option<u32>) -> usize {
        let Some(window) = max_context.filter(|w| *w > 0) else {
            return self.token_threshold;
        };
        let Some(fraction) = self
            .context_fraction
            .filter(|fraction| *fraction > 0.0 && *fraction <= 1.0)
        else {
            return self.token_threshold;
        };
        // Bounded by u32::MAX, so the saturating `as` is exact enough.
        let scaled = (f64::from(window) * f64::from(fraction)) as usize;
        let window = usize::try_from(window).unwrap_or(usize::MAX);
        let reserved = window.saturating_sub(self.reserve_tokens);
        scaled.min(reserved).max(MIN_RESOLVED_THRESHOLD)
    }
}

#[cfg(test)]
pub(crate) fn test_config() -> Config {
    Config {
        token_threshold: 100_000,
        keep_recent: 6,
        context_fraction: Some(0.75),
        reserve_tokens: 16_384,
        summary_max_tokens: 4_096,
        method_order: [
            Rung::SpillToolOutputs,
            Rung::ProviderCompact,
            Rung::Summarize,
            Rung::SnapCompact,
            Rung::Handoff,
        ],
        token_accounting: TokenAccountingMode::Auto,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn config_reads_the_host_settings() {
        let config = Config::from_value(&serde_json::json!({
            "token_threshold": 1, "keep_recent": 2, "context_fraction": 0.5,
            "reserve_tokens": 3, "summary_max_tokens": 4,
            "method_order": ["remote", "snapcompact", "handoff", "shake", "soft"],
            "token_accounting": "estimate",
        }))
        .unwrap();
        assert_eq!(config.method_order[0], Rung::ProviderCompact);
        assert_eq!(config.method_order[4], Rung::Summarize);
        assert_eq!(config.token_accounting, TokenAccountingMode::Estimate);
        // A non-finite fraction arrives as null.
        let null_fraction = Config::from_value(&serde_json::json!({
            "token_threshold": 1, "keep_recent": 2, "context_fraction": f32::NAN,
            "reserve_tokens": 3, "summary_max_tokens": 4,
            "method_order": ["shake", "remote", "soft", "snapcompact", "handoff"],
            "token_accounting": "auto",
        }))
        .unwrap();
        assert_eq!(null_fraction.context_fraction, None);
    }

    #[test]
    fn threshold_scales_to_the_window_and_guards_bad_input() {
        let base = test_config();
        // No advertised window -> configured threshold stands.
        assert_eq!(base.threshold(None), 100_000);
        assert_eq!(base.threshold(Some(0)), 100_000);
        // Advertised window -> scaled by the fraction.
        assert_eq!(base.threshold(Some(200_000)), 150_000);
        assert_eq!(base.threshold(Some(1_000_000)), 750_000);
        // A tiny window must not produce a compact-every-turn threshold.
        assert_eq!(base.threshold(Some(100)), MIN_RESOLVED_THRESHOLD);
        // Nonsense fractions are not trusted.
        for bad in [Some(0.0_f32), Some(-1.0), Some(1.5), None] {
            let cfg = Config {
                context_fraction: bad,
                ..base.clone()
            };
            assert_eq!(cfg.threshold(Some(200_000)), 100_000, "fraction {bad:?}");
        }
    }

    /// The fraction alone can leave less headroom than the reply needs;
    /// `reserve_tokens` is the second bound, and the tighter one wins.
    #[test]
    fn threshold_reserves_room_for_the_response() {
        let cfg = Config {
            context_fraction: Some(0.95),
            ..test_config()
        };
        // 0.95 * 200_000 = 190_000 leaves only 10_000 for the reply.
        assert_eq!(cfg.threshold(Some(200_000)), 183_616);
        let loose = Config {
            context_fraction: Some(0.5),
            ..cfg.clone()
        };
        assert_eq!(loose.threshold(Some(200_000)), 100_000);
        let huge = Config {
            reserve_tokens: 500_000,
            ..cfg.clone()
        };
        assert_eq!(huge.threshold(Some(200_000)), MIN_RESOLVED_THRESHOLD);
        assert_eq!(cfg.threshold(None), 100_000);
    }
}
