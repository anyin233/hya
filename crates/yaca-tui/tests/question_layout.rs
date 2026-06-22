#[allow(dead_code)]
mod render_support;

use render_support::{render, render_buffer, with_assistant_message};
use yaca_tui::{AppState, QuestionPrompt};

fn row_index(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("missing {needle:?} in:\n{text}"))
}

#[test]
fn question_panel_keeps_footer_blocker_status_visible() {
    // Given: an active question prompt owns the footer body while scrollback exists.
    let mut state = AppState {
        question: Some(QuestionPrompt {
            prompt: "continue?".to_string(),
            options: vec!["yes".to_string(), "no".to_string()],
            selected: 0,
            input: String::new(),
            allow_custom: false,
        }),
        scroll_back: 5,
        ..AppState::default()
    };
    with_assistant_message(
        &mut state,
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\nthirteen\nfourteen\nfifteen\nsixteen",
    );

    // When: the TUI renders the OpenCode-style question footer.
    let text = render(&mut state, 100, 24);
    let footer_y = text.lines().count().saturating_sub(1);
    let question_y = row_index(&text, "continue?");
    let bottom_row = text.lines().last().unwrap_or_default();

    // Then: the question body is attached to the bottom footer and the
    // statusline exposes the blocker instead of scrollback.
    assert!(
        question_y >= footer_y.saturating_sub(10) && question_y < footer_y,
        "question prompt should render as a footer body, got row {question_y} of {footer_y}:\n{text}"
    );
    assert!(
        bottom_row.contains("awaiting answer"),
        "footer statusline should expose the question blocker, got {bottom_row:?} in:\n{text}"
    );
    assert!(
        bottom_row.contains("ctrl+p commands"),
        "footer statusline should keep the command hint visible, got {bottom_row:?}"
    );
    assert!(
        !bottom_row.contains("scroll 5"),
        "question blocker should take precedence over scrollback, got {bottom_row:?}"
    );
}

#[test]
fn question_panel_keeps_footer_row_background_separate() {
    // Given: a question prompt replaces the composer body.
    let mut state = AppState {
        question: Some(QuestionPrompt {
            prompt: "Pick a mode".to_string(),
            options: vec!["fast".to_string(), "careful".to_string()],
            selected: 1,
            input: String::new(),
            allow_custom: false,
        }),
        ..AppState::default()
    };

    // When: the terminal buffer renders with a footer statusline.
    let width = 100;
    let height = 20;
    let buffer = render_buffer(&mut state, width, height);

    // Then: the blocker body does not overwrite the bottom footer row.
    assert_ne!(buffer[(2, height - 2)].bg, buffer[(2, height - 1)].bg);
}
