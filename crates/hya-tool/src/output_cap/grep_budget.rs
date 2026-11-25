use serde_json::{Map, Value};

use super::json_budget::{bound_json_value, bound_string_or_json, serialized_len};
use super::{
    MAX_CODING_GREP_DISPLAY_BYTES, MAX_CODING_GREP_GROUPS, MAX_CODING_GREP_ROWS,
    MAX_GENERIC_METADATA_ITEMS, MAX_SMALL_METADATA_STRING_BYTES, TruncationFacts, TruncationKind,
};

struct GrepDisplayBudget {
    bytes_left: usize,
    rows_left: usize,
}

/// Bound a standalone Grep row list to the shared display limits.
pub(super) fn bound_grep_rows(value: Value, facts: &mut TruncationFacts) -> Value {
    let Value::Array(rows) = value else {
        facts.rows = true;
        facts.display = true;
        return Value::Array(Vec::new());
    };
    let mut budget = GrepDisplayBudget {
        // Reserve the serialized array delimiters before fitting complete rows.
        bytes_left: MAX_CODING_GREP_DISPLAY_BYTES.saturating_sub(2),
        rows_left: MAX_CODING_GREP_ROWS,
    };
    Value::Array(bound_grep_row_list(rows, &mut budget, facts))
}

/// Bound Grep groups to one hundred files and two thousand rows.
pub(super) fn bound_grep_groups(value: Value, facts: &mut TruncationFacts) -> Value {
    let Value::Array(groups) = value else {
        facts.groups = true;
        facts.display = true;
        return Value::Array(Vec::new());
    };
    let mut budget = GrepDisplayBudget {
        // Reserve the serialized array delimiters before fitting complete groups.
        bytes_left: MAX_CODING_GREP_DISPLAY_BYTES.saturating_sub(2),
        rows_left: MAX_CODING_GREP_ROWS,
    };
    let mut bounded = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        if index >= MAX_CODING_GREP_GROUPS {
            facts.groups = true;
            break;
        }
        if !group.is_object() || group.get("rows").is_some_and(|rows| !rows.is_array()) {
            facts.groups = true;
            facts.display = true;
            continue;
        }
        let had_rows = group
            .get("rows")
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty());
        let group = bound_grep_group(group, &budget, facts);
        let size = serialized_len(&group);
        let separator = usize::from(!bounded.is_empty());
        if had_rows
            && group
                .get("rows")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        {
            facts.rows = true;
            facts.display = true;
            continue;
        }
        if size.saturating_add(separator) > budget.bytes_left {
            facts.display = true;
            continue;
        }
        let rows_kept = group
            .get("rows")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        budget.bytes_left = budget.bytes_left.saturating_sub(size + separator);
        budget.rows_left = budget.rows_left.saturating_sub(rows_kept);
        bounded.push(group);
    }
    Value::Array(bounded)
}

/// Bound one Grep group and its nested rows.
fn bound_grep_group(
    value: Value,
    budget: &GrepDisplayBudget,
    facts: &mut TruncationFacts,
) -> Value {
    let Value::Object(fields) = value else {
        return bound_json_value(
            value,
            budget.bytes_left,
            6,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Display,
            facts,
        );
    };
    let mut bounded = Map::new();
    let mut rows = None;
    for (key, value) in fields {
        let value = match key.as_str() {
            "path" | "file" | "title" => bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                4,
                MAX_GENERIC_METADATA_ITEMS,
                TruncationKind::Metadata,
                facts,
            ),
            "rows" => {
                rows = Some(match value {
                    Value::Array(rows) => rows,
                    other => {
                        facts.rows = true;
                        Vec::from([other])
                    }
                });
                Value::Array(Vec::new())
            }
            "matches" | "count" | "total" | "truncated" => bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                2,
                8,
                TruncationKind::Metadata,
                facts,
            ),
            _ => {
                facts.unknown = true;
                facts.metadata = true;
                continue;
            }
        };
        bounded.insert(key, value);
    }

    if let Some(rows) = rows {
        // Reserve the group wrapper once.  The row list then consumes only
        // row payload bytes, so the outer group is not charged a second time.
        let wrapper_bytes = serialized_len(&Value::Object(bounded.clone()));
        let mut local = GrepDisplayBudget {
            bytes_left: budget.bytes_left.saturating_sub(wrapper_bytes),
            rows_left: budget.rows_left,
        };
        let rows = bound_grep_row_list(rows, &mut local, facts);
        bounded.insert("rows".to_string(), Value::Array(rows));
    }
    Value::Object(bounded)
}

/// Bound and count Grep rows while preserving match identity fields.
fn bound_grep_row_list(
    rows: Vec<Value>,
    budget: &mut GrepDisplayBudget,
    facts: &mut TruncationFacts,
) -> Vec<Value> {
    let mut bounded = Vec::new();
    for row in rows {
        if !row.is_object() {
            facts.rows = true;
            facts.display = true;
            continue;
        }
        if budget.rows_left == 0 {
            facts.rows = true;
            break;
        }
        let mut row_facts = TruncationFacts::default();
        let row = bound_grep_row(row, budget.bytes_left, &mut row_facts);
        if row_facts.display {
            facts.merge(&row_facts);
            facts.rows = true;
            continue;
        }
        facts.merge(&row_facts);
        let size = serialized_len(&row);
        let separator = usize::from(!bounded.is_empty());
        if size.saturating_add(separator) > budget.bytes_left {
            facts.rows = true;
            continue;
        }
        budget.bytes_left = budget.bytes_left.saturating_sub(size + separator);
        budget.rows_left -= 1;
        bounded.push(row);
    }
    bounded
}

/// Bound the allowlisted fields of one Grep row.
fn bound_grep_row(value: Value, available_bytes: usize, facts: &mut TruncationFacts) -> Value {
    let Value::Object(fields) = value else {
        return bound_json_value(
            value,
            available_bytes,
            4,
            MAX_GENERIC_METADATA_ITEMS,
            TruncationKind::Display,
            facts,
        );
    };
    let mut bounded = Map::new();
    for (key, value) in fields {
        let value = match key.as_str() {
            "text" | "match" | "context" => {
                bound_string_or_json(value, available_bytes, TruncationKind::Display, facts)
            }
            "line" | "isMatch" | "kind" => bound_json_value(
                value,
                MAX_SMALL_METADATA_STRING_BYTES,
                2,
                8,
                TruncationKind::Metadata,
                facts,
            ),
            _ => {
                facts.unknown = true;
                facts.metadata = true;
                continue;
            }
        };
        bounded.insert(key, value);
    }
    Value::Object(bounded)
}
