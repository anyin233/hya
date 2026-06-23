#![allow(dead_code)]

mod render_support;

use render_support::render;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

#[test]
fn completed_task_hides_raw_input_when_metadata_is_missing() {
    let mut state = AppState::default();
    with_completed_task(
        &mut state,
        json!({
            "prompt": "SECRET_API_TOKEN=abc123 inspect everything",
            "internal": "do not render raw task json"
        }),
        json!({ "members": [{ "status": "done", "summary": "ok" }] }),
        501,
    );

    let output = render(&mut state, 100, 28);

    assert!(
        output.contains("✓ Task Task"),
        "task row should fall back to a safe display label:\n{output}"
    );
    assert!(
        !output.contains("SECRET_API_TOKEN") && !output.contains(r#""prompt":"#),
        "task row should not leak raw task JSON:\n{output}"
    );
}

#[test]
fn completed_task_sanitizes_control_sequences_from_task_text() {
    let mut state = AppState::default();
    with_completed_task(
        &mut state,
        json!({
            "description": "Review \u{1b}[31mred\u{1b}[0m \u{1b}]8;;https://evil.test\u{7}link\u{1b}]8;;\u{7} \u{8}done",
            "subagent_type": "explore"
        }),
        json!({ "members": [{ "status": "done", "summary": "ok" }] }),
        501,
    );

    let output = render(&mut state, 100, 28);

    assert!(
        !output.contains('\u{1b}') && !output.contains('\u{7}') && !output.contains('\u{8}'),
        "task text should not contain terminal control sequences:\n{output:?}"
    );
    assert!(
        output.contains("Review red link done"),
        "sanitized task text should preserve readable content:\n{output}"
    );
}

#[test]
fn completed_task_counts_array_tool_calls() {
    let mut state = AppState::default();
    with_completed_task(
        &mut state,
        task_input("Inspect active task spacing", "explore"),
        json!({
            "members": [{ "status": "done", "summary": "ok" }],
            "tool_calls": [{ "name": "read" }, { "name": "grep" }]
        }),
        501,
    );

    let output = render(&mut state, 100, 28);

    assert!(
        output.contains("↳ 2 toolcalls · 501ms"),
        "array-shaped tool_calls should drive the delegated summary:\n{output}"
    );
}

#[test]
fn completed_task_suppresses_summary_when_explicit_count_is_malformed() {
    let mut state = AppState::default();
    with_completed_task(
        &mut state,
        task_input("Inspect active task spacing", "explore"),
        json!({
            "members": [{ "status": "done", "summary": "ok" }],
            "toolcalls": "0"
        }),
        501,
    );

    let output = render(&mut state, 100, 28);

    assert!(
        !output.contains("↳"),
        "malformed explicit toolcall count should suppress fallback:\n{output}"
    );
}

fn task_input(description: &str, subagent_type: &str) -> serde_json::Value {
    json!({
        "description": description,
        "prompt": "review the implementation",
        "subagent_type": subagent_type
    })
}

fn with_completed_task(
    state: &mut AppState,
    input: serde_json::Value,
    output: serde_json::Value,
    time_ms: u64,
) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("task");
    state.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    state.apply(&env(
        2,
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        3,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input,
        },
    ));
    state.apply(&env(
        4,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output,
            time_ms,
        },
    ));
}

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}
