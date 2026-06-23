#[allow(dead_code)]
mod render_support;

use render_support::{render, with_assistant_message};
use yaca_tui::{AppState, ConnectorState, ConnectorView, PermissionPrompt, PermissionPromptStage};

#[test]
fn default_footer_omits_legacy_navigation_hints() {
    // Given: the default OpenCode-style shell is idle.
    let mut state = AppState::default();

    // When: it renders at a width where the composer command hint is visible.
    let text = render(&mut state, 100, 20);

    // Then: command affordance lives in the composer metadata, not a legacy footer row.
    assert!(
        text.contains("ctrl+p commands"),
        "composer metadata should keep command affordance visible"
    );
    for hint in ["PgUp/PgDn scroll", "Tab yolo", "F2 model"] {
        assert!(
            !text.contains(hint),
            "default footer should omit legacy hint {hint:?}, got {text:?}"
        );
    }
}

#[test]
fn idle_composer_metadata_occupies_bottom_row() {
    // Given: the default OpenCode-style shell is idle with no transient footer state.
    let mut state = AppState::default();

    // When: it renders in a compact terminal.
    let text = render(&mut state, 100, 16);
    let bottom_row = text.lines().last().unwrap_or_default();

    // Then: the composer metadata is visually attached to the viewport bottom.
    assert!(
        bottom_row.contains("ctrl+p commands"),
        "bottom row should be composer metadata, got {bottom_row:?} in {text:?}"
    );
}

#[test]
fn default_footer_shows_agent_shortcut_until_usage_exists() {
    // Given: an idle OpenCode-style composer before any usage data exists.
    let mut state = AppState::default();

    // When: it renders at a width where footer shortcuts are visible.
    let text = render(&mut state, 100, 16);
    let bottom_row = text.lines().last().unwrap_or_default();

    // Then: the footer shows agent and command affordances, not placeholder cost.
    assert!(
        bottom_row.contains("tab agents"),
        "OpenCode default footer should expose the agent-cycle shortcut, got {bottom_row:?}"
    );
    assert!(
        bottom_row.contains("ctrl+p commands"),
        "OpenCode default footer should keep command affordance visible, got {bottom_row:?}"
    );
    assert!(
        !bottom_row.contains("cost n/a"),
        "OpenCode default footer should not show placeholder billing before usage exists, got {bottom_row:?}"
    );
}

#[test]
fn footer_renders_project_mcp_without_app_version() {
    // Given: an idle OpenCode-style footer with worktree, branch, and MCP state.
    let mut state = AppState {
        branch_label: Some("feat/footer".to_string()),
        mcp: vec![ConnectorView {
            name: "context7".to_string(),
            state: ConnectorState::Connected,
        }],
        ..AppState::default()
    };
    state.projection.session.workdir = Some("/tmp/yaca-footer".to_string());

    // When: the composer footer renders with enough horizontal space.
    let text = render(&mut state, 118, 16);
    let bottom_row = text.lines().last().unwrap_or_default();

    // Then: runtime context sits on the left while command hints remain on the right.
    assert!(
        bottom_row.contains("/tmp/yaca-footer:feat/footer"),
        "footer should show workdir and branch, got {bottom_row:?}"
    );
    assert!(
        bottom_row.contains("1 MCP"),
        "footer should show connected MCP count, got {bottom_row:?}"
    );
    assert!(
        bottom_row.contains("/status"),
        "footer should expose the OpenCode status command, got {bottom_row:?}"
    );
    assert!(
        !bottom_row.contains("0.0.0"),
        "bottom footer should not duplicate the sidebar app version, got {bottom_row:?}"
    );
    assert!(
        bottom_row.contains("ctrl+p commands"),
        "footer should keep command affordance visible, got {bottom_row:?}"
    );
}

#[test]
fn footer_renders_lsp_status_like_opencode() {
    // Given: a narrow shell where the sidebar is hidden but LSP state is known.
    let mut state = AppState {
        lsp_status: Some("LSPs are disabled".to_string()),
        ..AppState::default()
    };
    state.projection.session.workdir = Some("/tmp/yaca-footer".to_string());

    // When: the composer footer renders the OpenCode status strip.
    let text = render(&mut state, 100, 16);
    let bottom_row = text.lines().last().unwrap_or_default();

    // Then: the footer keeps the LSP counter beside the status command.
    assert!(
        bottom_row.contains("0 LSP"),
        "footer should show the OpenCode LSP counter, got {bottom_row:?}"
    );
    assert!(
        bottom_row.contains("/status"),
        "footer should expose the OpenCode status command with LSP state, got {bottom_row:?}"
    );
}

#[test]
fn footer_renders_permission_count_like_opencode() {
    // Given: an active permission prompt blocks the composer.
    let mut state = AppState {
        permission: Some(PermissionPrompt {
            title: "bash".to_string(),
            detail: "git status".to_string(),
            selected: 0,
            reply: String::new(),
            stage: PermissionPromptStage::Permission,
        }),
        ..AppState::default()
    };

    // When: the footer renders the OpenCode status strip.
    let text = render(&mut state, 100, 16);
    let bottom_row = text.lines().last().unwrap_or_default();

    // Then: permission status uses OpenCode's count label, not the old yaca wording.
    assert!(
        bottom_row.contains("△ 1 Permission"),
        "footer should show the OpenCode permission count, got {bottom_row:?}"
    );
    assert!(
        !bottom_row.contains("awaiting permission"),
        "footer should not show legacy permission copy, got {bottom_row:?}"
    );
}

#[test]
fn transient_footer_shortcuts_use_opencode_key_style() {
    // Given: transient footer states expose keyboard shortcuts.
    let mut scrolled = AppState {
        scroll_back: 3,
        ..AppState::default()
    };
    with_assistant_message(
        &mut scrolled,
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten",
    );
    let mut exit_armed = AppState {
        exit_armed: true,
        ..AppState::default()
    };

    // When: the footer renders each transient state.
    let scroll_text = render(&mut scrolled, 100, 16);
    let exit_text = render(&mut exit_armed, 100, 16);

    // Then: key names use OpenCode's lowercase shortcut style.
    assert!(
        scroll_text.contains("end to return · ctrl+c clear/interrupt"),
        "scroll footer should use OpenCode shortcut casing:\n{scroll_text}"
    );
    assert!(
        exit_text.contains("ctrl+c again to exit · type to cancel"),
        "exit footer should use OpenCode shortcut casing:\n{exit_text}"
    );
    for legacy in ["End to return", "Ctrl-C"] {
        assert!(
            !scroll_text.contains(legacy) && !exit_text.contains(legacy),
            "transient footers should not expose legacy shortcut {legacy:?}"
        );
    }
}
