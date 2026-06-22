#![allow(clippy::expect_used)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tui::{AppState, DialogItem};

use super::{Controller, SessionSummary, TuiEffect};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn ctrl(code: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
}

fn arm_leader(controller: &mut Controller) {
    assert_eq!(controller.handle_key(ctrl('x')), TuiEffect::None);
}

#[test]
fn ctrl_x_then_m_opens_model_dialog() {
    // Given
    let mut controller =
        Controller::with_models(AppState::default(), vec!["alpha".into(), "beta".into()]);

    // When
    arm_leader(&mut controller);
    let effect = controller.handle_key(key(KeyCode::Char('m')));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(
        controller.app.dialog.as_ref().expect("model dialog").title,
        "select model"
    );
}

#[test]
fn ctrl_x_then_a_opens_agent_dialog() {
    // Given
    let mut controller = Controller::new(AppState::default());
    controller.set_agents(vec![DialogItem {
        label: "plan".to_string(),
        detail: "Plan before editing".to_string(),
    }]);

    // When
    arm_leader(&mut controller);
    let effect = controller.handle_key(key(KeyCode::Char('a')));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(
        controller.app.dialog.as_ref().expect("agent dialog").title,
        "agents"
    );
}

#[test]
fn ctrl_x_then_l_opens_resume_dialog() {
    // Given
    let mut controller = Controller::with_sessions(
        AppState::default(),
        vec![SessionSummary {
            id: "sess-1".to_string(),
            title: "Earlier task".to_string(),
            detail: "fake - just now".to_string(),
        }],
    );

    // When
    arm_leader(&mut controller);
    let effect = controller.handle_key(key(KeyCode::Char('l')));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(
        controller.app.dialog.as_ref().expect("resume dialog").title,
        "resume session"
    );
}

#[test]
fn ctrl_x_then_n_requests_new_session() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    arm_leader(&mut controller);
    let effect = controller.handle_key(key(KeyCode::Char('n')));

    // Then
    assert_eq!(effect, TuiEffect::NewSession);
}

#[test]
fn ctrl_x_then_c_requests_compact_transcript() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    arm_leader(&mut controller);
    let effect = controller.handle_key(key(KeyCode::Char('c')));

    // Then
    assert_eq!(effect, TuiEffect::CompactTranscript);
}

#[test]
fn ctrl_x_then_s_opens_status_dialog() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    arm_leader(&mut controller);
    let effect = controller.handle_key(key(KeyCode::Char('s')));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(
        controller.app.dialog.as_ref().expect("status dialog").title,
        "tools"
    );
}

#[test]
fn ctrl_x_then_x_requests_export_transcript() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    arm_leader(&mut controller);
    let effect = controller.handle_key(key(KeyCode::Char('x')));

    // Then
    assert_eq!(effect, TuiEffect::ExportTranscript);
}

#[test]
fn ctrl_x_then_q_requests_exit_without_typing() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    arm_leader(&mut controller);
    let effect = controller.handle_key(key(KeyCode::Char('q')));

    // Then
    assert_eq!(effect, TuiEffect::Exit);
    assert_eq!(controller.app.input, "");
}

#[test]
fn unknown_leader_key_cancels_without_typing_and_is_one_shot() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    arm_leader(&mut controller);
    let unknown_effect = controller.handle_key(key(KeyCode::Char('z')));
    let normal_effect = controller.handle_key(key(KeyCode::Char('m')));

    // Then
    assert_eq!(unknown_effect, TuiEffect::None);
    assert_eq!(normal_effect, TuiEffect::None);
    assert_eq!(controller.app.input, "m");
    assert!(controller.app.dialog.is_none());
}

#[test]
fn escape_cancels_leader_without_typing_and_is_one_shot() {
    // Given
    let mut controller = Controller::new(AppState::default());

    // When
    arm_leader(&mut controller);
    let escape_effect = controller.handle_key(key(KeyCode::Esc));
    let normal_effect = controller.handle_key(key(KeyCode::Char('m')));

    // Then
    assert_eq!(escape_effect, TuiEffect::None);
    assert_eq!(normal_effect, TuiEffect::None);
    assert_eq!(controller.app.input, "m");
    assert!(controller.app.dialog.is_none());
}

#[test]
fn dialog_key_handling_keeps_priority_over_ctrl_x_leader() {
    // Given
    let mut controller =
        Controller::with_models(AppState::default(), vec!["alpha".into(), "beta".into()]);
    assert_eq!(controller.handle_key(key(KeyCode::F(2))), TuiEffect::None);

    // When
    let leader_attempt = controller.handle_key(ctrl('x'));
    let second_key = controller.handle_key(key(KeyCode::Char('m')));

    // Then
    assert_eq!(leader_attempt, TuiEffect::None);
    assert_eq!(second_key, TuiEffect::None);
    assert_eq!(
        controller.app.dialog.as_ref().expect("model dialog").title,
        "select model"
    );
    assert_eq!(controller.app.input, "");
}
