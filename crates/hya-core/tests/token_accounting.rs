//! Token accounting: calibrated estimation versus provider-reported usage.
//!
//! Ground-truth counts are `o200k_base` encodings; regenerate them with
//! `python3 scripts/tokenizer-calibration.py fixtures`. They pin the
//! estimator's accuracy contract so a regression in the weight table, or a
//! divergence between the Rust counter and the fitting script, cannot pass
//! silently.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_core::{CalibratedTokenizer, TokenAccounting, TokenAccountingMode, TokenSource, Tokenizer};
use hya_proto::{
    AgentName, FinishReason, Message, MessageId, ModelRef, Part, PartId, TokenUsage, ToolCallId,
    ToolName, ToolPartState,
};

/// `(name, text, o200k_base token count)`.
const FIXTURES: &[(&str, &str, usize)] = &[
    (
        "rust_code",
        "pub fn resolved_threshold(cfg: &CompactionConfig, max_context: Option<u32>) -> usize {\n    let Some(window) = max_context.filter(|w| *w > 0) else {\n        return cfg.token_threshold;\n    };\n    if !(cfg.context_fraction > 0.0 && cfg.context_fraction <= 1.0) {\n        return cfg.token_threshold;\n    }\n    let scaled = f64::from(window) * f64::from(cfg.context_fraction);\n    (scaled as usize).max(MIN_RESOLVED_THRESHOLD)\n}\n",
        113,
    ),
    (
        "json_blob",
        "{\n  \"session\": \"01JQ7X8Z9ABCDEFGHJKMNPQRST\",\n  \"messages\": [\n    {\n      \"role\": \"user\",\n      \"tokens\": {\n        \"input\": 18422,\n        \"output\": 512,\n        \"cache_read\": 16384\n      }\n    },\n    {\n      \"role\": \"assistant\",\n      \"finish\": \"tool_calls\",\n      \"tools\": [\n        \"read\",\n        \"grep\",\n        \"bash\"\n      ]\n    }\n  ],\n  \"paths\": [\n    \"crates/hya-core/src/compaction.rs\",\n    \"crates/hya-tool/src/handle/artifact.rs\"\n  ]\n}",
        143,
    ),
    (
        "markdown_prose",
        "## Compaction ladder\n\nThe turn loop walks a fixed escalation order, cheapest and most recoverable\nfirst, and stops at the first rung that brings the transcript under the\ntrigger. Spilling tool output is lossless because the body moves to an\n`artifact://` handle and only the pointer stays in the transcript.\n",
        65,
    ),
    (
        "chinese",
        "本次我们开始开发上下文管理功能，首先阅读当前仓库，分析系统如何管理上下文。压缩工具的实现完全参考分级设计进行，使用多个工具分级依次尝试，从而实现当高效压缩工具有效时直接调用高效的压缩工具。",
        64,
    ),
    (
        "mixed_cjk",
        "接下来我们需要为 hya 引入类似 omp 的 artifact:// 等路径工具，即引入包括 skill:// 在内的这类基于 url 的索引工具，替换现有的索引工具，为 hya 带来统一化的外部工具调用体验。",
        59,
    ),
    (
        "indented_code",
        "impl Tool for ShellTool {\n    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {\n        let args: ShellArgs = serde_json::from_value(input)?;\n        let artifacts = ctx.handles.artifacts();\n        let mut sink = OutputSink::new(artifacts.clone());\n        for chunk in stream {\n            sink.push(&chunk)?;\n        }\n        Ok(sink.finish()?)\n    }\n}\n",
        93,
    ),
    (
        "numeric_table",
        "threshold=150000 reserve=16384 window=200000 fraction=0.75\ninput=18422 output=512 cache_read=16384 cache_write=0\n1234567890 987654321 42 3.14159265358979 0xDEADBEEF\n",
        60,
    ),
    (
        "log_output",
        "error[E0433]: failed to resolve: use of undeclared crate or module `handle`\n  --> crates/hya-tool/src/shell.rs:354:41\n   |\n354 |         let artifact_root = normalize(&absolutize(&ctx.workdir));\n   |                                         ^^^^^^^^^^ not found in this scope\nwarning: unused import: `std::sync::Arc`\n",
        78,
    ),
];

/// Relative error of `predicted` against `truth`, as a signed fraction.
fn relative_error(predicted: usize, truth: usize) -> f64 {
    (predicted as f64 - truth as f64) / truth as f64
}

