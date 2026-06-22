#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::buffer::Buffer;
use ratatui::style::Color;
use render_support::render_buffer;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

fn rendered_row(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn find_text(buffer: &Buffer, width: u16, height: u16, needle: &str) -> (u16, u16) {
    for y in 0..height {
        let row = rendered_row(buffer, width, y);
        if let Some(x) = row.find(needle) {
            return (u16::try_from(x).unwrap(), y);
        }
    }
    panic!("missing {needle:?}");
}

#[test]
fn edit_diff_output_uses_opencode_diff_coloring() {
    // Given: an edit tool completed with unified-diff output.
    let mut state = AppState::default();
    with_completed_edit_diff(
        &mut state,
        "@@ -1,2 +1,2 @@\n-old line\n+new line\n context",
    );

    // When: the transcript renders the completed tool block.
    let width = 100;
    let height = 24;
    let buffer = render_buffer(&mut state, width, height);

    // Then: diff semantics are visible through OpenCode-style color roles.
    let (hunk_x, hunk_y) = find_text(&buffer, width, height, "@@ -1,2");
    let (removed_x, removed_y) = find_text(&buffer, width, height, "-old line");
    let (added_x, added_y) = find_text(&buffer, width, height, "+new line");
    assert_eq!(buffer[(hunk_x, hunk_y)].fg, Color::Rgb(128, 128, 128));
    assert_eq!(buffer[(removed_x, removed_y)].fg, Color::Rgb(224, 108, 117));
    assert_eq!(buffer[(added_x, added_y)].fg, Color::Rgb(127, 216, 143));
}

fn with_completed_edit_diff(state: &mut AppState, diff: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("edit");
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
            input: json!({ "path": "src/lib.rs" }),
        },
    ));
    state.apply(&env(
        4,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "diff": diff }),
            time_ms: 12,
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
