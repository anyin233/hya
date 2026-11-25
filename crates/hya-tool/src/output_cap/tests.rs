use super::grep_budget::{bound_grep_groups, bound_grep_rows};
use super::json_budget::serialized_len;
use super::*;
use serde_json::{Map, Value, json};

#[test]
fn under_limit_preserves_original_value_shape() {
    let original = json!({"results": ["a", "b"]});
    let capped = cap_tool_output(original.clone());
    assert_eq!(capped, original);
}

/// Split a truncated view into (notice, head, marker, tail).
fn parts(text: &str) -> (&str, &str, &str, &str) {
    let (notice, rest) = text.split_once('\n').unwrap();
    let marker_at = rest.find("\n[… ").expect("marker between head and tail");
    let head = &rest[..marker_at];
    let after = &rest[marker_at + 1..];
    let (marker, tail) = after.split_once('\n').expect("marker + tail");
    (notice, head, marker, tail)
}

#[test]
fn over_limit_keeps_the_head_and_the_tail_with_a_marker() {
    let body = format!(
        "HEADER {}{} FOOTER",
        "a".repeat(3000),
        "b".repeat(MAX_TOOL_OUTPUT_CHARS)
    );
    let n = body.chars().count();
    let capped = cap_tool_output(Value::String(body.clone()));
    let text = capped.as_str().expect("string");
    let (notice, head, marker, tail) = parts(text);
    assert!(notice.starts_with("[tool output truncated: original "));
    assert!(notice.contains(&format!("{n} chars")), "{notice}");
    assert!(
        notice.contains(&format!(
            "showing first {TRUNCATED_HEAD_CHARS} and last {TRUNCATED_TAIL_CHARS} chars"
        )),
        "{notice}"
    );
    assert_eq!(head, &body[..TRUNCATED_HEAD_CHARS]);
    assert!(head.starts_with("HEADER "));
    assert_eq!(tail, &body[body.len() - TRUNCATED_TAIL_CHARS..]);
    assert!(tail.ends_with(" FOOTER"));
    let omitted = n - TRUNCATED_HEAD_CHARS - TRUNCATED_TAIL_CHARS;
    assert_eq!(marker, format!("[… {omitted} chars omitted …]"));
    assert!(text.chars().count() <= MAX_TOOL_OUTPUT_CHARS);
}

#[test]
fn head_and_tail_handle_multibyte_chars() {
    let body: String = "你".repeat(MAX_TOOL_OUTPUT_CHARS + 10);
    let capped = cap_tool_output(Value::String(body));
    let text = capped.as_str().unwrap();
    let (_, head, _, tail) = parts(text);
    assert_eq!(head.chars().count(), TRUNCATED_HEAD_CHARS);
    assert_eq!(tail.chars().count(), TRUNCATED_TAIL_CHARS);
    assert!(head.chars().chain(tail.chars()).all(|c| c == '你'));
}

#[test]
fn over_limit_json_object_is_stringified_then_capped() {
    let big = "y".repeat(MAX_TOOL_OUTPUT_CHARS + 50);
    let capped = cap_tool_output(json!({ "blob": big }));
    assert!(capped.is_string());
    let text = capped.as_str().unwrap();
    assert!(text.starts_with("[tool output truncated:"));
    let (_, head, _, tail) = parts(text);
    assert!(head.starts_with("{\"blob\":\"yyy"), "{head}");
    assert_eq!(tail.chars().count(), TRUNCATED_TAIL_CHARS);
}

#[test]
fn coding_policy_preserves_envelope_and_bounds_output() {
    let result = json!({
        "title": "src/main.rs",
        "output": "x".repeat(MAX_CODING_OUTPUT_BYTES + 100),
        "metadata": {
            "display": {"type": "file", "text": "line\n".repeat(20)},
            "truncated": false,
        },
        "content": "line\n".repeat(20),
    });
    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
    assert!(capped.is_object());
    assert_eq!(capped["title"], "src/main.rs");
    assert!(capped["output"].as_str().unwrap().len() <= MAX_CODING_OUTPUT_BYTES);
    assert_eq!(capped["metadata"]["outputTruncated"], true);
    assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
}