#[test]
fn calibrated_tokenizer_stays_within_twenty_percent_on_every_fixture() {
    let tokenizer = CalibratedTokenizer;
    for (name, text, truth) in FIXTURES {
        let estimate = tokenizer.count_text(text);
        let error = relative_error(estimate, *truth);
        assert!(
            error.abs() <= 0.20,
            "{name}: estimate {estimate} vs truth {truth} is {:+.1}% off",
            error * 100.0
        );
    }
}

/// The baseline being replaced is `hya_core::compaction::estimate_tokens`, which
/// is `str::len() / 4` — that is *bytes* over four, not characters.
fn bytes_div_four(text: &str) -> usize {
    text.len() / 4
}

#[test]
fn calibrated_tokenizer_is_tighter_than_bytes_div_four() {
    let tokenizer = CalibratedTokenizer;
    let mut calibrated_total = 0.0f64;
    let mut baseline_total = 0.0f64;
    let mut calibrated_worst = 0.0f64;
    let mut baseline_worst = 0.0f64;
    for (_, text, truth) in FIXTURES {
        let calibrated = relative_error(tokenizer.count_text(text), *truth).abs();
        let baseline = relative_error(bytes_div_four(text), *truth).abs();
        calibrated_total += calibrated;
        baseline_total += baseline;
        calibrated_worst = calibrated_worst.max(calibrated);
        baseline_worst = baseline_worst.max(baseline);
    }
    assert!(
        calibrated_total < baseline_total,
        "calibrated total error {calibrated_total:.3} must beat bytes/4 total {baseline_total:.3}"
    );
    assert!(
        calibrated_worst < baseline_worst,
        "calibrated worst error {calibrated_worst:.3} must beat bytes/4 worst {baseline_worst:.3}"
    );
}

/// The failure mode that matters. `bytes / 4` under-counts structured payloads
/// — JSON tool output and numeric tables — by 22% to 32%, and under-counting is
/// the direction that overflows the window. Over-counting only compacts early.
#[test]
fn structured_payloads_are_not_under_counted() {
    let tokenizer = CalibratedTokenizer;
    for (name, text, truth) in FIXTURES
        .iter()
        .filter(|(name, ..)| *name == "json_blob" || *name == "numeric_table")
    {
        let baseline = relative_error(bytes_div_four(text), *truth);
        assert!(
            baseline < -0.20,
            "{name}: fixture must exercise the baseline's under-count, got {:+.1}%",
            baseline * 100.0
        );
        let error = relative_error(tokenizer.count_text(text), *truth);
        assert!(
            error.abs() <= 0.10,
            "{name}: calibrated estimate must stay tight, got {:+.1}%",
            error * 100.0
        );
    }
}

/// No fixture may be under-counted enough to matter, in either encoding family.
#[test]
fn no_fixture_is_materially_under_counted() {
    let tokenizer = CalibratedTokenizer;
    for (name, text, truth) in FIXTURES {
        let error = relative_error(tokenizer.count_text(text), *truth);
        assert!(
            error > -0.15,
            "{name}: under-counting risks overflow, got {:+.1}%",
            error * 100.0
        );
    }
}

#[test]
fn empty_and_ascii_edge_cases_are_stable() {
    let tokenizer = CalibratedTokenizer;
    assert_eq!(tokenizer.count_text(""), 0);
    assert_eq!(tokenizer.name(), "calibrated");
    assert!(
        tokenizer.count_text("a") >= 1,
        "a single word costs a token"
    );
}

/// A transcript whose last assistant message reports `usage`.
fn transcript(reported: Option<TokenUsage>) -> Vec<Message> {
    vec![
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "x".repeat(4_000),
            }],
        },
        Message::Assistant {
            id: MessageId::new(),
            agent: AgentName::new("build"),
            model: ModelRef::new("test/model"),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "ok".to_string(),
            }],
            finish: Some(FinishReason::Stop),
            tokens: reported,
        },
    ]
}

#[test]
fn auto_mode_estimates_when_the_route_reports_no_usage() {
    let accounting = TokenAccounting::new(TokenAccountingMode::Auto);
    let counted = accounting.tokens_in_use(&transcript(None), true);
    assert_eq!(counted.source, TokenSource::Estimated);
    assert!(counted.tokens > 0, "an estimate must still be produced");
}

