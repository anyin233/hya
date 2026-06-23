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
fn completion_dialogs_omit_extra_subtitle_chrome() {
    // Given: inline completion popups use the compact OpenCode list shell.
    let cases = [
        ("commands", "select a slash command", "/model"),
        ("references", "select a file or reference", "@README.md"),
    ];
    for (title, subtitle, label) in cases {
        let mut state = AppState {
            dialog: Some(DialogView {
                title: title.to_string(),
                subtitle: subtitle.to_string(),
                items: vec![DialogItem {
                    label: label.to_string(),
                    detail: "file".to_string(),
                }],
                selected: 0,
            }),
            ..AppState::default()
        };

        // When: the completion popup renders.
        let text = render(&mut state, 100, 24);

        // Then: it keeps the title and footer hints without adding subtitle chrome.
        assert!(text.contains(title), "dialog title should render:\n{text}");
        assert!(
            !text.contains(subtitle),
            "completion dialog should not render extra subtitle chrome:\n{text}"
        );
        assert!(
            text.contains("↑↓/tab select   enter confirm   esc dismiss"),
            "completion dialog should keep footer hints:\n{text}"
        );
    }
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

#[test]
fn skills_dialog_shows_empty_state_copy_when_no_skills() {
    // Given: the skills chooser has no rows to render.
    let mut state = AppState {
        dialog: Some(DialogView {
            title: "Skills".to_string(),
            subtitle: "Search skills...".to_string(),
            items: Vec::new(),
            selected: 0,
        }),
        ..AppState::default()
    };

    // When: the dialog renders.
    let text = render(&mut state, 100, 24);

    // Then: it shows an intentional empty state instead of a selectable fake row.
    assert!(
        text.contains("No skills found"),
        "missing empty title:\n{text}"
    );
    assert!(
        text.contains("Add SKILL.md under .yaca/skills or ~/.config/yaca/skills"),
        "missing setup hint:\n{text}"
    );
    assert!(
        !text.contains("no skills"),
        "dialog should not expose a fake selectable row:\n{text}"
    );
    assert!(
        text.contains("↑↓/tab select   enter confirm   esc dismiss"),
        "empty dialog should keep the command hint visible:\n{text}"
    );
}

#[test]
fn dialog_window_follows_selected_item_past_first_ten_rows() {
    // Given: a skill dialog with more rows than the visible OpenCode popup window.
    let items = (0..12)
        .map(|idx| DialogItem {
            label: format!("skill-{idx:02}"),
            detail: format!("skill detail {idx:02}"),
        })
        .collect();
    let mut state = AppState {
        dialog: Some(DialogView {
            title: "Skills".to_string(),
            subtitle: "Search skills...".to_string(),
            items,
            selected: 11,
        }),
        ..AppState::default()
    };

    // When: the selected skill is beyond the first ten rows.
    let text = render(&mut state, 100, 32);

    // Then: the rendered dialog window follows selection instead of confirming an invisible row.
    assert!(
        text.contains("skill-11"),
        "selected skill row should stay visible:\n{text}"
    );
    assert!(
        !text.contains("skill-00"),
        "dialog should scroll away from the first row when selection is at the bottom:\n{text}"
    );
}

#[test]
fn dialog_keeps_composer_and_footer_visible_on_short_terminals() {
    // Given: a tall command dialog on a short terminal.
    let items = (0..12)
        .map(|idx| DialogItem {
            label: format!("/command-{idx:02}"),
            detail: "Suggested · leader s · Run command".to_string(),
        })
        .collect();
    let mut state = AppState {
        dialog: Some(DialogView {
            title: "commands".to_string(),
            subtitle: "select a command; enter runs".to_string(),
            items,
            selected: 11,
        }),
        ..AppState::default()
    };

    // When: the dialog renders where the composer occupies a meaningful share of height.
    let text = render(&mut state, 100, 12);

    // Then: the overlay is constrained above the composer/footer instead of clearing them.
    assert!(
        text.contains("Ask anything"),
        "composer placeholder should remain visible below dialog:\n{text}"
    );
    assert!(
        text.contains("ctrl+p commands"),
        "footer shortcuts should remain visible below dialog:\n{text}"
    );
}
