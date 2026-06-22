use crate::AppState;

pub(super) fn active_agent_name(state: &AppState) -> &str {
    if state.agent.is_empty() {
        "build"
    } else {
        state.agent.as_str()
    }
}

pub(super) fn active_agent_label(state: &AppState) -> String {
    let agent = active_agent_name(state);
    state
        .team
        .iter()
        .find(|(member, _status)| member == agent)
        .map_or_else(
            || agent.to_string(),
            |(_member, status)| {
                if status.trim().is_empty() {
                    agent.to_string()
                } else {
                    format!("{agent} - {status}")
                }
            },
        )
}
