//! Token accounting: how many tokens of the context window a transcript occupies.
//!
//! Two independent concerns live here. [`Tokenizer`] estimates a token count
//! from text alone, for routes that report no usage or report it wrongly.
//! [`TokenAccounting`] decides *which* number to believe for a given route:
//! the provider's reported prompt size, or the local estimate.

use std::sync::Arc;

use hya_proto::{Message, Part, ToolPartState};

/// Counts tokens for a text fragment.
///
/// Implementations are estimators, not encoders: they must be cheap enough to
/// run over a whole transcript on every turn.
pub trait Tokenizer: Send + Sync {
    /// Stable tokenizer name, for diagnostics and events.
    fn name(&self) -> &str;
    /// Estimated token count for `text`.
    fn count_text(&self, text: &str) -> usize;
}

/// Weights in sixteenths, fitted by relative-error least squares against
/// `o200k_base` over repository files and quantized so counting is exact
/// integer arithmetic. Refit with `scripts/tokenizer-calibration.py fit`.
mod weight {
    /// An ASCII letter run, which usually encodes as one token up to length 4.
    pub const WORD_RUN: u64 = 12;
    /// Each ASCII letter beyond the fourth in one run.
    pub const WORD_EXTRA: u64 = 2;
    /// A digit run. Digits tokenize far more densely than letters.
    pub const DIGIT_RUN: u64 = 26;
    /// Each digit beyond the first in one run.
    pub const DIGIT_EXTRA: u64 = 3;
    /// A punctuation, symbol, or multi-space run.
    pub const PUNCT_RUN: u64 = 15;
    /// Each punctuation character beyond the second in one run.
    pub const PUNCT_EXTRA: u64 = 2;
    /// A newline together with the indentation that follows it.
    pub const NEWLINE_RUN: u64 = 15;
    /// A CJK or other wide scalar value.
    pub const CJK: u64 = 13;
    /// Any remaining non-ASCII scalar value.
    pub const OTHER: u64 = 14;
    /// Common denominator for every weight above.
    pub const SCALE: u64 = 16;
}

/// Lowest scalar value treated as CJK-like for weighting purposes.
const CJK_START: u32 = 0x2E80;

/// Structure-aware token estimator calibrated against `o200k_base`.
///
/// Classifies text into run categories (words, digits, punctuation, newline and
/// indentation, CJK) and weights them. Measured on held-out repository corpora
/// it lands within 15% of the true count for over 99% of files, against roughly
/// 58–84% for a `chars / 4` heuristic, and unlike `chars / 4` it does not
/// under-count CJK text by half.
#[derive(Clone, Copy, Debug, Default)]
pub struct CalibratedTokenizer;

impl Tokenizer for CalibratedTokenizer {
    fn name(&self) -> &str {
        "calibrated"
    }

    fn count_text(&self, text: &str) -> usize {
        let mut sixteenths: u64 = 0;
        let bytes = text.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let byte = bytes[i];
            if byte.is_ascii_alphabetic() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let run = (i - start) as u64;
                sixteenths += weight::WORD_RUN + weight::WORD_EXTRA * run.saturating_sub(4);
            } else if byte.is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let run = (i - start) as u64;
                sixteenths += weight::DIGIT_RUN + weight::DIGIT_EXTRA * run.saturating_sub(1);
            } else if byte == b'\n' {
                // A newline and the indentation after it collapse into one
                // token in every BPE vocabulary we checked.
                i += 1;
                while i < bytes.len() && matches!(bytes[i], b'\n' | b'\t' | b' ') {
                    i += 1;
                }
                sixteenths += weight::NEWLINE_RUN;
            } else if byte == b' ' || byte == b'\t' {
                let start = i;
                while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
                    i += 1;
                }
                let run = (i - start) as u64;
                // A lone separator merges into the neighbouring word token.
                if run > 1 {
                    sixteenths += weight::PUNCT_RUN + weight::PUNCT_EXTRA * (run - 2);
                }
            } else if byte.is_ascii() {
                let start = i;
                while i < bytes.len()
                    && bytes[i].is_ascii()
                    && !bytes[i].is_ascii_alphanumeric()
                    && !matches!(bytes[i], b'\n' | b'\t' | b' ')
                {
                    i += 1;
                }
                let run = (i - start) as u64;
                sixteenths += weight::PUNCT_RUN + weight::PUNCT_EXTRA * run.saturating_sub(2);
            } else {
                // Non-ASCII: step by whole scalar values so a multi-byte
                // character is never split or counted twice.
                let rest = text.get(i..).unwrap_or_default();
                let Some(character) = rest.chars().next() else {
                    break;
                };
                sixteenths += if character as u32 >= CJK_START {
                    weight::CJK
                } else {
                    weight::OTHER
                };
                i += character.len_utf8();
            }
        }
        // Round rather than truncate: the weights were fitted against whole
        // totals, and truncating loses up to a token on every short fragment.
        let rounded = (sixteenths + weight::SCALE / 2) / weight::SCALE;
        usize::try_from(rounded).unwrap_or(usize::MAX)
    }
}

