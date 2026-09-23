//! Session-level token usage fold: billed provider usage keyed by the model
//! that served it.
//!
//! The accumulator is part of [`crate::SessionProjection`] and is folded from
//! [`crate::Event::UsageRecorded`] (one record per provider call) with a
//! fallback to legacy `MessageFinished.tokens` for logs written before
//! per-round records existed. It is pure and deterministic: replaying the same
//! log always yields the same totals, and it is reusable per session so a
//! caller can [`SessionUsage::merge`] several session logs (for example a
//! session tree) without re-reading events.
//!
//! Billed stays billed: deleting or reverting messages and compacting the
//! transcript never decrement the totals.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::message::{TokenUsage, UsagePurpose};
use crate::model::ModelRef;

/// Model key used for legacy usage that predates per-round attribution.
///
/// Folded from `MessageFinished.tokens` of messages that have no
/// `UsageRecorded` record; the serving model of those rounds is unknown.
pub const UNATTRIBUTED_MODEL: &str = "unattributed";

/// Summed usage of a set of provider calls.
///
/// Every counter follows the [`TokenUsage`] invariant. Thinking is tracked so
/// partial knowledge survives aggregation: `reasoning` counts thinking tokens
/// of calls that reported them, and `reasoning_unknown_output` counts the whole
/// `output` of calls that did not (see [`UsageTotals::output_split`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotals {
    /// Uncached prompt tokens.
    #[serde(default)]
    pub input: u64,
    /// Prompt tokens read from cache.
    #[serde(default)]
    pub cache_read: u64,
    /// Prompt tokens written to cache (cache creation).
    #[serde(default)]
    pub cache_write: u64,
    /// All generated tokens, thinking included.
    #[serde(default)]
    pub output: u64,
    /// Thinking tokens of calls that reported them (subset of `output`).
    #[serde(default)]
    pub reasoning: u64,
    /// `output` of calls whose thinking split is unknown.
    #[serde(default)]
    pub reasoning_unknown_output: u64,
    /// Provider calls folded from `UsageRecorded` records.
    #[serde(default)]
    pub rounds: u64,
    /// Legacy messages folded from `MessageFinished.tokens`.
    #[serde(default)]
    pub legacy_messages: u64,
}

/// Output tokens split into thinking and visible text, preserving what is known.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSplit {
    /// Thinking tokens of calls that reported the split.
    pub thinking: u64,
    /// Visible output of calls that reported the split (`output - thinking`).
    pub visible: u64,
    /// Output of calls whose split is unknown (thinking + visible combined).
    pub unknown: u64,
}

impl OutputSplit {
    /// Thinking tokens, or `None` when any contributing call's split is unknown.
    #[must_use]
    pub fn thinking_exact(self) -> Option<u64> {
        (self.unknown == 0).then_some(self.thinking)
    }

    /// Visible output, or `None` when any contributing call's split is unknown.
    #[must_use]
    pub fn visible_exact(self) -> Option<u64> {
        (self.unknown == 0).then_some(self.visible)
    }
}

impl UsageTotals {
    /// Fold one attributed provider call.
    pub fn add_round(&mut self, tokens: &TokenUsage) {
        self.add_usage(tokens, tokens.reasoning_unknown);
        self.rounds = self.rounds.saturating_add(1);
    }

    /// Fold one legacy `MessageFinished.tokens` sum; its thinking split is unknown.
    pub fn add_legacy(&mut self, tokens: &TokenUsage) {
        self.add_usage(tokens, true);
        self.legacy_messages = self.legacy_messages.saturating_add(1);
    }

    fn add_usage(&mut self, tokens: &TokenUsage, reasoning_unknown: bool) {
        self.input = self.input.saturating_add(tokens.input);
        self.cache_read = self.cache_read.saturating_add(tokens.cache_read);
        self.cache_write = self.cache_write.saturating_add(tokens.cache_write);
        self.output = self.output.saturating_add(tokens.output);
        if reasoning_unknown {
            self.reasoning_unknown_output =
                self.reasoning_unknown_output.saturating_add(tokens.output);
        } else {
            // Clamp so thinking can never exceed the output it belongs to.
            self.reasoning = self
                .reasoning
                .saturating_add(tokens.reasoning.min(tokens.output));
        }
    }