#[test]
fn hostile_metadata_is_dropped_and_row_bounded() {
    let result = json!({
        "title": "read",
        "output": "ok",
        "metadata": {
            "evil": "z".repeat(MAX_CODING_ENVELOPE_BYTES),
            "warnings": (0..40).map(|_| "w".repeat(MAX_CODING_DIAGNOSTIC_ROW_BYTES + 10)).collect::<Vec<_>>(),
            "display": {"rows": (0..2100).map(|line| json!({"line": line, "text": "x"})).collect::<Vec<_>>()},
        },
    });
    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
    let metadata = capped["metadata"].as_object().unwrap();
    assert!(!metadata.contains_key("evil"));
    assert_eq!(metadata["unknownFieldsDropped"], true);
    assert!(metadata["warnings"].as_array().unwrap().len() <= MAX_CODING_DIAGNOSTIC_ROWS);
    assert!(metadata["display"]["rows"].as_array().unwrap().len() <= MAX_CODING_GREP_ROWS);
    assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
}

#[test]
fn edit_policy_bounds_diff_separately() {
    let result = json!({
        "title": "src/main.rs",
        "output": "Edit applied",
        "metadata": {
            "diff": "-old\n+new\n".repeat(MAX_CODING_DIFF_BYTES),
            "filediff": {"patch": "p".repeat(MAX_CODING_DIFF_BYTES + 20), "additions": 1},
            "display": {"type": "file", "text": "final"},
        },
    });
    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::CodingWithDiff);
    assert!(capped["metadata"]["diff"].as_str().unwrap().len() <= MAX_CODING_DIFF_BYTES);
    assert_eq!(capped["metadata"]["diffTruncated"], true);
    assert_eq!(capped["metadata"]["display"]["text"], "final");
    assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
}
#[test]
fn coding_metadata_retains_current_write_and_bash_fields() {
    let result = json!({
        "title": "tool",
        "output": "ok",
        "metadata": {
            "existed": true,
            "created": false,
            "executable": true,
            "bytes": 42,
            "hardLink": true,
            "noopLocations": ["2#ABCD"],
            "count": 3,
            "matches": 2,
            "files": 1,
            "total": 2,
            "timedOut": false,
            "durationMs": 12,
            "timeoutSeconds": 300,
            "timeoutClamped": false,
            "deadlineCapped": true,
            "pty": true,
            "cwd": "/workspace"
        }
    });
    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
    let metadata = capped["metadata"].as_object().unwrap();
    assert_eq!(metadata["existed"], true);
    assert_eq!(metadata["created"], false);
    assert_eq!(metadata["executable"], true);
    assert_eq!(metadata["bytes"], 42);
    assert_eq!(metadata["hardLink"], true);
    assert_eq!(metadata["noopLocations"], json!(["2#ABCD"]));
    assert_eq!(metadata["count"], 3);
    assert_eq!(metadata["matches"], 2);
    assert_eq!(metadata["files"], 1);
    assert_eq!(metadata["total"], 2);
    assert_eq!(metadata["timedOut"], false);
    assert_eq!(metadata["durationMs"], 12);
    assert_eq!(metadata["timeoutSeconds"], 300);
    assert_eq!(metadata["timeoutClamped"], false);
    assert_eq!(metadata["deadlineCapped"], true);
    assert_eq!(metadata["pty"], true);
    assert_eq!(metadata["cwd"], "/workspace");
}

#[test]
fn deeply_nested_array_stops_at_depth_bound() {
    let mut value = Value::String("leaf".to_string());
    for _ in 0..64 {
        value = Value::Array(vec![value]);
    }
    let capped = cap_tool_output_with_policy(value, ToolResultPolicy::Coding);
    assert!(serialized_len(&capped) <= MAX_CODING_OUTPUT_BYTES);
    let mut depth = 0;
    let mut current = &capped;
    while let Value::Array(values) = current {
        depth += 1;
        if depth == 9 {
            break;
        }
        let Some(next) = values.first() else {
            break;
        };
        current = next;
    }
    assert!(depth <= 9);
}

