use yaca_tui::{AppState, DialogItem};

pub(super) fn active_items(app: &AppState) -> Vec<DialogItem> {
    app.team
        .iter()
        .filter(|(_member, status)| !AppState::is_finished_team_status(status))
        .map(|(member, status)| DialogItem {
            label: member.clone(),
            detail: if status.trim().is_empty() {
                "active".to_string()
            } else {
                status.clone()
            },
        })
        .collect()
}