    /// Add another total (for aggregating models, purposes, or sessions).
    pub fn merge(&mut self, other: &UsageTotals) {
        self.input = self.input.saturating_add(other.input);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.output = self.output.saturating_add(other.output);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
        self.reasoning_unknown_output = self
            .reasoning_unknown_output
            .saturating_add(other.reasoning_unknown_output);
        self.rounds = self.rounds.saturating_add(other.rounds);
        self.legacy_messages = self.legacy_messages.saturating_add(other.legacy_messages);
    }

    /// Whole prompt: `input + cache_read + cache_write`.
    #[must_use]
    pub fn prompt(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Split `output` into thinking, visible, and unknown-split tokens.
    ///
    /// `thinking + visible + unknown == output`.
    #[must_use]
    pub fn output_split(&self) -> OutputSplit {
        let known_output = self.output.saturating_sub(self.reasoning_unknown_output);
        let thinking = self.reasoning.min(known_output);
        OutputSplit {
            thinking,
            visible: known_output - thinking,
            unknown: self.output - known_output,
        }
    }
}

/// Session-level usage fold: totals by serving model and by purpose.
///
/// Both maps sum the same provider calls, sliced two ways.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsage {
    /// Totals keyed by the model that served each call; legacy usage is keyed
    /// by [`UNATTRIBUTED_MODEL`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_model: BTreeMap<ModelRef, UsageTotals>,
    /// Totals keyed by call purpose; legacy usage counts as `turn`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_purpose: BTreeMap<UsagePurpose, UsageTotals>,
}

impl SessionUsage {
    /// True when no usage has been folded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_model.is_empty() && self.by_purpose.is_empty()
    }

    /// Fold one attributed provider call.
    pub fn record(&mut self, model: &ModelRef, purpose: UsagePurpose, tokens: &TokenUsage) {
        self.by_model
            .entry(model.clone())
            .or_default()
            .add_round(tokens);
        self.by_purpose
            .entry(purpose)
            .or_default()
            .add_round(tokens);
    }

    /// Fold one legacy `MessageFinished.tokens` sum under [`UNATTRIBUTED_MODEL`].
    pub fn record_legacy(&mut self, tokens: &TokenUsage) {
        self.by_model
            .entry(ModelRef::new(UNATTRIBUTED_MODEL))
            .or_default()
            .add_legacy(tokens);
        self.by_purpose
            .entry(UsagePurpose::Turn)
            .or_default()
            .add_legacy(tokens);
    }

    /// Grand total over every model.
    #[must_use]
    pub fn total(&self) -> UsageTotals {
        let mut total = UsageTotals::default();
        for totals in self.by_model.values() {
            total.merge(totals);
        }
        total
    }

    /// Add another session's fold into this one.
    pub fn merge(&mut self, other: &SessionUsage) {
        for (model, totals) in &other.by_model {
            self.by_model
                .entry(model.clone())
                .or_default()
                .merge(totals);
        }
        for (purpose, totals) in &other.by_purpose {
            self.by_purpose.entry(*purpose).or_default().merge(totals);
        }
    }
}

/// Attributed usage of one assistant message: the sum of its `UsageRecorded`
/// rounds and the model that served the latest one.
///
/// Kept on the message so consumers (the usage ledger, legacy fallback) can
/// tell an attributed message from a legacy one. Removed with the message on
/// `MessageDeleted`; the session totals are not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageUsage {
    /// Model that served the latest recorded round.
    pub model: ModelRef,
    /// Sum of the message's recorded rounds.
    pub tokens: TokenUsage,
    /// Number of recorded rounds.
    pub rounds: u32,
}

