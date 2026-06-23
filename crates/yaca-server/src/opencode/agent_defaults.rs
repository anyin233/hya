use super::agent_catalog::AgentEntry;

pub(super) fn sort(agents: &mut [AgentEntry], configured_default: Option<&str>) {
    agents.sort_by(|left, right| {
        let left_default = is_default(left, configured_default);
        let right_default = is_default(right, configured_default);
        right_default
            .cmp(&left_default)
            .then_with(|| left.name.cmp(&right.name))
    });
}

pub(super) fn selected_name(
    agents: &[AgentEntry],
    configured_default: Option<&str>,
) -> Option<String> {
    selected_default(agents, configured_default).map(|agent| agent.name.clone())
}

fn selected_default<'a>(
    agents: &'a [AgentEntry],
    configured_default: Option<&str>,
) -> Option<&'a AgentEntry> {
    if let Some(name) = configured_default {
        return agents
            .iter()
            .find(|agent| agent.name == name && selectable(agent));
    }
    agents.iter().find(|agent| selectable(agent))
}

fn is_default(agent: &AgentEntry, configured_default: Option<&str>) -> bool {
    match configured_default {
        Some(name) => agent.name == name,
        None => agent.name == "build",
    }
}

fn selectable(agent: &AgentEntry) -> bool {
    agent.mode != "subagent" && !agent.hidden
}
