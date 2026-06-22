#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};
use yaca_tui::{AppState, ContextView, PromptAttachment, draw};

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

fn find_row_index(buffer: &Buffer, width: u16, height: u16, needle: &str) -> u16 {
    (0..height)
        .find(|&y| rendered_row(buffer, width, y).contains(needle))
        .unwrap()
}

fn find_row(buffer: &Buffer, width: u16, height: u16, needle: &str) -> String {
    let y = find_row_index(buffer, width, height, needle);
    rendered_row(buffer, width, y)
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
fn main_stream_keeps_opencode_scrollbox_top_spacer() {
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
    let second_row = rendered_row(&buffer, 80, 1);

    // Then: the scrollback keeps OpenCode's top spacer without a legacy status banner.
    assert_eq!(
        first_row.trim(),
        "",
        "OpenCode scrollbox keeps a one-row top spacer, got {first_row:?}"
    );
    assert!(
        second_row.contains("top stream"),
        "transcript content should start after the top spacer, got {second_row:?}"
    );
    assert!(
        !second_row.contains("yaca #1"),
        "OpenCode assistant blocks should not render numbered role labels, got {second_row:?}"
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
    let metadata_row = find_row(&buffer, 120, 16, "ctrl+p commands");

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
fn composer_metadata_includes_active_agent_role() {
    // Given: the active agent has an OpenCode-style role label.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        team: vec![("sisyphus".to_string(), "ultraworker retry".to_string())],
        ..AppState::default()
    };

    // When: the prompt identity row renders.
    let buffer = render_buffer(&mut state, 120, 16);
    let metadata_row = find_row(&buffer, 120, 16, "sisyphus - ultraworker retry");

    // Then: the same agent-role identity used by OpenCode appears before the model.
    assert!(
        metadata_row.contains("sisyphus - ultraworker retry · kimi-k2"),
        "composer metadata should show the active agent role, got {metadata_row:?}"
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
    let buffer = render_buffer(&mut state, 124, 16);
    let metadata_row = find_row(&buffer, 124, 16, "ctrl+p commands");
    let main_column: String = metadata_row.chars().take(80).collect();

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
    let status_y = find_row_index(&buffer, 120, 16, "streaming");
    let status_row = rendered_row(&buffer, 120, status_y);
    let padding_row = status_y + 1;
    let prompt_row = rendered_row(&buffer, 120, status_y + 2);

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
        !rendered_row(&buffer, 120, padding_row).contains("▌"),
        "composer should keep OpenCode's top padding row before the input rail"
    );
    assert_eq!(
        buffer[(2, padding_row)].bg,
        Color::Rgb(30, 30, 30),
        "composer top padding should use the input surface"
    );
    assert!(
        prompt_row.starts_with("  ▌"),
        "composer input rail should sit below the input padding row, got {prompt_row:?}"
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
    let status_row = find_row(&buffer, 120, 16, "streaming");

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
    let buffer = render_buffer(&mut state, 124, 16);
    let title_row = find_row(&buffer, 124, 16, "GUI borderless input parity");

    // Then: the rail title keeps the GUI prefix but uses the session label.
    assert!(
        title_row.contains("GUI borderless input parity"),
        "context rail should show the active session title, got {title_row:?}"
    );
}

#[test]
fn composer_renders_prompt_attachment_badges() {
    // Given: the prompt owns an OpenCode-style local image attachment.
    let mut state = AppState {
        attachments: vec![PromptAttachment {
            placeholder: "[Image #1]".to_string(),
            source_path: Some("/tmp/screenshots/CleanShot.png".to_string()),
            mime: "image/png".to_string(),
        }],
        ..AppState::default()
    };

    // When: the grounded composer renders its reserved attachment row.
    let buffer = render_buffer(&mut state, 120, 16);
    let attachment_row = find_row(&buffer, 120, 16, "[Image #1]");

    // Then: the attachment is visible as a compact prompt badge.
    assert!(
        attachment_row.contains("[Image #1]"),
        "composer should expose the attachment placeholder, got {attachment_row:?}"
    );
    assert!(
        attachment_row.contains("CleanShot.png"),
        "composer should show the attachment filename, got {attachment_row:?}"
    );
}
