use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct LeaderKey {
    armed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaderAction {
    Arm,
    ModelList,
    AgentList,
    SessionList,
    SessionNew,
    SessionCompact,
    SessionExport,
    Cancel,
}

impl LeaderKey {
    pub(super) fn handle(&mut self, key: &KeyEvent) -> Option<LeaderAction> {
        if self.armed {
            self.armed = false;
            return Some(match key.code {
                KeyCode::Char('m') if key.modifiers.is_empty() => LeaderAction::ModelList,
                KeyCode::Char('a') if key.modifiers.is_empty() => LeaderAction::AgentList,
                KeyCode::Char('l') if key.modifiers.is_empty() => LeaderAction::SessionList,
                KeyCode::Char('n') if key.modifiers.is_empty() => LeaderAction::SessionNew,
                KeyCode::Char('c') if key.modifiers.is_empty() => LeaderAction::SessionCompact,
                KeyCode::Char('x') if key.modifiers.is_empty() => LeaderAction::SessionExport,
                KeyCode::Esc => LeaderAction::Cancel,
                _ => LeaderAction::Cancel,
            });
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('x') {
            self.armed = true;
            return Some(LeaderAction::Arm);
        }
        None
    }
}
