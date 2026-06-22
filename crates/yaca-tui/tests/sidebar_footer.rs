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
        let mut row = String::new();
        for x in 0..width {
            row.push_str(buffer[(x, y)].symbol());
        }
        if row.contains(needle) {
            return Some(y);
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
    let buffer = render_buffer(&mut state, 120, 36);

    // Then: the worktree footer is anchored near the rail bottom, not after Agents.
    let workdir_row = row_containing(&buffer, 120, 36, "/tmp/yaca-footer").unwrap();
    let branch_row = row_containing(&buffer, 120, 36, "feat/footer").unwrap();
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
