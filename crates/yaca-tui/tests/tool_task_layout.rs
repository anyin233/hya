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

fn with_completed_task(state: &mut AppState, description: &str, subagent_type: &str) {
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
            input: json!({
                "description": description,
                "prompt": "review the implementation",
                "subagent_type": subagent_type
            }),
        },
    ));
    state.apply(&env(
        4,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "members": [{ "status": "done", "summary": "ok" }] }),
            time_ms: 9,
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
