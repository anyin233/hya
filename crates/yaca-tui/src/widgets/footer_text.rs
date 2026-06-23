use crate::{AppState, ConnectorState};

use super::sidebar_format::workdir_label;

const SIDEBAR_BREAKPOINT: u16 = 120;

pub(super) fn footer_left_text(state: &AppState, width: u16) -> String {
    let mode = if state.exit_armed { "EXIT" } else { "BUILD" };
    if state.permission.is_some() {
        return with_mode(mode, "△ 1 Permission");
    }
    if state.question.is_some() {
        return with_mode(mode, default_footer_text(state, width));
    }
    if state.scroll_back > 0 {
        return with_mode(
            mode,
            format!(
                "scroll {} · end to return · ctrl+c clear/interrupt",
                state.scroll_back
            ),
        );
    }
    if state.exit_armed {
        return with_mode(mode, "ctrl+c again to exit · type to cancel");
    }
    if state.yolo {
        return with_mode(
            mode,
            "YOLO mode · /yolo disables auto-allow · / commands · @ references",
        );
    }
    if state.goal.is_some() || state.loop_view.is_some() {
        return with_mode(mode, runtime_footer_text(state));
    }
    with_mode(mode, default_footer_text(state, width))
}

fn with_mode(mode: &str, text: impl Into<String>) -> String {
    format!("{mode} · {}", text.into())
}

fn runtime_footer_text(state: &AppState) -> String {
    let mut segments = Vec::new();
    if let Some(goal) = &state.goal {
        segments.push(format!("GOAL:{} turns {}", goal.condition, goal.turns));
    }
    if let Some(loop_view) = &state.loop_view {
        segments.push(format!(
            "LOOP:{} iter {}/{} score {}",
            loop_view.target, loop_view.iteration, loop_view.budget, loop_view.last_score
        ));
    }
    segments.push("ctrl+p commands".to_string());
    segments.join(" · ")
}

fn default_footer_text(state: &AppState, width: u16) -> String {
    let mut segments = Vec::new();
    if width <= SIDEBAR_BREAKPOINT && state.projection.session.workdir.is_some() {
        segments.push(project_label(state));
    }
    segments.push("• 0 LSP".to_string());
    if let Some(label) = mcp_label(state) {
        segments.push(label);
    }
    segments.push("/status".to_string());
    segments.join(" · ")
}

fn project_label(state: &AppState) -> String {
    let workdir = workdir_label(state.projection.session.workdir.as_deref());
    match state.visible_branch_label() {
        Some(branch) => format!("{workdir}:{branch}"),
        None => workdir,
    }
}

fn mcp_label(state: &AppState) -> Option<String> {
    if state.mcp.is_empty() {
        return None;
    }
    let connected = state
        .mcp
        .iter()
        .filter(|connector| matches!(connector.state, ConnectorState::Connected))
        .count();
    if connected == 0 {
        return None;
    }
    Some(format!("⊙ {connected} MCP"))
}
