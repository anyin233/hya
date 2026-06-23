#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use unicode_width::UnicodeWidthStr;
use yaca_proto::{Envelope, Event, EventSeq, SessionId};
use yaca_tui::{AppState, draw};

fn render_buffer(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal.backend().buffer().clone()
}

fn row_containing(buffer: &Buffer, width: u16, height: u16, needle: &str) -> Option<u16> {
    for y in 0..height {
        let row = row_text(buffer, width, y);
        if row.contains(needle) {
            return Some(y);
        }
    }
    None
}

fn row_text(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn column_of(row: &str, needle: &str) -> u16 {
    let byte_index = row.find(needle).unwrap();
    u16::try_from(UnicodeWidthStr::width(&row[..byte_index])).unwrap()
}

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
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
fn context_rail_footer_stays_anchored_to_bottom() {
    // Given: a wide OpenCode-style context rail with worktree and branch state.
    let mut state = AppState {
        branch_label: Some("feat/footer".to_string()),
        workspace_workdir: Some("/tmp/yaca-footer".to_string()),
        ..AppState::default()
    };
    with_session(&mut state, "/tmp/yaca-footer");

    // When: the rail renders with extra vertical space.
    let buffer = render_buffer(&mut state, 124, 36);

    // Then: the OpenCode-style worktree and branch footer is one attached line.
    let workdir_row = row_containing(&buffer, 124, 36, "/tmp/yaca-footer").unwrap();
    let branch_row = row_containing(&buffer, 124, 36, "feat/footer").unwrap();
    assert!(
        workdir_row >= 31,
        "worktree footer should be bottom anchored, found at row {workdir_row}"
    );
    assert_eq!(
        workdir_row, branch_row,
        "OpenCode renders worktree and branch as one path:branch footer line"
    );
}

#[test]
fn context_rail_footer_shows_getting_started_until_provider_exists() {
    // Given: OpenCode shows a getting-started footer card before a real provider is configured.
    let mut no_provider = AppState::default();
    with_session(&mut no_provider, "/tmp/yaca-footer");
    let mut with_provider = AppState {
        model_provider_label: Some("OpenAI".to_string()),
        ..AppState::default()
    };
    with_session(&mut with_provider, "/tmp/yaca-footer");

    // When: both sidebar footers render.
    let no_provider_buffer = render_buffer(&mut no_provider, 124, 36);
    let provider_buffer = render_buffer(&mut with_provider, 124, 36);

    // Then: the onboarding card appears only before the provider-backed model is known.
    assert!(row_containing(&no_provider_buffer, 124, 36, "Getting started").is_some());
    assert!(row_containing(&no_provider_buffer, 124, 36, "Connect provider").is_some());
    assert!(row_containing(&no_provider_buffer, 124, 36, "/connect").is_some());
    assert!(row_containing(&provider_buffer, 124, 36, "Getting started").is_none());
}

#[test]
fn context_rail_footer_omits_getting_started_at_threshold_height() {
    // Given: a compact wide context rail without a configured provider.
    let mut state = AppState::default();
    with_session(&mut state, "/tmp/yaca-footer");

    // When: the rail renders at the threshold where onboarding would crowd the footer.
    let buffer = render_buffer(&mut state, 124, 32);

    // Then: yaca keeps the OpenCode footer anchored and omits the onboarding card.
    assert!(row_containing(&buffer, 124, 32, "Getting started").is_none());
    assert_eq!(
        row_containing(&buffer, 124, 32, "/tmp/yaca-footer"),
        Some(30)
    );
    assert_eq!(row_containing(&buffer, 124, 32, "yaca 0.0.0"), Some(31));
}

#[test]
fn context_rail_footer_emphasizes_workdir_name_like_opencode() {
    // Given: a wide context rail with a worktree path and branch.
    let mut state = AppState {
        branch_label: Some("feat/footer".to_string()),
        workspace_workdir: Some("/tmp/yaca-footer".to_string()),
        ..AppState::default()
    };
    with_session(&mut state, "/tmp/yaca-footer");

    // When: the OpenCode-style sidebar footer renders.
    let buffer = render_buffer(&mut state, 124, 36);

    // Then: OpenCode keeps everything before the last slash muted and highlights the leaf.
    let workdir_row = row_containing(&buffer, 124, 36, "/tmp/yaca-footer").unwrap();
    let rendered = row_text(&buffer, 124, workdir_row);
    let parent_x = u16::try_from(rendered.find("yaca-footer:feat/").unwrap()).unwrap();
    let name_x = u16::try_from(rendered.rfind("footer").unwrap()).unwrap();
    assert_eq!(
        buffer[(parent_x, workdir_row)].fg,
        Color::Rgb(128, 128, 128)
    );
    assert_eq!(buffer[(name_x, workdir_row)].fg, Color::Rgb(238, 238, 238));
}

#[test]
fn context_rail_footer_emphasizes_app_name_like_opencode() {
    // Given: a wide context rail with the sidebar footer visible.
    let mut state = AppState::default();
    with_session(&mut state, "/tmp/yaca-footer");

    // When: the OpenCode-style version footer renders.
    let buffer = render_buffer(&mut state, 124, 36);

    // Then: the app label is primary text while the version stays muted.
    let version_row = row_containing(&buffer, 124, 36, "yaca 0.0.0").unwrap();
    let rendered = row_text(&buffer, 124, version_row);
    let label_x = column_of(&rendered, "yaca");
    let version_x = column_of(&rendered, "0.0.0");
    assert_eq!(buffer[(label_x, version_row)].fg, Color::Rgb(238, 238, 238));
    assert_eq!(
        buffer[(version_x, version_row)].fg,
        Color::Rgb(128, 128, 128)
    );
}

#[test]
fn context_rail_footer_marks_workdir_as_context_prefix() {
    // Given: a wide context rail with a known worktree path.
    let mut state = AppState::default();
    with_session(&mut state, "/tmp/yaca-footer");

    // When: the OpenCode-style sidebar footer renders.
    let buffer = render_buffer(&mut state, 124, 36);

    // Then: the branchless OpenCode footer renders the bare worktree path.
    let workdir_row = row_containing(&buffer, 124, 36, "/tmp/yaca-footer").unwrap();
    let rendered = row_text(&buffer, 124, workdir_row);
    assert!(
        !rendered.contains("/tmp/yaca-footer:"),
        "branchless worktree footer should not append a colon, got {rendered:?}"
    );
}

#[test]
fn context_rail_footer_omits_branch_for_external_session_workdir() {
    // Given: OpenCode has a branch for the active workspace but renders another session directory.
    let mut state = AppState {
        branch_label: Some("feat/footer".to_string()),
        workspace_workdir: Some("/tmp/yaca-workspace".to_string()),
        ..AppState::default()
    };
    with_session(&mut state, "/tmp/yaca-external-session");

    // When: the wide context rail renders the sidebar footer.
    let buffer = render_buffer(&mut state, 124, 36);

    // Then: the external session path is shown without the active workspace branch suffix.
    let workdir_row = row_containing(&buffer, 124, 36, "/tmp/yaca-external-session").unwrap();
    let rendered = row_text(&buffer, 124, workdir_row);
    assert!(
        !rendered.contains("feat/footer"),
        "external session footer should not append the active workspace branch, got {rendered:?}"
    );
}

#[test]
fn context_rail_footer_keeps_long_workdir_tail_and_colon_visible() {
    // Given: the current worktree path is longer than the visible sidebar width.
    let mut state = AppState::default();
    with_session(
        &mut state,
        "/chivier-disk/yanweiye/.config/superpowers/worktrees/yaca/opencode-gui-parity",
    );

    // When: the wide OpenCode-style context rail renders.
    let buffer = render_buffer(&mut state, 124, 36);

    // Then: the footer preserves the project tail without adding a branch separator.
    let version_row = row_containing(&buffer, 124, 36, "yaca 0.0.0").unwrap();
    let rendered = row_text(&buffer, 124, version_row.saturating_sub(1));
    assert!(
        rendered.contains("opencode-gui-parity"),
        "long worktree footer should keep the project tail visible, got {rendered:?}"
    );
    assert!(
        !rendered.contains("opencode-gui-parity:"),
        "branchless long worktree footer should not append a colon, got {rendered:?}"
    );
}
