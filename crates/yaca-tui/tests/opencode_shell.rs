#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::{AppState, ContextView, draw};

fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal.backend().buffer().clone()
}

fn rendered_row(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
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

#[test]
fn main_stream_starts_at_top_without_status_banner() {
    // Given: a narrow OpenCode-style shell with one assistant block.
    let mut state = AppState {
        agent: "build".to_string(),
        model: "fake".to_string(),
        session_label: "sess-1".to_string(),
        ..AppState::default()
    };
    with_text_message(&mut state, Role::Assistant, "top stream");

    // When: the shell renders without the wide context rail.
    let buffer = render_buffer(&mut state, 80, 16);
    let first_row = rendered_row(&buffer, 80, 0);

    // Then: the stream owns the first row instead of an extra status banner.
    assert!(
        first_row.contains("yaca #1"),
        "first row should be the selected transcript stream, got {first_row:?}"
    );
    assert!(
        !first_row.contains(" · build · fake · sess-1"),
        "OpenCode shell should not reserve a top status banner"
    );
}

#[test]
fn composer_metadata_includes_context_usage_before_cost() {
    // Given: the runtime has OpenCode-style context and billing metadata.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        reasoning_effort: Some("max".to_string()),
        cost_label: Some("$3.14".to_string()),
        context: ContextView {
            current_tokens: Some(187_750),
            context_window_tokens: Some(988_000),
            ..ContextView::default()
        },
        ..AppState::default()
    };

    // When: the composer renders in the OpenCode shell.
    let buffer = render_buffer(&mut state, 120, 16);
    let metadata_row = rendered_row(&buffer, 120, 13);

    // Then: token usage appears before billing and command affordances.
    assert!(
        metadata_row.contains("187.8K (19%)"),
        "composer metadata should show compact context usage, got {metadata_row:?}"
    );
    assert!(
        metadata_row.contains("$3.14"),
        "composer metadata should keep billing visible"
    );
    assert!(
        metadata_row.contains("ctrl+p commands"),
        "composer metadata should keep command affordance visible"
    );
}

#[test]
fn composer_metadata_anchors_commands_to_main_column_edge() {
    // Given: a wide OpenCode-style shell with the right context rail visible.
    let mut state = AppState {
        agent: "build".to_string(),
        model: "mini".to_string(),
        reasoning_effort: Some("off".to_string()),
        cost_label: Some("$0".to_string()),
        ..AppState::default()
    };

    // When: the composer metadata row is rendered in the main output column.
    let buffer = render_buffer(&mut state, 120, 16);
    let metadata_row = rendered_row(&buffer, 120, 13);
    let main_column: String = metadata_row.chars().take(82).collect();

    // Then: the command affordance is anchored to the main column's right edge.
    assert!(
        main_column.ends_with("ctrl+p commands"),
        "composer commands should end at the main column edge, got {main_column:?}"
    );
}

#[test]
fn active_runtime_strip_sits_above_the_composer() {
    // Given: a running OpenCode-style shell with a selected agent and model.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        running: true,
        ..AppState::default()
    };

    // When: the wide shell renders the grounded composer.
    let buffer = render_buffer(&mut state, 120, 16);
    let status_row = rendered_row(&buffer, 120, 11);
    let prompt_row = rendered_row(&buffer, 120, 12);

    // Then: the active runtime strip is visible directly above the input row.
    assert!(
        status_row.contains("sisyphus"),
        "runtime strip should show the active agent, got {status_row:?}"
    );
    assert!(
        status_row.contains("kimi-k2"),
        "runtime strip should show the active model, got {status_row:?}"
    );
    assert!(
        status_row.contains("streaming"),
        "runtime strip should expose the running state, got {status_row:?}"
    );
    assert!(
        prompt_row.starts_with("▌"),
        "composer input rail should remain directly below the runtime strip, got {prompt_row:?}"
    );
}

#[test]
fn active_runtime_strip_includes_current_team_role() {
    // Given: the active agent also has an OpenCode-style team role.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        running: true,
        team: vec![("sisyphus".to_string(), "ultraworker retry".to_string())],
        ..AppState::default()
    };

    // When: the wide shell renders the runtime strip above the composer.
    let buffer = render_buffer(&mut state, 120, 16);
    let status_row = rendered_row(&buffer, 120, 11);

    // Then: the strip uses the active agent plus role before the model.
    assert!(
        status_row.contains("sisyphus - ultraworker retry"),
        "runtime strip should show the active agent role, got {status_row:?}"
    );
    assert!(
        status_row.contains("kimi-k2"),
        "runtime strip should keep the active model visible, got {status_row:?}"
    );
}

#[test]
fn context_rail_title_uses_session_label_when_available() {
    // Given: the wide context rail has an OpenCode-style session title.
    let mut state = AppState {
        session_label: "borderless input parity".to_string(),
        ..AppState::default()
    };

    // When: the shell renders with the right context rail visible.
    let buffer = render_buffer(&mut state, 120, 16);
    let title_row = rendered_row(&buffer, 120, 0);

    // Then: the rail title keeps the GUI prefix but uses the session label.
    assert!(
        title_row.contains("GUI borderless input parity"),
        "context rail should show the active session title, got {title_row:?}"
    );
}
