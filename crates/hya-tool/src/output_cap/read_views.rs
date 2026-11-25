use serde_json::json;
use serde_json::{Map, Value};

use super::json_budget::bound_string;
use super::{MAX_CODING_OUTPUT_BYTES, TruncationFacts, TruncationKind, serialized_string_len};

/// Synchronize Read content, display metadata, offsets, and wrapper notices.
pub(super) fn synchronize_read_views(
    envelope: &mut Map<String, Value>,
    facts: &mut TruncationFacts,
) {
    let Some(Value::String(content)) = envelope.get("content") else {
        return;
    };
    let content = content.clone();
    let Some(Value::Object(metadata)) = envelope.get("metadata") else {
        return;
    };
    if !metadata.contains_key("nextOffset") {
        return;
    }
    let Some(Value::Object(display)) = metadata.get("display") else {
        return;
    };
    if display.get("type").and_then(Value::as_str) != Some("file") {
        return;
    }
    let line_start = display
        .get("lineStart")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1);
    let total_lines = display
        .get("totalLines")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let content_count = visible_content_lines(&content);
    let content_end = if content_count == 0 {
        line_start.saturating_sub(1)
    } else {
        line_start.saturating_add(content_count).saturating_sub(1)
    };
    let output = envelope
        .get("output")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let output_end = output.as_deref().and_then(last_read_row_line);
    let mut line_end = output_end.map_or(content_end, |end| content_end.min(end));
    let initial_keep_count = if line_end >= line_start {
        line_end.saturating_sub(line_start).saturating_add(1)
    } else {
        0
    };
    let initial_content_changed = initial_keep_count < content_count;
    let output_rows_mismatch = output_end.is_some_and(|end| end != content_end);
    let had_next_offset = metadata.get("nextOffset").and_then(Value::as_u64).is_some();
    let retained_continuation = output.as_deref().is_some_and(|text| {
        [
            "\n\n[Showing lines ",
            "\n\n(Showing lines ",
            "\n\n(Output capped at ",
        ]
        .iter()
        .any(|marker| text.contains(marker))
    });
    let raw_needs_notice = output_end.is_none()
        && output
            .as_deref()
            .is_some_and(|text| !text.contains("<content>\n"))
        && total_lines > line_end
        && (!had_next_offset || !retained_continuation);
    if output_rows_mismatch || raw_needs_notice {
        facts.output = true;
        facts.display = true;
    }
    let rewritten_output = if initial_content_changed || output_rows_mismatch {
        output.and_then(|output| {
            rewrite_read_continuation(&output, line_start, line_end, total_lines).map(
                |(rewritten, retained_end)| {
                    line_end = retained_end;
                    rewritten
                },
            )
        })
    } else if raw_needs_notice {
        rewrite_raw_read_continuation(&content, line_start, line_end, total_lines).map(
            |(rewritten, retained_end)| {
                line_end = retained_end;
                rewritten
            },
        )
    } else {
        None
    };
    let keep_count = if line_end >= line_start {
        line_end.saturating_sub(line_start).saturating_add(1)
    } else {
        0
    };
    let synchronized_content = take_complete_content_lines(&content, keep_count);
    let content_changed = synchronized_content != content;
    let existing_truncated = display
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let truncated = existing_truncated
        || content_changed
        || output_rows_mismatch
        || (total_lines > 0 && line_end < total_lines);
    let next_offset = (truncated && total_lines > line_end).then(|| line_end.saturating_add(1));

    if content_changed {
        envelope.insert(
            "content".to_string(),
            Value::String(synchronized_content.clone()),
        );
        facts.display = true;
    }
    if let Some(Value::Object(metadata)) = envelope.get_mut("metadata") {
        metadata.insert(
            "nextOffset".to_string(),
            next_offset.map_or(Value::Null, |offset| json!(offset)),
        );
        if let Some(Value::Object(display)) = metadata.get_mut("display") {
            display.insert(
                "text".to_string(),
                Value::String(synchronized_content.clone()),
            );
            display.insert("lineEnd".to_string(), json!(line_end));
            if truncated {
                display.insert("truncated".to_string(), Value::Bool(true));
            }
        }
        if truncated {
            metadata.insert("truncated".to_string(), Value::Bool(true));
        }
    }
    if envelope.contains_key("nextOffset") {
        envelope.insert(
            "nextOffset".to_string(),
            next_offset.map_or(Value::Null, |offset| json!(offset)),
        );
    }
    if let Some(output) = rewritten_output {
        envelope.insert("output".to_string(), Value::String(output));
    }
}

/// Count non-sentinel content lines in a normalized coding preview.
fn visible_content_lines(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content
            .split('\n')
            .count()
            .saturating_sub(usize::from(content.ends_with('\n')))
    }
}

