#![allow(dead_code, clippy::unwrap_used)]

mod render_support;

use render_support::render;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

#[test]
fn search_and_web_tools_use_opencode_glyphs_and_titles() {
    // Given: completed search and network tool calls in an assistant transcript.
    let mut state = AppState::default();
    with_completed_tool(&mut state, 1, "ls", json!({ "path": "crates/yaca-tui" }));
    with_completed_tool(
        &mut state,
        10,
        "find",
        json!({ "pattern": "*.rs", "path": "crates/yaca-tui" }),
    );
    with_completed_tool(
        &mut state,
        20,
        "grep",
        json!({ "pattern": "render_tool", "path": "crates/yaca-tui" }),
    );
    with_completed_tool(&mut state, 30, "glob", json!({ "pattern": "*.rs" }));
    with_completed_tool(
        &mut state,
        40,
        "webfetch",
        json!({ "url": "https://opencode.ai" }),
    );
    with_completed_tool(
        &mut state,
        50,
        "websearch",
        json!({ "query": "opencode tui layout" }),
    );

    // When: the transcript is rendered on a normal terminal width.
    let output = render(&mut state, 120, 28);

    // Then: yaca uses OpenCode's tool-specific glyphs, titles, and compact summaries.
    assert!(
        output.contains("→ List crates/yaca-tui"),
        "ls should render like OpenCode's List tool instead of raw JSON:\n{output}"
    );
    assert!(
        output.contains("✱ Find \"*.rs\" in crates/yaca-tui"),
        "find should quote the searched pattern like OpenCode search rows:\n{output}"
    );
    assert!(
        output.contains("✱ Grep \"render_tool\" in crates/yaca-tui"),
        "grep should quote the searched pattern like OpenCode search rows:\n{output}"
    );
    assert!(
        output.contains("✱ Glob \"*.rs\""),
        "glob should quote the pattern like OpenCode search rows:\n{output}"
    );
    assert!(
        output.contains("% WebFetch https://opencode.ai"),
        "webfetch should render with the OpenCode percent glyph and camel-case title:\n{output}"
    );
    assert!(
        output.contains("◈ WebSearch \"opencode tui layout\""),
        "websearch should quote the query like OpenCode websearch rows:\n{output}"
    );
    assert!(
        !output.contains("completed ✓"),
        "successful inline tools should not append generic completed/time suffixes:\n{output}"
    );
}

fn with_completed_tool(
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
    state.apply(&env(
        base_seq + 3,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "ok": true }),
            time_ms: base_seq,
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
