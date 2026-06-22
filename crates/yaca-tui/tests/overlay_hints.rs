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