/// Keep a prefix of complete content lines without a trailing delimiter.
fn take_complete_content_lines(content: &str, count: usize) -> String {
    if count >= visible_content_lines(content) {
        return content.to_owned();
    }
    content
        .split('\n')
        .take(count)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Return the last complete numbered row in a canonical or legacy Read wrapper.
fn last_read_row_line(output: &str) -> Option<usize> {
    const OPEN: &str = "<content>\n";
    const CLOSE: &str = "\n</content>";
    let body_start = output.find(OPEN)?.saturating_add(OPEN.len());
    let close_start = output[body_start..].find(CLOSE)?.saturating_add(body_start);
    output[body_start..close_start]
        .lines()
        .filter_map(|row| {
            if let Some(hash) = row.find('#')
                && row[hash + 1..].find(':').is_some_and(|colon| colon > 0)
                && let Ok(line) = row[..hash].trim().parse::<usize>()
            {
                return Some(line);
            }
            let (line, _) = row.split_once(':')?;
            line.trim().parse::<usize>().ok()
        })
        .next_back()
}

/// Rewrite a retained Read wrapper to a complete-row prefix that fits with its
/// synchronized continuation notice.
fn rewrite_read_continuation(
    output: &str,
    line_start: usize,
    line_end: usize,
    total_lines: usize,
) -> Option<(String, usize)> {
    const OPEN: &str = "<content>\n";
    const CLOSE: &str = "\n</content>";
    let body_start = output.find(OPEN)?.saturating_add(OPEN.len());
    let close_start = output[body_start..].find(CLOSE)?.saturating_add(body_start);
    let body = &output[body_start..close_start];
    let marker = [
        "\n\n[Showing lines ",
        "\n\n(Showing lines ",
        "\n\n(Output capped at ",
    ]
    .iter()
    .filter_map(|marker| body.rfind(marker).map(|index| (index, *marker)))
    .max_by_key(|(index, _)| *index);
    let inferred_legacy = body.lines().next().is_some_and(|row| {
        !row.contains('#')
            && row
                .split_once(':')
                .is_some_and(|(line, _)| line.trim().parse::<usize>().is_ok())
    });
    let (rows_end, legacy, byte_limited) = marker.map_or(
        (body.len(), inferred_legacy, true),
        |(marker_start, marker)| {
            (
                marker_start,
                marker.starts_with("\n\n("),
                marker.contains("Output capped") || body[marker_start + 2..].contains("byte limit"),
            )
        },
    );
    let rows = body[..rows_end].lines().collect::<Vec<_>>();
    let requested_count = if line_end >= line_start {
        line_end.saturating_sub(line_start).saturating_add(1)
    } else {
        0
    }
    .min(rows.len());

    for count in (0..=requested_count).rev() {
        let retained_end = if count == 0 {
            line_start.saturating_sub(1)
        } else {
            line_start.saturating_add(count).saturating_sub(1)
        };
        let mut rewritten = String::with_capacity(output.len());
        rewritten.push_str(&output[..body_start]);
        rewritten.push_str(&rows[..count].join("\n"));
        if retained_end < total_lines {
            if count > 0 {
                rewritten.push_str("\n\n");
            }
            let reason = if byte_limited {
                format!(" ({} byte limit)", MAX_CODING_OUTPUT_BYTES)
            } else {
                String::new()
            };
            let next_offset = retained_end.saturating_add(1);
            if legacy {
                rewritten.push_str(&format!(
                    "(Showing lines {line_start}-{retained_end} of {total_lines}{reason}. Use offset={next_offset} to continue.)"
                ));
            } else {
                rewritten.push_str(&format!(
                    "[Showing lines {line_start}-{retained_end} of {total_lines}{reason}. Use offset={next_offset} to continue.]"
                ));
            }
        }
        rewritten.push_str(&output[close_start..]);
        if serialized_string_len(&rewritten) <= MAX_CODING_OUTPUT_BYTES {
            return Some((rewritten, retained_end));
        }
    }

    None
}

/// Fit a capped raw Read prefix with a complete continuation notice.
fn rewrite_raw_read_continuation(
    content: &str,
    line_start: usize,
    line_end: usize,
    total_lines: usize,
) -> Option<(String, usize)> {
    let rows = content.split('\n').collect::<Vec<_>>();
    let requested_count = if line_end >= line_start {
        line_end.saturating_sub(line_start).saturating_add(1)
    } else {
        0
    }
    .min(visible_content_lines(content));
    for count in (0..=requested_count).rev() {
        let retained_end = if count == 0 {
            line_start.saturating_sub(1)
        } else {
            line_start.saturating_add(count).saturating_sub(1)
        };
        let next_offset = retained_end.saturating_add(1);
        let notice = format!(
            "[Showing lines {line_start}-{retained_end} of {total_lines} ({} byte limit). Use offset={next_offset} to continue.]",
            MAX_CODING_OUTPUT_BYTES
        );
        let mut rewritten = rows[..count].join("\n");
        if !rewritten.is_empty() {
            rewritten.push_str("\n\n");
        }
        rewritten.push_str(&notice);
        if serialized_string_len(&rewritten) <= MAX_CODING_OUTPUT_BYTES {
            return Some((rewritten, retained_end));
        }
    }
    None
}

/// Bound a Read file wrapper by complete rows while retaining its footer.
pub(super) fn bound_read_output(
    text: &str,
    max_serialized_bytes: usize,
    facts: &mut TruncationFacts,
) -> Option<String> {
    const OPEN: &str = "<content>\n";
    const CLOSE: &str = "\n</content>";
    let body_start = text.find(OPEN)?.saturating_add(OPEN.len());
    let close_start = text[body_start..].find(CLOSE)?.saturating_add(body_start);
    let close_end = close_start.saturating_add(CLOSE.len());
    let prefix = &text[..body_start];
    let body = &text[body_start..close_start];
    let warning_suffix = &text[close_end..];
    let continuation_start = [
        "\n\n[Showing lines ",
        "\n\n(Showing lines ",
        "\n\n(Output capped at ",
    ]
    .iter()
    .filter_map(|marker| body.rfind(marker))
    .max();
    let (rows_text, continuation) = continuation_start.map_or((body, None), |start| {
        (&body[..start], Some(body[start + 2..].to_owned()))
    });
    let rows = if rows_text.is_empty() {
        Vec::new()
    } else {
        rows_text.split('\n').collect::<Vec<_>>()
    };

    let prefix_bytes = serialized_string_len(prefix).saturating_sub(2);
    let close_bytes = serialized_string_len(CLOSE).saturating_sub(2);
    let continuation_bytes = continuation.as_deref().map_or_else(
        || {
            let digits = usize::MAX.to_string().len();
            let number = "9".repeat(digits);
            let worst_case = format!(
                "\n\n[Showing lines {number}-{number} of {number} ({} byte limit). Use offset={number} to continue.]",
                max_serialized_bytes
            );
            serialized_string_len(&worst_case).saturating_sub(2)
        },
        |notice| {
            serialized_string_len("\n\n")
                .saturating_sub(2)
                .saturating_add(serialized_string_len(notice).saturating_sub(2))
        },
    );
    let warning_bytes = serialized_string_len(warning_suffix).saturating_sub(2);
    let fixed_without_warning = 2usize
        .saturating_add(prefix_bytes)
        .saturating_add(close_bytes)
        .saturating_add(continuation_bytes);
    if fixed_without_warning > max_serialized_bytes {
        return None;
    }
    let warning_budget = max_serialized_bytes.saturating_sub(fixed_without_warning);
    let warning = if warning_bytes > warning_budget {
        let mut warning = warning_suffix.to_owned();
        let mut warning_facts = TruncationFacts::default();
        warning = bound_string(
            warning,
            warning_budget,
            warning_budget,
            TruncationKind::Output,
            &mut warning_facts,
        );
        if warning_facts.any() {
            facts.mark(TruncationKind::Output);
        }
        warning
    } else {
        warning_suffix.to_owned()
    };
    let warning_bytes = serialized_string_len(&warning).saturating_sub(2);
    let mut used = 2usize
        .saturating_add(prefix_bytes)
        .saturating_add(close_bytes)
        .saturating_add(continuation_bytes)
        .saturating_add(warning_bytes);
    let mut count = 0usize;
    for row in &rows {
        let separator = usize::from(count > 0);
        let row_bytes = serialized_string_len(row).saturating_sub(2);
        let required = separator
            .saturating_mul(serialized_string_len("\n").saturating_sub(2))
            .saturating_add(row_bytes);
        if used.saturating_add(required) > max_serialized_bytes {
            break;
        }
        used = used.saturating_add(required);
        count += 1;
    }
    let mut output = String::with_capacity(text.len().min(max_serialized_bytes));
    output.push_str(prefix);
    if count > 0 {
        output.push_str(&rows[..count].join("\n"));
    }
    if let Some(notice) = continuation {
        output.push_str("\n\n");
        output.push_str(&notice);
    }
    output.push_str(CLOSE);
    output.push_str(&warning);
    if count < rows.len() || output != text {
        facts.mark(TruncationKind::Output);
    }
    Some(output)
}
