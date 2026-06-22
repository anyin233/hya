use crate::AppState;

use super::identity::active_agent_label;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AssistantBlockStatus {
    Completed,
    Streaming,
}

pub(super) fn assistant_metadata_label(state: &AppState, status: AssistantBlockStatus) -> String {
    let model = if state.model.is_empty() {
        "offline"
    } else {
        state.model.as_str()
    };
    let status = match status {
        AssistantBlockStatus::Completed => "completed",
        AssistantBlockStatus::Streaming => "streaming",
    };
    format!("▣ {} · {model} · {status}", active_agent_label(state))
}