#[test]
fn oversized_attachment_is_dropped_without_payload_truncation() {
    let payload = format!(
        "data:image/png;base64,{}",
        "A".repeat(MAX_CODING_ATTACHMENT_BYTES)
    );
    let result = json!({
        "title": "image",
        "output": "Image read successfully",
        "attachments": [{"type": "file", "mime": "image/png", "url": payload}],
        "metadata": {}
    });
    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
    assert_eq!(capped["attachments"], json!([]));
    assert_eq!(capped["metadata"]["attachmentsTruncated"], true);
    assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
}

#[test]
fn attachment_within_its_budget_survives_an_over_large_envelope() {
    let payload = format!("data:image/png;base64,{}", "A".repeat(1536 * 1024));
    let result = json!({
        "title": "image.png",
        "output": "o".repeat(MAX_CODING_OUTPUT_BYTES * 2),
        "content": "c".repeat(MAX_CODING_ENVELOPE_BYTES),
        "metadata": {},
        "attachments": [{"type": "file", "mime": "image/png", "url": payload}],
    });
    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
    assert_eq!(capped["attachments"][0]["url"], payload);
    assert_eq!(capped["metadata"].get("attachmentsTruncated"), None);
    assert_eq!(capped["metadata"]["outputTruncated"], true);
    let Value::Object(mut rest) = capped else {
        panic!("coding result stays an object");
    };
    rest.remove("attachments");
    assert!(serialized_len(&rest) <= MAX_CODING_ENVELOPE_BYTES);
}

#[test]
fn path_keyed_diagnostics_bound_rows_without_rewriting_paths() {
    let long_path = "p".repeat(MAX_CODING_DIAGNOSTIC_PATH_BYTES + 10);
    let mut diagnostics = Map::new();
    diagnostics.insert(long_path.clone(), json!([{"message": "must be omitted"}]));
    diagnostics.insert(
        "/src/big.rs".to_string(),
        json!([{"message": "x".repeat(MAX_CODING_DIAGNOSTIC_ROW_BYTES)}]),
    );
    for index in 0..MAX_CODING_DIAGNOSTIC_ROWS {
        diagnostics.insert(
            format!("/src/{index}.rs"),
            json!([{"message": format!("issue {index}")}]),
        );
    }
    let capped = cap_tool_output_with_policy(
        json!({
            "title": "edit",
            "output": "done",
            "metadata": {"diagnostics": Value::Object(diagnostics)}
        }),
        ToolResultPolicy::Coding,
    );
    let diagnostics = capped["metadata"]["diagnostics"].as_object().unwrap();
    assert!(!diagnostics.contains_key(&long_path));
    let total_rows: usize = diagnostics
        .values()
        .map(|rows| rows.as_array().map_or(0, Vec::len))
        .sum();
    assert!(total_rows <= MAX_CODING_DIAGNOSTIC_ROWS);
    for rows in diagnostics.values() {
        for row in rows.as_array().unwrap() {
            assert!(serialized_len(row) <= MAX_CODING_DIAGNOSTIC_ROW_BYTES);
        }
    }
    assert_eq!(capped["metadata"]["diagnosticsTruncated"], true);
}

