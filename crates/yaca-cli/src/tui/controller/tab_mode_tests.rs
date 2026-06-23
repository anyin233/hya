use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tui::{AppState, DialogItem};

use super::{Controller, TuiEffect};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn shift_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

fn agents(labels: &[&str]) -> Vec<DialogItem> {
    labels
        .iter()
        .map(|label| DialogItem {
            label: (*label).to_string(),
            detail: "agent profile".to_string(),
        })
        .collect()
}

#[test]
fn tab_toggles_yolo_without_cycling_agents_when_no_popup_is_active() {
    // Given: the composer is idle with multiple agent profiles available.
    let mut controller = Controller::new(AppState {
        agent: "build".to_string(),
        ..AppState::default()
    });
    controller.set_agents(agents(&["build", "plan", "review"]));

    // When: the user presses Tab outside completion popups.
    assert_eq!(controller.handle_key(key(KeyCode::Tab)), TuiEffect::None);

    // Then: OpenCode-style max/yolo mode toggles without changing agent identity.
    assert!(controller.app.yolo);
    assert_eq!(controller.app.agent, "build");

    // When: the user presses Tab again.
    assert_eq!(controller.handle_key(key(KeyCode::Tab)), TuiEffect::None);

    // Then: yolo toggles back off and the active agent is still stable.
    assert!(!controller.app.yolo);
    assert_eq!(controller.app.agent, "build");
}

#[test]
fn shift_tab_and_backtab_still_cycle_agents_without_toggling_yolo() {
    // Given: agent cycling remains available through the reverse-tab chord.
    let mut controller = Controller::new(AppState {
        agent: "build".to_string(),
        ..AppState::default()
    });
    controller.set_agents(agents(&["build", "plan", "review"]));

    // When / Then: BackTab cycles agents without touching yolo state.
    assert_eq!(
        controller.handle_key(key(KeyCode::BackTab)),
        TuiEffect::SelectAgent("review".to_string())
    );
    assert!(!controller.app.yolo);

    // When / Then: terminal Shift+Tab encoding behaves the same.
    assert_eq!(
        controller.handle_key(shift_key(KeyCode::Tab)),
        TuiEffect::SelectAgent("plan".to_string())
    );
    assert!(!controller.app.yolo);
}
