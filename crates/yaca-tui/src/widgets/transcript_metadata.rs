use crate::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AssistantBlockStatus {
    Completed,
    Streaming,
}

pub(super) fn assistant_metadata_label(state: &AppState, status: AssistantBlockStatus) -> String {
    let agent = if state.agent.is_empty() {
        "build"
    } else {
        state.agent.as_str()
    };
    let model = if state.model.is_empty() {
        "offline"
    } else {
        state.model.as_str()
    };
    let status = match status {
        AssistantBlockStatus::Completed => "completed",
        AssistantBlockStatus::Streaming => "streaming",
    };
    format!("{agent} · {model} · {status}")
}
