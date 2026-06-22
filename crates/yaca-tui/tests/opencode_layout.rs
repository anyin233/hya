#![allow(clippy::unwrap_used, clippy::field_reassign_with_default)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::{AppState, draw};

fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f, state)).unwrap();
    terminal.backend().buffer().clone()
}

fn render(state: &mut AppState, width: u16, height: u16) -> String {
    let buffer = render_buffer(state, width, height);
    buffer_text(&buffer, width, height)
}

fn buffer_text(buffer: &Buffer, width: u16, height: u16) -> String {
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn with_text_message(state: &mut AppState, role: Role, text: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    state.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role,
        },
    ));
    state.apply(&env(
        2,
        Event::TextStart {
            session,
            message,
            part,
        },
    ));
    state.apply(&env(
        3,
        Event::TextDelta {
            session,
            message,
            part,
            delta: text.to_string(),
        },
    ));
}

fn with_session(state: &mut AppState, workdir: &str) {
    let session = SessionId::new();
    state.apply(&env(
        1,
        Event::SessionCreated {
            session,
            parent: None,
            agent: "build".into(),
            model: "fake".into(),
            workdir: workdir.to_string(),
        },
    ));
}

#[test]
fn prompt_renders_opencode_metadata_band() {
    let mut state = AppState {
        model: "kimi-k2".to_string(),
        input: "ship the tui".to_string(),
        reasoning_effort: Some("max".to_string()),
        ..AppState::default()
    };

    // Given: a typed prompt with model and thinking-effort metadata.
    // When: the composer renders at a wide OpenCode-style terminal width.
    let text = render(&mut state, 120, 20);

    // Then: input and metadata sit in the grounded composer, not a border title.
    assert!(text.contains("ship the tui"), "typed prompt still renders");
    assert!(text.contains("kimi-k2"), "metadata shows current model");
    assert!(text.contains("think max"), "metadata shows thinking effort");
    assert!(
        text.contains("ctrl+p commands"),
        "metadata exposes command affordance"
    );
    assert!(
        !text.contains("message —"),
        "composer should not use a bordered title"
    );
}

#[test]
fn composer_is_borderless() {
    let mut state = AppState {
        input: "hello".to_string(),
        ..AppState::default()
    };

    // Given: the prompt area is visible at the narrow supported width.
    // When: the composer renders.
    let text = render(&mut state, 80, 12);

    // Then: it uses a rail and tonal surface instead of box corners.
    for glyph in ["┌", "┐", "└", "┘"] {
        assert!(
            !text.contains(glyph),
            "composer should not draw box border corner {glyph}"
        );
    }
}

#[test]
fn sidebar_uses_tonal_column_without_border_title() {
    let mut state = AppState {
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        input: "type here".to_string(),
        ..AppState::default()
    };
    with_text_message(&mut state, Role::Assistant, "HELLOTUI");

    // Given: a wide terminal with the context rail visible.
    // When: the app renders the shell layout.
    let text = render(&mut state, 120, 36);

    // Then: the rail has a title but no bordered block title.
    assert!(text.contains("GUI"), "sidebar keeps a clear title");
    assert!(
        !text.contains("│ context"),
        "sidebar should be a tonal column, not a bordered block"
    );
}

#[test]
fn sidebar_matches_opencode_context_rail_sections() {
    let mut state = AppState {
        model: "kimi-k2".to_string(),
        session_label: "sess-1".to_string(),
        reasoning_effort: Some("max".to_string()),
        ..AppState::default()
    };
    with_session(&mut state, "/tmp/yaca");
    with_text_message(&mut state, Role::Assistant, "context rail parity");

    // Given: a session with transcript content and worktree context.
    // When: the wide context rail renders.
    let text = render(&mut state, 120, 42);

    // Then: it exposes the OpenCode-style information groups.
    assert!(text.contains("ContextPilot"));
    assert!(text.contains("session saved"));
    assert!(text.contains("all-time saved"));
    assert!(text.contains("Context"));
    assert!(text.contains("tokens"));
    assert!(text.contains("MCP"));
    assert!(text.contains("none configured Disabled"));
    assert!(text.contains("LSP"));
    assert!(text.contains("LSPs are disabled"));
    assert!(text.contains("Agents"));
    assert!(text.contains("build - active"));
    assert!(text.contains("/tmp/yaca"));
    assert!(text.contains("yaca 0.0.0"));
}
