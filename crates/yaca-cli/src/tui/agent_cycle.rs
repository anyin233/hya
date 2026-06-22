use yaca_tui::DialogItem;

#[must_use]
pub fn next_agent_label(current: &str, agents: &[DialogItem]) -> Option<String> {
    if agents.len() <= 1 {
        return None;
    }
    let next = agents
        .iter()
        .position(|agent| agent.label == current)
        .map_or(0, |index| (index + 1) % agents.len());
    agents.get(next).map(|agent| agent.label.clone())
}