#[test]
fn malformed_coding_array_is_bounded_without_stringifying_input() {
    let malformed = Value::Array(vec![Value::String(
        "x".repeat(MAX_CODING_OUTPUT_BYTES.saturating_mul(2)),
    )]);
    let capped = cap_tool_output_with_policy(malformed, ToolResultPolicy::Coding);
    assert!(capped.is_array());
    assert!(serialized_len(&capped) <= MAX_CODING_OUTPUT_BYTES);
}
/// Keep complete Grep display groups within one shared byte budget.
#[test]
fn coding_policy_counts_grep_group_bytes_once_and_keeps_later_groups() {
    let first_row = "L".repeat(32 * 1024);
    let later_row = "S".repeat(12 * 1024);
    let result = json!({
        "title": "needle",
        "output": "2 matches in 2 files.",
        "metadata": {
            "matches": 2,
            "files": 2,
            "truncated": false,
            "display": {
                "groups": [
                    {
                        "path": "large.rs",
                        "rows": [{"line": 1, "text": first_row.clone(), "isMatch": true}]
                    },
                    {
                        "path": "later.rs",
                        "rows": [{"line": 2, "text": later_row.clone(), "isMatch": true}]
                    }
                ]
            }
        }
    });

    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
    let envelope = capped
        .as_object()
        .expect("coding result must remain an object");
    assert!(envelope.get("title").is_some_and(Value::is_string));
    assert!(envelope.get("output").is_some_and(Value::is_string));
    assert!(envelope.get("metadata").is_some_and(Value::is_object));
    assert_eq!(capped["title"], "needle");
    assert_eq!(capped["output"], "2 matches in 2 files.");

    let metadata = capped["metadata"].as_object().expect("metadata object");
    assert_eq!(metadata["truncated"], false);
    let groups = metadata["display"]["groups"]
        .as_array()
        .expect("Grep display groups must remain an array");
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0]["path"], "large.rs");
    assert_eq!(groups[1]["path"], "later.rs");
    assert_eq!(groups[0]["rows"][0]["line"], 1);
    assert_eq!(groups[1]["rows"][0]["line"], 2);
    assert_eq!(
        groups[0]["rows"][0]["text"].as_str(),
        Some(first_row.as_str())
    );
    assert_eq!(
        groups[1]["rows"][0]["text"].as_str(),
        Some(later_row.as_str())
    );
    assert!(serialized_len(&metadata["display"]["groups"]) <= MAX_CODING_GREP_DISPLAY_BYTES);
    assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
}

/// Keep all Read projections on the same complete-row prefix after capping.
#[test]
fn coding_policy_synchronizes_read_rows_and_is_idempotent() {
    let result = json!({
        "title": "notes.txt",
        "output": "<content>\n1#AA:one\n2#BB:two\n3#CC:three\n</content>",
        "content": "one\ntwo",
        "metadata": {
            "nextOffset": 4,
            "display": {
                "type": "file",
                "text": "one\ntwo\nthree",
                "lineStart": 1,
                "lineEnd": 3,
                "totalLines": 4,
                "truncated": false
            }
        }
    });

    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
    assert_eq!(capped["content"], "one\ntwo");
    assert_eq!(capped["metadata"]["display"]["text"], "one\ntwo");
    assert_eq!(capped["metadata"]["display"]["lineEnd"], 2);
    assert_eq!(capped["metadata"]["nextOffset"], 3);
    let output = capped["output"].as_str().expect("bounded Read output");
    assert!(output.contains("1#AA:one\n2#BB:two"));
    assert!(!output.contains("3#CC:three"));
    assert!(output.contains("Showing lines 1-2 of 4"));
    assert!(output.contains("Use offset=3 to continue"));

    let capped_again = cap_tool_output_with_policy(capped.clone(), ToolResultPolicy::Coding);
    assert_eq!(capped_again, capped);
}

/// Keep the hidden legacy numbered Read wrapper synchronized after capping.
#[test]
fn coding_policy_synchronizes_legacy_read_rows() {
    let result = json!({
        "title": "notes.txt",
        "output": "<path>notes.txt</path>\n<type>file</type>\n<content>\n1: one\n2: two\n3: three\n\n(End of file - total 4 lines)\n</content>",
        "content": "one\ntwo",
        "metadata": {
            "nextOffset": null,
            "display": {
                "type": "file",
                "text": "one\ntwo\nthree",
                "lineStart": 1,
                "lineEnd": 3,
                "totalLines": 4,
                "truncated": false
            }
        }
    });

    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
    let output = capped["output"].as_str().expect("legacy Read output");
    assert!(output.contains("1: one\n2: two"));
    assert!(!output.contains("3: three"));
    assert!(output.contains("(Showing lines 1-2 of 4"));
    assert_eq!(capped["content"], "one\ntwo");
    assert_eq!(capped["metadata"]["display"]["lineEnd"], 2);
    assert_eq!(capped["metadata"]["nextOffset"], 3);
}

