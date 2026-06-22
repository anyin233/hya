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
            || titlecase(agent),
            |(_member, status)| {
                if status.trim().is_empty() {
                    titlecase(agent)
                } else {
                    format!("{} - {}", titlecase(agent), titlecase(status))
                }
            },
        )
}

fn titlecase(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut word_start = true;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if word_start {
                output.extend(ch.to_uppercase());
            } else {
                output.push(ch);
            }
            word_start = false;
        } else {
            output.push(ch);
            word_start = true;
        }
    }

    output
}
