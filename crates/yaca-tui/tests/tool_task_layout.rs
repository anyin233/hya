#![allow(dead_code)]

mod render_support;

use render_support::render;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

#[test]
fn completed_task_renders_opencode_task_snapshot() {
    // Given: an assistant turn with a completed task tool carrying subagent metadata.
    let mut state = AppState::default();
    with_completed_task(&mut state, "Cross-model plan review", "quick");

    // When: the transcript is rendered.
    let output = render(&mut state, 100, 28);

    // Then: yaca mirrors OpenCode's task final snapshot instead of dumping raw JSON.
    assert!(
        output.contains("✓ Task Cross-model plan review"),
        "task status row should use OpenCode's task icon and description:\n{output}"
    );
    assert!(
        output.contains("# Quick Task"),
        "task snapshot title missing:\n{output}"
    );
    assert!(
        output.contains("Cross-model plan review"),
        "task snapshot should include the requested task description:\n{output}"
    );
    assert!(
        !output.contains(r#""subagent_type":"#),
        "task snapshot should not expose raw JSON input:\n{output}"
    );
}

#[test]
fn completed_task_renders_toolcall_count_summary() {
    // Given: a completed task tool reports the number of delegated tool calls.
    let mut state = AppState::default();
    with_completed_task_with_summary(&mut state, "Cross-model plan review", "oracle", 9, 501);

    // When: the transcript is rendered.
    let output = render(&mut state, 100, 28);

    // Then: the task block mirrors OpenCode's subagent summary row.
    assert!(
        output.contains("↳ 9 toolcalls · 501ms"),
        "task snapshot should show delegated toolcall count and duration:\n{output}"
    );
}

#[test]
fn completed_task_derives_summary_from_builtin_members_output() {
    // Given: the built-in task tool output shape contains members but no
    // explicit toolcall count.
    let mut state = AppState::default();
    with_completed_task_members_only(&mut state, "Inspect active task spacing", "explore", 501);

    // When: the transcript is rendered.
    let output = render(&mut state, 100, 28);

    // Then: yaca still renders the OpenCode-style task summary row.
    assert!(
        output.contains("↳ 1 toolcall · 501ms"),
        "task snapshot should derive a delegated summary from built-in output:\n{output}"
    );
}

#[test]
fn completed_task_suppresses_summary_when_explicit_count_is_zero() {
    // Given: a task result explicitly reports zero delegated tool calls.
    let mut state = AppState::default();
    with_completed_task_with_summary(&mut state, "Inspect planning notes", "explore", 0, 501);

    // When: the transcript is rendered.
    let output = render(&mut state, 100, 28);

    // Then: yaca does not invent a summary from unrelated member metadata.
    assert!(
        !output.contains("↳"),
        "zero delegated tool calls should not render a task summary:\n{output}"
    );
}

fn with_completed_task(state: &mut AppState, description: &str, subagent_type: &str) {
    with_completed_task_with_summary(state, description, subagent_type, 0, 9);
}

fn with_completed_task_members_only(
    state: &mut AppState,
    description: &str,
    subagent_type: &str,
    time_ms: u64,
) {
    with_completed_task_output(
        state,
        task_input(description, subagent_type),
        json!({ "members": [{ "status": "done", "summary": "ok" }] }),
        time_ms,
    );
}

fn with_completed_task_with_summary(
    state: &mut AppState,
    description: &str,
    subagent_type: &str,
    toolcalls: u64,
    time_ms: u64,
) {
    with_completed_task_output(
        state,
        task_input(description, subagent_type),
        json!({
            "members": [{ "status": "done", "summary": "ok" }],
            "toolcalls": toolcalls
        }),
        time_ms,
    );
}

fn task_input(description: &str, subagent_type: &str) -> serde_json::Value {
    json!({
        "description": description,
        "prompt": "review the implementation",
        "subagent_type": subagent_type
    })
}

fn with_completed_task_output(
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
