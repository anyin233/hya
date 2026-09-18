//! Wire types for token accounting, shared by events, projections, and clients.

use serde::{Deserialize, Serialize};

/// How window occupancy is derived from a transcript.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenAccountingMode {
    /// Trust reported usage until the route proves unreliable, then estimate.
    #[default]
    Auto,
    /// Always trust reported usage, even when it is absent or implausible.
    Provider,
    /// Always estimate locally and ignore reported usage.
    Estimate,
}

impl TokenAccountingMode {
    /// Parse a configuration or environment value.
    ///
    /// Unrecognized values return `None` so callers can keep their default
    /// rather than silently adopting a mode the user did not ask for.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "provider" => Some(Self::Provider),
            "estimate" => Some(Self::Estimate),
            _ => None,
        }
    }

    /// Stable lowercase name, the inverse of [`Self::parse`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Provider => "provider",
            Self::Estimate => "estimate",
        }
    }
}

/// Where a token count came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenSource {
    /// The provider's reported prompt size, plus an estimate of what was
    /// appended after the message that reported it.
    Provider,
    /// A local tokenizer estimate over the whole transcript.
    Estimate,
}
