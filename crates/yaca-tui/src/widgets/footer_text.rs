use crate::{AppState, ConnectorState};

use super::sidebar_format::workdir_label;

const SIDEBAR_BREAKPOINT: u16 = 120;

pub(super) fn footer_left_text(state: &AppState, width: u16) -> String {
    if state.scroll_back > 0 {
        return format!(
            "scroll {} · End to return · Ctrl-C clear/interrupt",
            state.scroll_back
        );
    }
    if state.exit_armed {
        return "Ctrl-C again to exit · type to cancel".to_string();
    }
    if state.yolo {
        return "YOLO mode · /yolo disables auto-allow · / commands · @ references".to_string();
    }
    if state.goal.is_some() || state.loop_view.is_some() {
        return runtime_footer_text(state);
    }
    default_footer_text(state, width)
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
    if let Some(label) = mcp_label(state) {
        segments.push(label);
    }
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
    Some(format!("⊙ {connected} MCP /status"))
}
