//! The `snapcompact` rung's archive: a local, deterministic, model-free fold.

use std::fmt::Write as _;

use hya_proto::{Message, Part, ToolPartState};

use crate::evict::value_text;

/// Max rendered characters of one tool result in a snapcompact archive.
///
/// oh-my-pi's `toolResultMaxChars` default: the ends of an output carry its
/// verdict and final state, so they are kept and the middle is dropped.
const SNAP_TOOL_RESULT_MAX_CHARS: usize = 2_000;
/// Share of [`SNAP_TOOL_RESULT_MAX_CHARS`] kept from the head of a payload.
const SNAP_HEAD_RATIO: f32 = 0.6;
/// Max rendered characters of one string value inside a tool-call input.
///
/// oh-my-pi's `toolArgMaxChars` default.
const SNAP_TOOL_ARG_MAX_CHARS: usize = 500;
/// Max rendered characters of a tool call's whole input object.
///
/// oh-my-pi's `toolCallMaxChars` default.
const SNAP_TOOL_CALL_MAX_CHARS: usize = 2_000;

/// Largest char-boundary-safe prefix of at most `max` bytes.
fn floor_boundary(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Smallest char-boundary-safe suffix start of at least `min` bytes from the end.
fn ceil_boundary(text: &str, min: usize) -> usize {
    let mut start = text.len().saturating_sub(min);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// Keep the head and tail of `text` under a byte budget, saying how much was
/// dropped. Errors and final state live at the ends of a tool output, not its
/// middle, so a head+tail split preserves more usable signal than a prefix cut.
fn head_tail_budgeted(text: &str, max: usize, head_ratio: f32) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let head = ((max as f32) * head_ratio) as usize;
    let head = head.clamp(1, max.saturating_sub(1));
    let tail = max - head;
    let head_end = floor_boundary(text, head);
    let tail_start = ceil_boundary(text, tail);
    let omitted = text.len() - head_end - (text.len() - tail_start);
    format!(
        "{}…[{} chars omitted]…{}",
        &text[..head_end],
        omitted,
        &text[tail_start..]
    )
}

/// Collapse runs of blanks to one blank line and runs of spaces/tabs to one.
///
/// An archive trades formatting for density: the words and structure survive,
/// the indentation that carried them does not.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = false;
    for line in text.lines() {
        let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            blank_run = true;
            continue;
        }
        if blank_run && !out.is_empty() {
            out.push('\n');
        }
        blank_run = false;
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&collapsed);
    }
    out
}

