#![allow(dead_code)]

mod render_support;

use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

use render_support::render;

#[test]
fn generic_tool_output_collapses_after_three_lines_like_opencode() {
    // Given: a completed non-shell tool with block output longer than OpenCode's generic preview.
    let mut state = AppState::default();
    with_completed_generic_output(&mut state, "line 1\nline 2\nline 3\nline 4\nline 5");

    // When: the transcript is rendered.
    let output = render(&mut state, 100, 24);

    // Then: generic output uses OpenCode's three-line preview, not the shell ten-line preview.
    assert!(
        output.contains("▏ line 3"),
        "generic output should include the third preview line:\n{output}"
    );
    assert!(
        !output.contains("▏ line 4"),
        "generic output should collapse after the third preview line:\n{output}"
    );
    assert!(
        output.contains("▏ …"),
        "collapsed generic output should advertise overflow:\n{output}"
    );
}

fn with_completed_generic_output(state: &mut AppState, output: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("custom_tool");
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
            input: json!({ "prompt": "inspect" }),
        },
    ));
    state.apply(&env(
        4,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "output": output }),
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