/// How window occupancy is derived from a transcript.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenSource {
    /// The provider's reported prompt size, plus an estimate of what was
    /// appended after the message that reported it.
    ProviderAnchored,
    /// A local tokenizer estimate over the whole transcript.
    Estimated,
}

/// A window-occupancy measurement and how it was derived.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenCount {
    /// Tokens the transcript is believed to occupy.
    pub tokens: usize,
    /// Provenance of [`Self::tokens`].
    pub source: TokenSource,
}

/// Narrowest ratio of reported-to-estimated tokens still considered plausible.
///
/// Below this the provider is reporting a different quantity (a delta, a
/// post-cache figure, or a different unit) rather than the prompt size.
const PLAUSIBLE_MIN_RATIO: f64 = 0.5;
/// Widest plausible ratio of reported-to-estimated tokens.
const PLAUSIBLE_MAX_RATIO: f64 = 2.0;

/// Decides which token count to believe for a route, and estimates the rest.
#[derive(Clone)]
pub struct TokenAccounting {
    mode: TokenAccountingMode,
    tokenizer: Arc<dyn Tokenizer>,
}

impl Default for TokenAccounting {
    fn default() -> Self {
        Self::new(TokenAccountingMode::default())
    }
}

impl std::fmt::Debug for TokenAccounting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenAccounting")
            .field("mode", &self.mode)
            .field("tokenizer", &self.tokenizer.name())
            .finish()
    }
}

impl TokenAccounting {
    /// Accounting in `mode` backed by [`CalibratedTokenizer`].
    #[must_use]
    pub fn new(mode: TokenAccountingMode) -> Self {
        Self {
            mode,
            tokenizer: Arc::new(CalibratedTokenizer),
        }
    }

    /// Accounting in `mode` backed by an explicit tokenizer.
    #[must_use]
    pub fn with_tokenizer(mode: TokenAccountingMode, tokenizer: Arc<dyn Tokenizer>) -> Self {
        Self { mode, tokenizer }
    }

    /// Configured mode.
    #[must_use]
    pub const fn mode(&self) -> TokenAccountingMode {
        self.mode
    }

    /// Backing tokenizer.
    #[must_use]
    pub fn tokenizer(&self) -> &dyn Tokenizer {
        self.tokenizer.as_ref()
    }

    /// Estimated tokens for `messages`, ignoring any reported usage.
    #[must_use]
    pub fn estimate(&self, messages: &[Message]) -> usize {
        messages
            .iter()
            .map(|message| self.estimate_message(message))
            .sum()
    }

