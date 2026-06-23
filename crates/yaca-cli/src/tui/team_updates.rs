use yaca_proto::SessionId;
use yaca_tui::AppState;

use crate::team_supervisor::TeamStatusUpdate;

pub(super) fn apply(app: &mut AppState, current_session: SessionId, update: TeamStatusUpdate) {
    if update.parent == current_session {
        app.team = update.members;
    }
}

pub(super) fn clear(app: &mut AppState) {
    app.team.clear();
}

#[cfg(test)]
mod tests {
    use yaca_proto::SessionId;
    use yaca_tui::AppState;

    use super::*;

    #[test]
    fn apply_updates_team_for_current_session() {
        // Given: the active TUI session receives a live team update.
        let session = SessionId::new();
        let mut app = AppState::default();
        let update = TeamStatusUpdate {
            parent: session,
            members: vec![("inspect".to_string(), "running".to_string())],
        };

        // When: the update is folded into the app state.
        apply(&mut app, session, update);

        // Then: the renderer can expose the active subagent hint and Agents card.
        assert_eq!(
            app.team,
            vec![("inspect".to_string(), "running".to_string())]
        );
    }

    #[test]
    fn apply_ignores_updates_for_other_sessions() {
        // Given: a background or previous session publishes a team update.
        let current = SessionId::new();
        let other = SessionId::new();
        let mut app = AppState {
            team: vec![("current".to_string(), "running".to_string())],
            ..AppState::default()
        };
        let update = TeamStatusUpdate {
            parent: other,
            members: vec![("other".to_string(), "running".to_string())],
        };

        // When: the update is folded into the app state.
        apply(&mut app, current, update);

        // Then: the visible TUI keeps the current session team state.
        assert_eq!(
            app.team,
            vec![("current".to_string(), "running".to_string())]
        );
    }

    #[test]
    fn clear_removes_team_state_for_session_transition() {
        // Given: the visible session has an active team.
        let mut app = AppState {
            team: vec![("inspect".to_string(), "running".to_string())],
            ..AppState::default()
        };

        // When: the TUI switches to another session.
        clear(&mut app);

        // Then: no previous-session team state can render in the new session.
        assert!(app.team.is_empty());
    }
}