#[test]
fn auto_mode_estimates_when_the_route_disclaims_usage_support() {
    let usage = TokenUsage {
        input: 1_000,
        output: 10,
        ..TokenUsage::default()
    };
    let accounting = TokenAccounting::new(TokenAccountingMode::Auto);
    let counted = accounting.tokens_in_use(&transcript(Some(usage)), false);
    assert_eq!(counted.source, TokenSource::Estimated);
}

/// A route reporting a token count wildly inconsistent with the prompt it was
/// given is counting something else — a delta, or a different unit. Believing
/// it would silently disable compaction.
#[test]
fn auto_mode_rejects_implausible_reported_usage() {
    let accounting = TokenAccounting::new(TokenAccountingMode::Auto);
    let messages = transcript(Some(TokenUsage {
        input: 3,
        output: 1,
        ..TokenUsage::default()
    }));
    let counted = accounting.tokens_in_use(&messages, true);
    assert_eq!(
        counted.source,
        TokenSource::Estimated,
        "a 3-token claim for a 4000-byte prompt is not believable"
    );
    assert!(counted.tokens > 100, "got {}", counted.tokens);
}

#[test]
fn auto_mode_anchors_on_plausible_reported_usage() {
    let accounting = TokenAccounting::new(TokenAccountingMode::Auto);
    let estimated = accounting.estimate(&transcript(None));
    let messages = transcript(Some(TokenUsage {
        input: estimated as u64,
        output: 10,
        ..TokenUsage::default()
    }));
    let counted = accounting.tokens_in_use(&messages, true);
    assert_eq!(counted.source, TokenSource::ProviderAnchored);
}

#[test]
fn provider_mode_trusts_even_implausible_usage() {
    let accounting = TokenAccounting::new(TokenAccountingMode::Provider);
    let messages = transcript(Some(TokenUsage {
        input: 3,
        output: 1,
        ..TokenUsage::default()
    }));
    let counted = accounting.tokens_in_use(&messages, true);
    assert_eq!(counted.source, TokenSource::ProviderAnchored);
    assert_eq!(counted.tokens, 3, "nothing was appended after the report");
}

#[test]
fn estimate_mode_ignores_reported_usage() {
    let accounting = TokenAccounting::new(TokenAccountingMode::Estimate);
    let messages = transcript(Some(TokenUsage {
        input: 999_999,
        output: 1,
        ..TokenUsage::default()
    }));
    let counted = accounting.tokens_in_use(&messages, true);
    assert_eq!(counted.source, TokenSource::Estimated);
    assert!(counted.tokens < 999_999);
}

/// Cached prompt tokens still occupy the window, so they are counted.
#[test]
fn cache_read_tokens_count_against_the_window() {
    let accounting = TokenAccounting::new(TokenAccountingMode::Provider);
    let messages = transcript(Some(TokenUsage {
        input: 500,
        cache_read: 1_500,
        ..TokenUsage::default()
    }));
    assert_eq!(accounting.tokens_in_use(&messages, true).tokens, 2_000);
}

#[test]
fn accounting_mode_round_trips_through_parse() {
    for mode in [
        TokenAccountingMode::Auto,
        TokenAccountingMode::Provider,
        TokenAccountingMode::Estimate,
    ] {
        assert_eq!(TokenAccountingMode::parse(mode.as_str()), Some(mode));
    }
    assert_eq!(
        TokenAccountingMode::parse("  AUTO "),
        Some(TokenAccountingMode::Auto)
    );
    assert_eq!(TokenAccountingMode::parse("nonsense"), None);
}

/// Tool input and output dominate a tool-heavy transcript; skipping them is how
/// the old estimator let explore loops run past the window.
#[test]
fn tool_payloads_are_counted() {
    let accounting = TokenAccounting::new(TokenAccountingMode::Estimate);
    let bare = vec![Message::Assistant {
        id: MessageId::new(),
        agent: AgentName::new("build"),
        model: ModelRef::new("test/model"),
        parts: vec![Part::Tool {
            id: PartId::new(),
            call_id: ToolCallId::new(),
            name: ToolName::new("grep"),
            state: ToolPartState::Completed {
                input: serde_json::json!({ "pattern": "fn main" }),
                output: serde_json::Value::String("hit\n".repeat(500)),
                time_ms: 4,
            },
        }],
        finish: Some(FinishReason::Stop),
        tokens: None,
    }];
    assert!(
        accounting.estimate(&bare) > 400,
        "tool output must be counted, got {}",
        accounting.estimate(&bare)
    );
}
