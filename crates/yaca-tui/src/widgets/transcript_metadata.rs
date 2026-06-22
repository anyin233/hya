use crate::AppState;

pub(super) fn assistant_metadata_label(state: &AppState) -> String {
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
    let status = if state.running {
        "streaming"
    } else {
        "completed"
    };
    format!("{agent} · {model} · {status}")
}