impl MessageUsage {
    /// Start from the message's first recorded round.
    #[must_use]
    pub fn first(model: &ModelRef, tokens: &TokenUsage) -> Self {
        Self {
            model: model.clone(),
            tokens: *tokens,
            rounds: 1,
        }
    }

    /// Add a later round; the latest round's model wins.
    pub fn add(&mut self, model: &ModelRef, tokens: &TokenUsage) {
        self.model = model.clone();
        self.tokens.add(*tokens);
        self.rounds = self.rounds.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn known(output: u64, reasoning: u64) -> TokenUsage {
        TokenUsage {
            input: 10,
            output,
            reasoning,
            cache_read: 2,
            cache_write: 1,
            reasoning_unknown: false,
        }
    }

    #[test]
    fn output_split_preserves_partial_knowledge() {
        let mut totals = UsageTotals::default();
        totals.add_round(&known(30, 12));
        totals.add_round(&TokenUsage {
            output: 50,
            reasoning_unknown: true,
            ..TokenUsage::default()
        });
        let split = totals.output_split();
        assert_eq!(
            split,
            OutputSplit {
                thinking: 12,
                visible: 18,
                unknown: 50,
            }
        );
        assert_eq!(split.thinking_exact(), None);
        assert_eq!(split.visible_exact(), None);
        assert_eq!(totals.rounds, 2);
        assert_eq!(totals.prompt(), 13);
    }

    #[test]
    fn output_split_is_exact_when_every_round_reported_thinking() {
        let mut totals = UsageTotals::default();
        totals.add_round(&known(30, 12));
        totals.add_round(&known(20, 0));
        let split = totals.output_split();
        assert_eq!(split.thinking_exact(), Some(12));
        assert_eq!(split.visible_exact(), Some(38));
    }

    #[test]
    fn thinking_is_clamped_to_output() {
        let mut totals = UsageTotals::default();
        totals.add_round(&known(5, 9));
        assert_eq!(totals.reasoning, 5);
        assert_eq!(totals.output_split().visible, 0);
    }

    #[test]
    fn legacy_usage_is_unattributed_and_unknown_split() {
        let mut usage = SessionUsage::default();
        usage.record_legacy(&known(40, 10));
        let totals = usage.by_model[&ModelRef::new(UNATTRIBUTED_MODEL)];
        assert_eq!(totals.legacy_messages, 1);
        assert_eq!(totals.rounds, 0);
        assert_eq!(totals.reasoning, 0);
        assert_eq!(totals.reasoning_unknown_output, 40);
        assert_eq!(usage.by_purpose[&UsagePurpose::Turn].output, 40);
    }

    #[test]
    fn merge_adds_sessions() {
        let mut a = SessionUsage::default();
        a.record(&ModelRef::new("m1"), UsagePurpose::Turn, &known(10, 2));
        let mut b = SessionUsage::default();
        b.record(&ModelRef::new("m1"), UsagePurpose::Title, &known(5, 0));
        b.record(&ModelRef::new("m2"), UsagePurpose::Turn, &known(7, 1));
        a.merge(&b);
        assert_eq!(a.by_model[&ModelRef::new("m1")].output, 15);
        assert_eq!(a.by_model[&ModelRef::new("m1")].rounds, 2);
        assert_eq!(a.by_model[&ModelRef::new("m2")].output, 7);
        assert_eq!(a.by_purpose[&UsagePurpose::Turn].rounds, 2);
        assert_eq!(a.total().output, 22);
    }

    #[test]
    fn usage_purpose_decodes_unknown_values_as_other() {
        let purpose: UsagePurpose = serde_json::from_str("\"evaluator\"").unwrap();
        assert_eq!(purpose, UsagePurpose::Other);
        assert_eq!(
            serde_json::to_string(&UsagePurpose::Compaction).unwrap(),
            "\"compaction\""
        );
    }
}
