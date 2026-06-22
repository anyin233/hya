#![allow(dead_code, clippy::unwrap_used)]

mod render_support;

use render_support::render;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

#[test]
fn running_inline_tools_use_opencode_pending_text() {
    // Given: representative inline tool calls that have started but not completed.
    let cases = [
        (
            "shell",
            json!({ "cmd": "cargo test" }),
            "Writing command...",
        ),
        ("read", json!({ "path": "README.md" }), "Reading file..."),
        (
            "write",
            json!({ "path": "README.md" }),
            "Preparing write...",
        ),
        ("edit", json!({ "path": "README.md" }), "Preparing edit..."),
        ("glob", json!({ "pattern": "*.rs" }), "Finding files..."),
        (
            "grep",
            json!({ "pattern": "render", "path": "crates/yaca-tui" }),
            "Searching content...",
        ),
        (
            "webfetch",
            json!({ "url": "https://opencode.ai" }),
            "Fetching from the web...",
        ),
        (
            "websearch",
            json!({ "query": "opencode tui layout" }),
            "Searching web...",
        ),
        (
            "task",
            json!({ "description": "review plan" }),
            "Delegating...",
        ),
        (
            "todowrite",
            json!({ "todos": [{ "content": "write test", "status": "pending" }] }),
            "Updating todos...",
        ),
        (
            "ask_user",
            json!({ "question": "Continue?", "options": ["yes", "no"] }),
            "Asking questions...",
        ),
        ("skill", json!({ "name": "rust" }), "Loading skill..."),
    ];

    for (idx, (tool_name, input, pending)) in cases.into_iter().enumerate() {
        // When: each running tool is rendered in the transcript.
        let output = render_running_tool(idx as u64, tool_name, input);

        // Then: yaca matches OpenCode's inline pending fallback row.
        let expected = format!("~ {pending}");
        assert!(
            output.contains(&expected),
            "{tool_name} should render OpenCode pending text {expected:?}:\n{output}"
        );
        assert!(
            !output.contains("running"),
            "{tool_name} should not append yaca's generic running suffix:\n{output}"
        );
    }
}

fn render_running_tool(base_seq: u64, tool_name: &str, input: serde_json::Value) -> String {
    let mut state = AppState::default();
    with_running_tool(
        &mut state,
        base_seq.saturating_mul(10) + 1,
        tool_name,
        input,
    );
    render(&mut state, 100, 16)
}

fn with_running_tool(
    state: &mut AppState,
    base_seq: u64,
    tool_name: &str,
    input: serde_json::Value,
) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new(tool_name);
    state.apply(&env(
        base_seq,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    state.apply(&env(
        base_seq + 1,
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        base_seq + 2,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input,
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