/// Cap every string leaf of a JSON value at [`SNAP_TOOL_ARG_MAX_CHARS`].
fn cap_json_strings(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) if s.len() > SNAP_TOOL_ARG_MAX_CHARS => {
            let end = floor_boundary(s, SNAP_TOOL_ARG_MAX_CHARS);
            serde_json::Value::String(format!("{}…", &s[..end]))
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(cap_json_strings).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cap_json_strings(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Render a tool-call input under the per-value and per-call budgets.
fn budget_tool_input(input: &serde_json::Value) -> String {
    let rendered = cap_json_strings(input).to_string();
    head_tail_budgeted(&rendered, SNAP_TOOL_CALL_MAX_CHARS, SNAP_HEAD_RATIO)
}

/// Serialize a transcript into the snapcompact archive: a local, deterministic,
/// model-free fold of the discarded history.
///
/// This is hya's `snapcompact` mechanism. oh-my-pi renders the same serialized
/// archive onto bitmap image frames for vision-capable models; hya commits the
/// dense text itself, which every route can read, with omp's serialization
/// budgets (head+tail tool results, capped call arguments, collapsed
/// whitespace) so one huge payload cannot crowd out the turns around it.
pub(crate) fn snapcompact_archive(messages: &[Message]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "[snapcompact archive of {} messages]", messages.len());
    for message in messages {
        match message {
            Message::User { parts, .. } => {
                let _ = writeln!(out, "[user]");
                snap_parts(&mut out, parts);
            }
            Message::Assistant { parts, .. } => {
                let _ = writeln!(out, "[assistant]");
                snap_parts(&mut out, parts);
            }
            Message::System { content, .. } => {
                let _ = writeln!(
                    out,
                    "[system] {}",
                    head_tail_budgeted(
                        &collapse_whitespace(content),
                        SNAP_TOOL_RESULT_MAX_CHARS,
                        SNAP_HEAD_RATIO
                    )
                );
            }
        }
    }
    out
}

fn snap_parts(out: &mut String, parts: &[Part]) {
    for part in parts {
        match part {
            Part::Text { text, .. } if !text.trim().is_empty() => {
                let _ = writeln!(
                    out,
                    "{}",
                    head_tail_budgeted(
                        &collapse_whitespace(text),
                        SNAP_TOOL_RESULT_MAX_CHARS,
                        SNAP_HEAD_RATIO
                    )
                );
            }
            Part::Reasoning { text, .. } if !text.trim().is_empty() => {
                let _ = writeln!(
                    out,
                    "[reasoning] {}",
                    head_tail_budgeted(
                        &collapse_whitespace(text),
                        SNAP_TOOL_RESULT_MAX_CHARS,
                        SNAP_HEAD_RATIO
                    )
                );
            }
            Part::Media {
                media_type,
                data,
                filename,
                ..
            } => {
                let name = filename.as_deref().unwrap_or("attachment");
                let _ = writeln!(out, "[media {name} ({media_type}), {} bytes]", data.len());
            }
            Part::Tool { name, state, .. } => {
                let tool = name.as_str();
                match state {
                    ToolPartState::Pending { input } | ToolPartState::Running { input } => {
                        let _ = writeln!(
                            out,
                            "[tool {tool}] input: {} (did not finish)",
                            budget_tool_input(input)
                        );
                    }
                    ToolPartState::Completed { input, output, .. } => {
                        let _ = writeln!(out, "[tool {tool}] input: {}", budget_tool_input(input));
                        let _ = writeln!(
                            out,
                            "[tool {tool}] output: {}",
                            head_tail_budgeted(
                                &collapse_whitespace(&value_text(output)),
                                SNAP_TOOL_RESULT_MAX_CHARS,
                                SNAP_HEAD_RATIO
                            )
                        );
                    }
                    ToolPartState::Error { input, message, .. } => {
                        let _ = writeln!(out, "[tool {tool}] input: {}", budget_tool_input(input));
                        let _ = writeln!(
                            out,
                            "[tool {tool}] failed: {}",
                            head_tail_budgeted(
                                &collapse_whitespace(message),
                                SNAP_TOOL_RESULT_MAX_CHARS,
                                SNAP_HEAD_RATIO
                            )
                        );
                    }
                }
            }
            Part::Text { .. } | Part::Reasoning { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fold::tests::user;
    use hya_proto::{MessageId, ModelRef, PartId};

    fn assistant_with_tool(output: &str) -> Message {
        Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts: vec![Part::Tool {
                id: PartId::new(),
                call_id: hya_proto::ToolCallId::new(),
                name: hya_proto::ToolName::new("find"),
                state: ToolPartState::Completed {
                    input: serde_json::json!({"pattern": "*.rs"}),
                    output: serde_json::Value::String(output.to_string()),
                    time_ms: 7,
                },
            }],
            finish: None,
            tokens: None,
            last_round: None,
        }
    }

    /// snapcompact is the no-LLM rung: a deterministic dense archive under the
    /// oh-my-pi budgets. A long tool output keeps its head and tail around an
    /// explicit omission marker, because errors and final state live at the
    /// ends of an output, not its middle.
    #[test]
    fn snapcompact_archive_keeps_head_and_tail_and_collapses_whitespace() {
        let body = format!(
            "{}{}{}",
            "H".repeat(2_000),
            "M".repeat(5_000),
            "T".repeat(2_000)
        );
        let mut msgs = vec![assistant_with_tool(&body)];
        msgs.push(user("a\n\n\n\nb   c"));
        let archive = snapcompact_archive(&msgs);
        assert!(
            archive.contains(&"H".repeat(100)),
            "head must survive: {archive}"
        );
        assert!(
            archive.contains(&"T".repeat(100)),
            "tail must survive: {archive}"
        );
        assert!(
            !archive.contains(&"M".repeat(50)),
            "the middle is omitted, not archived: {archive}"
        );
        assert!(archive.contains("chars omitted"), "{archive}");
        // Whitespace-collapsed: runs of blank lines and trailing space fold.
        assert!(
            archive.contains("a\n\nb c"),
            "whitespace is collapsed: {archive}"
        );
        assert_eq!(
            archive,
            snapcompact_archive(&msgs),
            "archive is deterministic"
        );
    }

    /// Tool-call arguments are capped per value and per call, matching omp's
    /// `SerializeOptions`, so one huge path glob cannot fill the archive.
    #[test]
    fn snapcompact_archive_budgets_tool_arguments() {
        let long_value = "v".repeat(1_000);
        let msgs = vec![Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: ModelRef::new("m"),
            parts: vec![Part::Tool {
                id: PartId::new(),
                call_id: hya_proto::ToolCallId::new(),
                name: hya_proto::ToolName::new("bash"),
                state: ToolPartState::Completed {
                    input: serde_json::json!({ "command": long_value }),
                    output: serde_json::Value::String("ok".to_string()),
                    time_ms: 1,
                },
            }],
            finish: None,
            tokens: None,
            last_round: None,
        }];
        let archive = snapcompact_archive(&msgs);
        assert!(
            !archive.contains(&"v".repeat(600)),
            "a single argument value is capped: {archive}"
        );
    }
}
