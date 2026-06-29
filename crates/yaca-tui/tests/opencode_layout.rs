#![allow(clippy::unwrap_used, clippy::field_reassign_with_default)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
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

fn find_rendered_text(
    buffer: &Buffer,
    width: u16,
    height: u16,
    needle: &str,
) -> Option<(u16, u16)> {
    for y in 0..height {
        let mut row = String::new();
        for x in 0..width {
            row.push_str(buffer[(x, y)].symbol());
        }
        if let Some(x) = row.find(needle) {
            return Some((u16::try_from(x).unwrap(), y));
        }
    }
    None
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

fn rich_state() -> AppState {
    let mut state = AppState {
        agent: "build".to_string(),
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        input: "type here".to_string(),
        ..AppState::default()
    };
    with_text_message(&mut state, Role::Assistant, "HELLOTUI");
    state
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
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        input: "ship the tui".to_string(),
        reasoning_effort: Some("max".to_string()),
        cost_label: Some("$3.14".to_string()),
        ..AppState::default()
    };

    let text = render(&mut state, 120, 20);
    assert!(text.contains("ship the tui"), "typed prompt still renders");
    assert!(text.contains("Sisyphus"), "metadata shows current agent");
    assert!(text.contains("kimi-k2"), "metadata shows current model");
    assert!(text.contains("max"), "metadata shows thinking effort");
    assert!(text.contains("$3.14"), "metadata shows billing summary");
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

    let text = render(&mut state, 80, 12);
    for glyph in ["┌", "┐", "└", "┘"] {
        assert!(
            !text.contains(glyph),
            "composer should not draw box border corner {glyph}"
        );
    }
}

#[test]
fn sidebar_uses_tonal_column_without_border_title() {
    let mut state = rich_state();

    let text = render(&mut state, 124, 36);
    assert!(text.contains("GUI"), "sidebar keeps a clear title");
    assert!(
        !text.contains("│ context"),
        "sidebar should be a tonal column, not a bordered block"
    );
}

#[test]
fn sidebar_matches_opencode_context_rail_sections() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        session_label: "sess-1".to_string(),
        reasoning_effort: Some("max".to_string()),
        cost_label: Some("$3.14".to_string()),
        mcp: vec![
            yaca_tui::ConnectorView {
                name: "codegraph".to_string(),
                state: yaca_tui::ConnectorState::Connected,
            },
            yaca_tui::ConnectorView {
                name: "linear-server".to_string(),
                state: yaca_tui::ConnectorState::NeedsAuth,
            },
        ],
        lsp_status: Some("LSPs are disabled".to_string()),
        branch_label: Some("feat/yaca-pi-parity".to_string()),
        ..AppState::default()
    };
    with_session(&mut state, "/tmp/yaca");
    with_text_message(&mut state, Role::Assistant, "context rail parity");

    let text = render(&mut state, 124, 42);
    assert!(text.contains("ContextPilot"));
    assert!(text.contains("session saved"));
    assert!(text.contains("all-time saved"));
    assert!(text.contains("Context"));
    assert!(text.contains("tokens"));
    assert!(text.contains("$3.14 spent"));
    assert!(text.contains("MCP"));
    assert!(text.contains("codegraph Connected"));
    assert!(text.contains("linear-server Needs auth"));
    assert!(text.contains("LSP"));
    assert!(text.contains("LSPs are disabled"));
    assert!(text.contains("Agents"));
    assert!(text.contains("sisyphus - active"));
    assert!(text.contains("/tmp/yaca"));
    assert!(text.contains("feat/yaca-pi-parity"));
    assert!(text.contains("yaca 0.0.0"));
}

#[test]
fn sidebar_expands_dot_workdir_footer() {
    let mut state = AppState::default();
    with_session(&mut state, ".");

    let text = render(&mut state, 120, 36);
    assert!(
        !text.lines().any(|line| line.trim() == "."),
        "worktree footer should not render a bare dot"
    );
}

#[test]
fn selected_stream_block_has_action_hints_and_surface() {
    let mut state = AppState {
        selected_message: Some(0),
        ..AppState::default()
    };
    with_text_message(&mut state, Role::Assistant, "selected assistant block");

    let buffer = render_buffer(&mut state, 120, 24);
    let text = buffer_text(&buffer, 120, 24);
    assert!(
        !text.contains("yaca #1"),
        "OpenCode stream blocks should not render numbered role labels"
    );
    assert!(
        text.contains("r revert · b branch"),
        "selected block action hints should render in the runtime strip"
    );
    let (x, y) = find_rendered_text(&buffer, 120, 24, "selected assistant block").unwrap();
    let (hint_x, hint_y) = find_rendered_text(&buffer, 120, 24, "r revert").unwrap();
    assert_eq!(
        buffer[(x, y)].bg,
        Color::Rgb(24, 48, 58),
        "selected block should use the semantic block surface"
    );
    assert_ne!(
        buffer[(hint_x, hint_y)].bg,
        Color::Rgb(24, 48, 58),
        "selected block action hints should stay outside the selected block surface"
    );
}
