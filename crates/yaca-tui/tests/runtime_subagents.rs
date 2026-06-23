#[allow(dead_code)]
mod render_support;

use ratatui::buffer::Buffer;
use render_support::render_buffer;
use yaca_tui::AppState;

fn row_text(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn runtime_row(buffer: &Buffer, width: u16, height: u16) -> String {
    (0..height)
        .map(|y| row_text(buffer, width, y))
        .find(|row| row.contains("Sisyphus"))
        .unwrap_or_default()
}

#[test]
fn running_status_hides_subagent_hint_without_subagents() {
    // Given: the main assistant turn is running without any subagent tabs.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        running: true,
        ..AppState::default()
    };

    // When: the runtime strip renders above the composer.
    let buffer = render_buffer(&mut state, 120, 16);
    let status_row = runtime_row(&buffer, 120, 16);

    // Then: it does not advertise OpenCode's subagent viewer when no subagent exists.
    assert!(
        !status_row.contains("view subagents"),
        "subagent hint should be hidden without active subagents, got {status_row:?}"
    );
}

#[test]
fn running_status_hides_subagent_hint_when_only_finished_subagents_remain() {
    for status in ["done", "failed", "completed"] {
        // Given: a prior subagent has finished and no active subagent tab remains.
        let mut state = AppState {
            agent: "sisyphus".to_string(),
            model: "kimi-k2".to_string(),
            running: true,
            team: vec![("explore".to_string(), status.to_string())],
            ..AppState::default()
        };

        // When: the runtime strip renders above the composer.
        let buffer = render_buffer(&mut state, 120, 16);
        let status_row = runtime_row(&buffer, 120, 16);

        // Then: it follows OpenCode and hides the subagent shortcut for finished-only state.
        assert!(
            !status_row.contains("view subagents"),
            "finished-only status {status:?} should not keep the viewer hint visible, got {status_row:?}"
        );
    }
}

#[test]
fn running_status_shows_subagent_hint_when_finished_and_running_subagents_are_mixed() {
    // Given: an old subagent has finished while another subagent is still active.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        running: true,
        team: vec![
            ("explore".to_string(), "done".to_string()),
            ("review".to_string(), "running".to_string()),
        ],
        ..AppState::default()
    };

    // When: the runtime strip renders above the composer.
    let buffer = render_buffer(&mut state, 120, 16);
    let status_row = runtime_row(&buffer, 120, 16);

    // Then: the active subagent keeps OpenCode's viewer shortcut visible.
    assert!(
        status_row.contains("ctrl+x down view subagents"),
        "mixed finished/running subagents should keep the viewer hint visible, got {status_row:?}"
    );
}
