use yaca_tui::DialogItem;

#[must_use]
pub fn previous_agent_label(current: &str, agents: &[DialogItem]) -> Option<String> {
    if agents.len() <= 1 {
        return None;
    }
    let previous = agents
        .iter()
        .position(|agent| agent.label == current)
        .map_or(0, |index| index.checked_sub(1).unwrap_or(agents.len() - 1));
    agents.get(previous).map(|agent| agent.label.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents(labels: &[&str]) -> Vec<DialogItem> {
        labels
            .iter()
            .map(|label| DialogItem {
                label: (*label).to_string(),
                detail: String::new(),
            })
            .collect()
    }

    #[test]
    fn previous_agent_label_moves_backward_and_wraps_to_last_agent() {
        let agents = agents(&["build", "plan", "review"]);

        assert_eq!(
            previous_agent_label("review", &agents),
            Some("plan".to_string())
        );
        assert_eq!(
            previous_agent_label("plan", &agents),
            Some("build".to_string())
        );
        assert_eq!(
            previous_agent_label("build", &agents),
            Some("review".to_string())
        );
    }

    #[test]
    fn previous_agent_label_returns_none_when_agent_cannot_cycle() {
        let agents = agents(&["build"]);

        assert_eq!(previous_agent_label("build", &agents), None);
    }
}
