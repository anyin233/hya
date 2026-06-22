use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const LEADER_TIMEOUT: Duration = Duration::from_millis(2_000);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct LeaderKey {
    armed_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaderAction {
    Arm,
    ModelList,
    AgentList,
    SessionList,
    SessionNew,
    SessionCompact,
    StatusView,
    SessionExport,
    Exit,
    Cancel,
}

impl LeaderKey {
    pub(super) fn handle(&mut self, key: &KeyEvent) -> Option<LeaderAction> {
        self.handle_at(key, Instant::now())
    }

    fn handle_at(&mut self, key: &KeyEvent, now: Instant) -> Option<LeaderAction> {
        if let Some(armed_at) = self.armed_at {
            if now.saturating_duration_since(armed_at) <= LEADER_TIMEOUT {
                self.armed_at = None;
                return Some(match key.code {
                    KeyCode::Char('m') if key.modifiers.is_empty() => LeaderAction::ModelList,
                    KeyCode::Char('a') if key.modifiers.is_empty() => LeaderAction::AgentList,
                    KeyCode::Char('l') if key.modifiers.is_empty() => LeaderAction::SessionList,
                    KeyCode::Char('n') if key.modifiers.is_empty() => LeaderAction::SessionNew,
                    KeyCode::Char('c') if key.modifiers.is_empty() => LeaderAction::SessionCompact,
                    KeyCode::Char('s') if key.modifiers.is_empty() => LeaderAction::StatusView,
                    KeyCode::Char('x') if key.modifiers.is_empty() => LeaderAction::SessionExport,
                    KeyCode::Char('q') if key.modifiers.is_empty() => LeaderAction::Exit,
                    KeyCode::Esc => LeaderAction::Cancel,
                    _ => LeaderAction::Cancel,
                });
            }
            self.armed_at = None;
        }

        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('x') {
            self.armed_at = Some(now);
            return Some(LeaderAction::Arm);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn plain(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::empty())
    }

    fn ctrl(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
    }

    #[test]
    fn leader_second_key_dispatches_within_opencode_timeout() {
        // Given
        let start = Instant::now();
        let mut leader = LeaderKey::default();

        // When
        let arm = leader.handle_at(&ctrl('x'), start);
        let action = leader.handle_at(&plain('m'), start + Duration::from_millis(1_999));

        // Then
        assert_eq!(arm, Some(LeaderAction::Arm));
        assert_eq!(action, Some(LeaderAction::ModelList));
    }

    #[test]
    fn leader_timeout_expires_before_second_key_is_processed() {
        // Given
        let start = Instant::now();
        let mut leader = LeaderKey::default();

        // When
        let arm = leader.handle_at(&ctrl('x'), start);
        let expired = leader.handle_at(&plain('m'), start + Duration::from_millis(2_001));

        // Then
        assert_eq!(arm, Some(LeaderAction::Arm));
        assert_eq!(expired, None);
    }
}