/// Charge serialized array delimiters before accepting a complete Grep row.
#[test]
fn grep_row_budget_omits_a_whole_row_that_only_fits_without_its_container() {
    let empty_row = json!({"text": ""});
    let payload_bytes = MAX_CODING_GREP_DISPLAY_BYTES - serialized_len(&empty_row);
    let row = json!({"text": "x".repeat(payload_bytes)});
    assert_eq!(serialized_len(&row), MAX_CODING_GREP_DISPLAY_BYTES);
    let mut facts = TruncationFacts::default();

    let bounded = bound_grep_rows(json!([row]), &mut facts);

    assert_eq!(bounded, json!([]));
    assert!(facts.rows);
    assert!(serialized_len(&bounded) <= MAX_CODING_GREP_DISPLAY_BYTES);
}

/// Reject malformed Grep groups atomically instead of slicing their JSON.
#[test]
fn grep_group_budget_drops_malformed_groups_atomically() {
    let malformed = Value::String("x".repeat(MAX_CODING_GREP_DISPLAY_BYTES));
    let mut facts = TruncationFacts::default();

    let bounded = bound_grep_groups(json!([malformed]), &mut facts);

    assert_eq!(bounded, json!([]));
    assert!(facts.groups);
    assert!(facts.display);
    assert!(serialized_len(&bounded) <= MAX_CODING_GREP_DISPLAY_BYTES);
}

/// Omit an oversized artifact path atomically instead of publishing a slice.
#[test]
fn coding_policy_never_slices_output_path() {
    let output_path = "p".repeat(MAX_CODING_OUTPUT_PATH_BYTES);
    let capped = cap_tool_output_with_policy(
        json!({
            "title": "bash",
            "output": "truncated",
            "metadata": {"outputPath": output_path}
        }),
        ToolResultPolicy::Coding,
    );

    assert!(capped["metadata"].get("outputPath").is_none());
    assert_eq!(capped["metadata"]["metadataTruncated"], true);
    assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
}

/// Keep raw Read bytes, display text, offsets, and notices synchronized.
#[test]
fn coding_policy_refits_escape_heavy_raw_read_with_complete_notice() {
    let row = "\"\\\t\u{0001}".repeat(64);
    let content = (0..400)
        .map(|_| row.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let result = json!({
        "title": "raw.txt",
        "output": content.clone(),
        "content": content.clone(),
        "metadata": {
            "nextOffset": null,
            "display": {
                "type": "file",
                "text": content,
                "lineStart": 1,
                "lineEnd": 400,
                "totalLines": 400,
                "truncated": false
            }
        }
    });

    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);
    let output = capped["output"].as_str().expect("raw Read output");
    let (raw, notice) = output
        .split_once("\n\n[Showing lines ")
        .expect("capped raw Read must retain its continuation notice");
    assert_eq!(capped["content"], raw);
    assert_eq!(capped["metadata"]["display"]["text"], raw);
    let line_end = capped["metadata"]["display"]["lineEnd"]
        .as_u64()
        .expect("lineEnd");
    assert_eq!(u64::try_from(raw.split('\n').count()).unwrap(), line_end);
    assert_eq!(capped["metadata"]["nextOffset"], line_end + 1);
    assert!(notice.contains(&format!("Use offset={} to continue", line_end + 1)));
    assert!(serialized_len(&capped["output"]) <= MAX_CODING_OUTPUT_BYTES);
    assert!(serialized_len(&capped) <= MAX_CODING_ENVELOPE_BYTES);
}
