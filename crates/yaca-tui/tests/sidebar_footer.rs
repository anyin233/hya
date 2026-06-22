#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
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
        ..AppState::default()
    };
    with_session(&mut state, "/tmp/yaca-footer");

    // When: the rail renders with extra vertical space.
    let buffer = render_buffer(&mut state, 124, 36);

    // Then: the worktree footer is anchored near the rail bottom, not after Agents.
    let workdir_row = row_containing(&buffer, 124, 36, "/tmp/yaca-footer").unwrap();
    let branch_row = row_containing(&buffer, 124, 36, "feat/footer").unwrap();
    assert!(
        workdir_row >= 31,
        "worktree footer should be bottom anchored, found at row {workdir_row}"
    );
    assert_eq!(
        branch_row,
        workdir_row + 1,
        "branch row should remain attached to the worktree footer"
    );
}

#[test]
fn context_rail_footer_marks_workdir_as_context_prefix() {
    // Given: a wide context rail with a known worktree path.
    let mut state = AppState::default();
    with_session(&mut state, "/tmp/yaca-footer");

    // When: the OpenCode-style sidebar footer renders.
    let buffer = render_buffer(&mut state, 124, 36);

    // Then: the worktree row reads like a context prefix, not a bare path.
    let workdir_row = row_containing(&buffer, 124, 36, "/tmp/yaca-footer").unwrap();
    let rendered = row_text(&buffer, 124, workdir_row);
    assert!(
        rendered.contains("/tmp/yaca-footer:"),
        "worktree footer should include an OpenCode-style trailing colon, got {rendered:?}"
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

    // Then: the footer preserves the project tail and trailing context colon.
    let version_row = row_containing(&buffer, 124, 36, "yaca 0.0.0").unwrap();
    let rendered = row_text(&buffer, 124, version_row.saturating_sub(1));
    assert!(
        rendered.contains("opencode-gui-parity:"),
        "long worktree footer should keep the tail and colon visible, got {rendered:?}"
    );
}
