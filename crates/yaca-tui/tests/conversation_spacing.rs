#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::buffer::Buffer;
use ratatui::style::Color;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

use render_support::{
    buffer_text, find_rendered_text, render_buffer, with_text_message, with_tool_error_message,
};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn row_text(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn with_assistant_text_then_tool(state: &mut AppState) {
    let session = SessionId::new();
    let message = MessageId::new();
    let text = PartId::new();
    let tool = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("read");
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
        Event::TextStart {
            session,
            message,
            part: text,
        },
    ));
    state.apply(&env(
        3,
        Event::TextDelta {
            session,
            message,
            part: text,
            delta: "I will inspect the file.".to_string(),
        },
    ));
    state.apply(&env(
        4,
        Event::ToolInputStart {
            session,
            message,
            part: tool,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        5,
        Event::ToolCallRequested {
            session,
            message,
            part: tool,
            call,
            name,
            input: json!({ "path": "README.md" }),
        },
    ));
    state.apply(&env(
        6,
        Event::ToolResult {
            session,
            message,
            part: tool,
            call,
            output: json!({ "ok": true }),
            time_ms: 8,
        },
    ));
}

#[test]
fn assistant_text_and_tool_rows_have_intentional_spacing() {
    let mut state = AppState::default();
    // Given: one assistant message streams text and then a tool call.
    with_assistant_text_then_tool(&mut state);

    // When: the transcript renders.
    let buffer = render_buffer(&mut state, 80, 24);

    // Then: the tool row is visually separated from prose by an empty row.
    let (_, text_y) = find_rendered_text(&buffer, 80, 24, "I will inspect").unwrap();
    let (_, tool_y) = find_rendered_text(&buffer, 80, 24, "→ Read README.md").unwrap();
    assert!(
        tool_y > text_y + 1,
        "tool rows should not be glued directly to assistant prose"
    );
    assert!(
        row_text(&buffer, 80, text_y + 1).trim().is_empty(),
        "the separator row between assistant prose and tools should be blank"
    );
}

#[test]
fn tool_error_detail_renders_as_a_railed_error_block() {
    let mut state = AppState::default();
    // Given: a failed tool call has an error detail.
    with_tool_error_message(&mut state, 1, "README.md", "permission denied");

    // When: the transcript renders.
    let buffer = render_buffer(&mut state, 120, 24);

    // Then: the detail uses a visible error rail, matching block-style failures.
    let (_, detail_y) = find_rendered_text(&buffer, 120, 24, "permission denied").unwrap();
    let detail_row = row_text(&buffer, 120, detail_y);
    assert!(
        detail_row.contains("▏ permission denied"),
        "tool error detail should render as a railed block row"
    );
    let rail_x = u16::try_from(detail_row.find('▏').unwrap()).unwrap();
    assert_eq!(buffer[(rail_x, detail_y)].fg, Color::Rgb(224, 108, 117));
}

#[test]
fn transcript_rails_do_not_use_box_drawing_border_glyphs() {
    let mut state = AppState::default();
    // Given: normal transcript content includes both user text and a tool error detail.
    with_text_message(&mut state, 1, Role::User, "read the file");
    with_tool_error_message(&mut state, 10, "README.md", "permission denied");

    // When: the transcript renders without any modal overlays.
    let buffer = render_buffer(&mut state, 120, 24);
    let text = buffer_text(&buffer, 120, 24);

    // Then: transcript rails use tonal block glyphs, not box-border glyphs.
    let user_row = text
        .lines()
        .find(|row| row.contains("read the file"))
        .unwrap_or_else(|| panic!("user row missing:\n{text}"));
    let tool_detail_row = text
        .lines()
        .find(|row| row.contains("permission denied"))
        .unwrap_or_else(|| panic!("tool detail row missing:\n{text}"));
    let user_transcript = user_row.split('│').next().unwrap_or(user_row);
    let tool_detail_transcript = tool_detail_row.split('│').next().unwrap_or(tool_detail_row);
    assert!(
        !user_transcript.contains('│') && !tool_detail_transcript.contains('│'),
        "borderless transcript rails should not trip frame alignment checks:\n{text}"
    );
    assert!(
        user_transcript.contains("▏") && tool_detail_transcript.contains("▏"),
        "transcript rows should still have a visible tonal rail:\n{text}"
    );
}
