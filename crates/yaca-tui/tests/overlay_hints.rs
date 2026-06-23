#[allow(dead_code)]
mod render_support;

use render_support::render;
use yaca_tui::{AppState, DialogItem, DialogView, Picker};

#[test]
fn dialog_shortcuts_use_opencode_key_style() {
    // Given: an OpenCode-style command dialog with tab navigation.
    let mut state = AppState {
        dialog: Some(DialogView {
            title: "commands".to_string(),
            subtitle: "choose a command".to_string(),
            items: vec![
                DialogItem {
                    label: "/model".to_string(),
                    detail: "select model".to_string(),
                },
                DialogItem {
                    label: "/agent".to_string(),
                    detail: "select agent".to_string(),
                },
            ],
            selected: 0,
        }),
        ..AppState::default()
    };

    // When: the dialog renders.
    let text = render(&mut state, 100, 24);

    // Then: shortcut copy mirrors OpenCode's lowercase key treatment.
    assert!(
        text.contains("↑↓/tab select   enter confirm   esc dismiss"),
        "dialog should use OpenCode-style shortcut copy:\n{text}"
    );
    assert!(
        !text.contains("Up/Down") && !text.contains("Enter") && !text.contains("Esc cancel"),
        "dialog should not expose legacy shortcut casing:\n{text}"
    );
}

#[test]
fn picker_shortcuts_use_opencode_key_style() {
    // Given: a non-dialog picker overlay.
    let mut state = AppState {
        picker: Some(Picker {
            title: "pick session".to_string(),
            entries: vec!["alpha".to_string(), "beta".to_string()],
            selected: 1,
        }),
        ..AppState::default()
    };

    // When: the picker renders.
    let text = render(&mut state, 100, 24);

    // Then: shortcut copy mirrors OpenCode's lowercase key treatment.
    assert!(
        text.contains("↑↓ select   enter confirm   esc dismiss"),
        "picker should use OpenCode-style shortcut copy:\n{text}"
    );
    assert!(
        !text.contains("Up/Down") && !text.contains("Enter") && !text.contains("Esc cancel"),
        "picker should not expose legacy shortcut casing:\n{text}"
    );
}

#[test]
fn dialog_renders_category_headers_for_command_groups() {
    // Given: command palette rows carry OpenCode-style category prefixes.
    let mut state = AppState {
        dialog: Some(DialogView {
            title: "commands".to_string(),
            subtitle: "select a command; enter runs".to_string(),
            items: vec![
                DialogItem {
                    label: "/model".to_string(),
                    detail: "Suggested · leader m · Select the model".to_string(),
                },
                DialogItem {
                    label: "/new".to_string(),
                    detail: "Session · leader n · Start a new conversation".to_string(),
                },
            ],
            selected: 0,
        }),
        ..AppState::default()
    };

    // When: the dialog renders.
    let text = render(&mut state, 100, 24);

    // Then: categories render as their own readable headers, not row detail.
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("Suggested")),
        "dialog should render a Suggested group header:\n{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("Session")),
        "dialog should render a Session group header:\n{text}"
    );
    assert!(
        !text.contains("/model  Suggested ·"),
        "category prefix should not be repeated in the selected command detail:\n{text}"
    );
}

#[test]
fn dialog_clears_underlying_prompt_rail_at_eighty_columns() {
    // Given: a command dialog tall enough to reach the composer region on a narrow terminal.
    let mut state = AppState {
        dialog: Some(DialogView {
            title: "commands".to_string(),
            subtitle: "select a command; enter runs".to_string(),
            items: vec![
                DialogItem {
                    label: "/model".to_string(),
                    detail: "Suggested · leader m · Select the model".to_string(),
                },
                DialogItem {
                    label: "/new".to_string(),
                    detail: "Suggested · leader n · Start a new conversation".to_string(),
                },
                DialogItem {
                    label: "/agent".to_string(),
                    detail: "Agent · leader a · Select the active agent".to_string(),
                },
                DialogItem {
                    label: "/resume".to_string(),
                    detail: "Session · leader l · Resume a session".to_string(),
                },
                DialogItem {
                    label: "/compact".to_string(),
                    detail: "Context · leader c · Compact context".to_string(),
                },
            ],
            selected: 0,
        }),
        ..AppState::default()
    };

    // When: the dialog renders at the OpenCode no-sidebar boundary.
    let text = render(&mut state, 80, 24);

    // Then: the modal clears the underlying composer rail from its vertical band.
    let hint_renders = text.lines().any(|line| line.contains("↑↓/tab select"));
    let hint_leaks_rail = text
        .lines()
        .any(|line| line.contains("↑↓/tab select") && line.contains('▌'));
    assert!(hint_renders, "dialog hint line should render:\n{text}");
    assert!(
        !hint_leaks_rail,
        "dialog footer hint should not leak the composer rail:\n{text}"
    );
}
