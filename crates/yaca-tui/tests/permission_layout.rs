#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use render_support::with_assistant_message;
use yaca_tui::{AppState, PermissionPrompt, PermissionPromptStage};

#[allow(dead_code)]
mod render_support;

#[test]
fn permission_panel_uses_opencode_left_rail_without_box_border() {
    // Given: an active permission prompt rendered over the bottom composer area.
    let mut state = AppState {
        input: "typed prompt".to_string(),
        permission: Some(PermissionPrompt {
            title: "task".to_string(),
            detail: "quick".to_string(),
            selected: 0,
            reply: String::new(),
            stage: PermissionPromptStage::Permission,
        }),
        ..AppState::default()
    };

    // When: the TUI renders at the same size used by tmux visual QA.
    let text = render(&mut state, 100, 30);

    // Then: the permission prompt uses OpenCode's warning rail instead of a boxed dialog.
    assert!(
        text.contains("▏ △ Permission required"),
        "permission panel should start with a warning rail header:\n{text}"
    );
    assert!(
        text.contains("Allow always"),
        "permission panel should use OpenCode's persistent allow label:\n{text}"
    );
    assert!(
        text.contains("Reject"),
        "permission panel should use OpenCode's reject label:\n{text}"
    );
    assert!(
        !text.contains("Allow all task") && !text.contains("Deny"),
        "permission panel should not render legacy yaca option labels:\n{text}"
    );
    assert!(
        text.contains("Esc reject") && !text.contains("Esc deny"),
        "permission hint should use OpenCode's reject wording:\n{text}"
    );
    assert!(
        !text.contains('┌') && !text.contains('└') && !text.contains('─') && !text.contains('│'),
        "permission panel should not render box border glyphs:\n{text}"
    );
    let reply_row = text
        .lines()
        .find(|row| row.contains("reply:"))
        .unwrap_or_else(|| panic!("permission reply row missing:\n{text}"));
    assert!(
        reply_row.starts_with("  ▏ reply:"),
        "permission reply row should own its left gutter:\n{reply_row}"
    );

    assert!(
        text.lines().any(|row| row.starts_with("  ▏ ←/→ select")),
        "permission hint row should stay inside the left rail:\n{text}"
    );
}

#[test]
fn permission_reply_ellipsizes_cjk_without_wrapping_off_rail() {
    // Given: a permission prompt with a reply longer than the panel width.
    let mut state = AppState {
        permission: Some(PermissionPrompt {
            title: "bash".to_string(),
            detail: "quick".to_string(),
            selected: 2,
            reply: "请先列出目录然后解释每一个文件为什么需要删除".to_string(),
            stage: PermissionPromptStage::Permission,
        }),
        ..AppState::default()
    };

    // When: the TUI renders in a compact terminal.
    let text = render(&mut state, 40, 20);

    // Then: the reply stays on the railed reply row instead of wrapping as loose text.
    let reply_row = text
        .lines()
        .find(|row| row.contains("reply:"))
        .unwrap_or_else(|| panic!("permission reply row missing:\n{text}"));
    assert!(
        reply_row.contains('…'),
        "long CJK reply should be ellipsized on the reply row:\n{reply_row}"
    );
    assert!(
        text.lines().all(|row| !row.trim_start().starts_with("后")),
        "reply continuation should not wrap onto an unrailed row:\n{text}"
    );
}

#[test]
fn permission_allow_always_stage_renders_confirm_prompt() {
    // Given: the user selected OpenCode's persistent allow option.
    let mut state = AppState {
        permission: Some(PermissionPrompt {
            title: "bash".to_string(),
            detail: "rm -rf /tmp/x".to_string(),
            selected: 0,
            reply: "not shown while confirming".to_string(),
            stage: PermissionPromptStage::Always,
        }),
        ..AppState::default()
    };

    // When: the confirmation stage renders.
    let text = render(&mut state, 100, 20);

    // Then: yaca mirrors OpenCode's second confirmation prompt.
    assert!(
        text.contains("Always allow"),
        "confirm title renders:\n{text}"
    );
    assert!(text.contains("Confirm"), "confirm option renders:\n{text}");
    assert!(text.contains("Cancel"), "cancel option renders:\n{text}");
    assert!(
        text.contains("Esc cancel"),
        "always stage should cancel back to the permission prompt:\n{text}"
    );
    assert!(
        !text.contains("reply:") && !text.contains("not shown while confirming"),
        "always stage should not keep the reject reply editor visible:\n{text}"
    );
}

#[test]
fn permission_prompt_is_not_duplicated_in_sidebar_runtime() {
    // Given: an active permission prompt on a wide terminal with the context rail visible.
    let mut state = AppState {
        permission: Some(PermissionPrompt {
            title: "bash".to_string(),
            detail: "rm -rf /tmp/x".to_string(),
            selected: 0,
            reply: String::new(),
            stage: PermissionPromptStage::Permission,
        }),
        ..AppState::default()
    };

    // When: the TUI renders the footer permission panel and sidebar together.
    let text = render(&mut state, 124, 36);

    // Then: permission remains a footer panel instead of being repeated as sidebar runtime text.
    assert!(
        text.contains("Permission required"),
        "footer permission panel should render:\n{text}"
    );
    assert!(
        !text.contains("Runtime") && !text.contains("permission bash"),
        "permission prompt should not be duplicated in the sidebar runtime section:\n{text}"
    );
}

#[test]
fn permission_panel_keeps_footer_blocker_status_visible() {
    // Given: an active permission prompt owns the footer body.
    let mut state = AppState {
        permission: Some(PermissionPrompt {
            title: "bash".to_string(),
            detail: "rm -rf /tmp/x".to_string(),
            selected: 0,
            reply: String::new(),
            stage: PermissionPromptStage::Permission,
        }),
        scroll_back: 5,
        ..AppState::default()
    };
    with_assistant_message(
        &mut state,
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\nthirteen\nfourteen\nfifteen\nsixteen",
    );

    // When: the TUI renders the OpenCode-style permission footer.
    let text = render(&mut state, 100, 20);
    let bottom_row = text.lines().last().unwrap_or_default();

    // Then: the statusline remains visible below the permission body.
    assert!(
        bottom_row.contains("awaiting permission"),
        "footer statusline should expose the permission blocker, got {bottom_row:?} in:\n{text}"
    );
    assert!(
        bottom_row.contains("ctrl+p commands"),
        "footer statusline should keep the command hint visible, got {bottom_row:?}"
    );
    assert!(
        !bottom_row.contains("scroll 5"),
        "permission blocker should take precedence over scrollback, got {bottom_row:?}"
    );
}

fn render(state: &mut AppState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| yaca_tui::draw(frame, state)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..height {
        for x in 0..width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}