    /// Best available window occupancy for `messages` on a route that
    /// advertises `usage_reporting`.
    ///
    /// In [`TokenAccountingMode::Auto`] the provider is believed only when it
    /// advertises usage support, actually reported a non-zero figure, and that
    /// figure is plausible against the local estimate. Otherwise the transcript
    /// is estimated outright, which is what makes a route that silently omits
    /// `usage` safe to use.
    #[must_use]
    pub fn tokens_in_use(&self, messages: &[Message], usage_reporting: bool) -> TokenCount {
        let estimated = TokenCount {
            tokens: self.estimate(messages),
            source: TokenSource::Estimated,
        };
        if self.mode == TokenAccountingMode::Estimate {
            return estimated;
        }
        let Some((index, reported)) = last_reported_usage(messages) else {
            return estimated;
        };
        let anchored = TokenCount {
            tokens: reported
                .saturating_add(self.estimate(messages.get(index + 1..).unwrap_or_default())),
            source: TokenSource::ProviderAnchored,
        };
        if self.mode == TokenAccountingMode::Provider {
            return anchored;
        }
        if !usage_reporting {
            return estimated;
        }
        // Compare like with like: the reported figure describes the prompt up to
        // and including the message that reported it, not the whole transcript.
        let prefix = self.estimate(messages.get(..=index).unwrap_or_default());
        if prefix == 0 {
            return anchored;
        }
        let ratio = reported as f64 / prefix as f64;
        if (PLAUSIBLE_MIN_RATIO..=PLAUSIBLE_MAX_RATIO).contains(&ratio) {
            anchored
        } else {
            estimated
        }
    }

    /// Estimated tokens for one message, including tool and reasoning payloads.
    fn estimate_message(&self, message: &Message) -> usize {
        match message {
            Message::User { parts, .. } | Message::Assistant { parts, .. } => {
                parts.iter().map(|part| self.estimate_part(part)).sum()
            }
            Message::System { content, .. } => self.tokenizer.count_text(content),
        }
    }

    /// Estimated tokens for one part.
    ///
    /// Tool input and output dominate a tool-heavy transcript, so they are
    /// counted rather than skipped. Media payloads are base64 blobs whose
    /// byte length, not token structure, is the useful signal.
    fn estimate_part(&self, part: &Part) -> usize {
        match part {
            Part::Text { text, .. } => self.tokenizer.count_text(text),
            Part::Reasoning {
                text,
                provider_data,
                ..
            } => {
                self.tokenizer.count_text(text)
                    + provider_data
                        .as_ref()
                        .map_or(0, |value| self.estimate_json(value))
            }
            Part::Media { data, .. } => data.len() / 4,
            Part::Tool { name, state, .. } => {
                self.tokenizer.count_text(name.as_str())
                    + match state {
                        ToolPartState::Pending { input } | ToolPartState::Running { input } => {
                            self.estimate_json(input)
                        }
                        ToolPartState::Completed { input, output, .. } => {
                            self.estimate_json(input) + self.estimate_json(output)
                        }
                        ToolPartState::Error {
                            input,
                            message,
                            value,
                            ..
                        } => {
                            self.estimate_json(input)
                                + self.tokenizer.count_text(message)
                                + value.as_ref().map_or(0, |v| self.estimate_json(v))
                        }
                    }
            }
        }
    }

    /// Estimated tokens for a JSON value as the provider will see it.
    fn estimate_json(&self, value: &serde_json::Value) -> usize {
        match value.as_str() {
            Some(text) => self.tokenizer.count_text(text),
            None => self.tokenizer.count_text(&value.to_string()),
        }
    }
}

/// Index and prompt size of the last message carrying non-zero reported usage.
///
/// Window occupancy counts `input + cache_read`: cached prompt tokens still
/// occupy the window, and providers disagree on whether `input` already
/// includes them. Summing can only over-count, which fails safe.
fn last_reported_usage(messages: &[Message]) -> Option<(usize, usize)> {
    messages.iter().enumerate().rev().find_map(|(index, m)| {
        let Message::Assistant {
            tokens: Some(usage),
            ..
        } = m
        else {
            return None;
        };
        if usage.is_zero() {
            return None;
        }
        let reported =
            usize::try_from(usage.input.saturating_add(usage.cache_read)).unwrap_or(usize::MAX);
        Some((index, reported))
    })
}
