use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tui::{AppState, DialogItem};

use super::{Controller, TuiEffect};

fn ctrl(code: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
}

fn alt(code: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(code), KeyModifiers::ALT)
}

fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn type_text(controller: &mut Controller, text: &str) {
    for ch in text.chars() {
        assert_eq!(
            controller.handle_key(key(KeyCode::Char(ch))),
            TuiEffect::None
        );
    }
}

fn command_completion_controller() -> Controller {
    let mut controller = Controller::new(AppState::default());
    type_text(&mut controller, "/model");
    assert!(controller.app.dialog.is_some());
    controller
}

#[test]
fn ctrl_u_deletes_to_current_line_start() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "first line\nsecond word".to_string(),
        exit_armed: true,
        ..AppState::default()
    });

    // When
    let effect = controller.handle_key(ctrl('u'));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(controller.app.input, "first line\n");
    assert!(!controller.app.exit_armed);
}

#[test]
fn ctrl_k_deletes_to_current_line_end() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "alpha beta\ngamma".to_string(),
        input_cursor: Some("alpha ".len()),
        ..AppState::default()
    });

    // When
    let effect = controller.handle_key(ctrl('k'));
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('!'))),
        TuiEffect::None
    );

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(controller.app.input, "alpha !\ngamma");
    assert_eq!(controller.app.input_cursor, Some("alpha !".len()));
}

#[test]
fn ctrl_w_deletes_previous_word_from_input_end() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "alpha beta   ".to_string(),
        ..AppState::default()
    });

    // When
    let effect = controller.handle_key(ctrl('w'));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(controller.app.input, "alpha ");
}

#[test]
fn input_edit_shortcuts_refresh_completion_popup() {
    // Given
    let mut controller = Controller::new(AppState::default());
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('/'))),
        TuiEffect::None
    );
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('m'))),
        TuiEffect::None
    );
    assert!(controller.app.dialog.is_some());

    // When
    assert_eq!(controller.handle_key(ctrl('u')), TuiEffect::None);

    // Then
    assert_eq!(controller.app.input, "");
    assert!(controller.app.dialog.is_none());
}

#[test]
fn left_arrow_moves_cursor_and_typing_inserts_at_cursor() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "ac".to_string(),
        ..AppState::default()
    });

    // When
    assert_eq!(controller.handle_key(key(KeyCode::Left)), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('b'))),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input, "abc");
}

#[test]
fn delete_removes_character_at_cursor() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "abc".to_string(),
        ..AppState::default()
    });

    // When
    assert_eq!(controller.handle_key(key(KeyCode::Left)), TuiEffect::None);
    assert_eq!(controller.handle_key(key(KeyCode::Left)), TuiEffect::None);
    let effect = controller.handle_key(key(KeyCode::Delete));

    // Then
    assert_eq!(effect, TuiEffect::None);
    assert_eq!(controller.app.input, "ac");
}

#[test]
fn ctrl_a_and_ctrl_e_move_within_current_line() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "first\nseond".to_string(),
        ..AppState::default()
    });

    // When
    assert_eq!(controller.handle_key(ctrl('a')), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('c'))),
        TuiEffect::None
    );
    assert_eq!(controller.handle_key(ctrl('e')), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('!'))),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input, "first\ncseond!");
}

#[test]
fn alt_b_and_alt_f_move_by_words() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "alpha beta gamma".to_string(),
        ..AppState::default()
    });

    // When
    assert_eq!(controller.handle_key(alt('b')), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('!'))),
        TuiEffect::None
    );
    assert_eq!(controller.handle_key(key(KeyCode::Home)), TuiEffect::None);
    assert_eq!(controller.handle_key(alt('f')), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('?'))),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input, "alpha? beta !gamma");
}

#[test]
fn alt_arrow_keys_move_by_words() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "alpha beta gamma".to_string(),
        ..AppState::default()
    });

    // When
    assert_eq!(
        controller.handle_key(modified_key(KeyCode::Left, KeyModifiers::ALT)),
        TuiEffect::None
    );
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('!'))),
        TuiEffect::None
    );
    assert_eq!(controller.handle_key(key(KeyCode::Home)), TuiEffect::None);
    assert_eq!(
        controller.handle_key(modified_key(KeyCode::Right, KeyModifiers::ALT)),
        TuiEffect::None
    );
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('?'))),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input, "alpha? beta !gamma");
}

#[test]
fn ctrl_arrow_keys_move_by_words() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "alpha beta gamma".to_string(),
        ..AppState::default()
    });

    // When
    assert_eq!(
        controller.handle_key(modified_key(KeyCode::Left, KeyModifiers::CONTROL)),
        TuiEffect::None
    );
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('!'))),
        TuiEffect::None
    );
    assert_eq!(controller.handle_key(key(KeyCode::Home)), TuiEffect::None);
    assert_eq!(
        controller.handle_key(modified_key(KeyCode::Right, KeyModifiers::CONTROL)),
        TuiEffect::None
    );
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('?'))),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input, "alpha? beta !gamma");
}

#[test]
fn alt_left_moves_by_word_inside_completion_popup() {
    // Given
    let mut controller = command_completion_controller();

    // When
    assert_eq!(
        controller.handle_key(modified_key(KeyCode::Left, KeyModifiers::ALT)),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input_cursor, Some(0));
}

#[test]
fn ctrl_left_moves_by_word_inside_completion_popup() {
    // Given
    let mut controller = command_completion_controller();

    // When
    assert_eq!(
        controller.handle_key(modified_key(KeyCode::Left, KeyModifiers::CONTROL)),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input_cursor, Some(0));
}

#[test]
fn alt_right_moves_by_word_inside_completion_popup() {
    // Given
    let mut controller = command_completion_controller();
    assert_eq!(controller.handle_key(key(KeyCode::Home)), TuiEffect::None);

    // When
    assert_eq!(
        controller.handle_key(modified_key(KeyCode::Right, KeyModifiers::ALT)),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input_cursor, Some("/model".len()));
}

#[test]
fn ctrl_right_moves_by_word_inside_completion_popup() {
    // Given
    let mut controller = command_completion_controller();
    assert_eq!(controller.handle_key(key(KeyCode::Home)), TuiEffect::None);

    // When
    assert_eq!(
        controller.handle_key(modified_key(KeyCode::Right, KeyModifiers::CONTROL)),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input_cursor, Some("/model".len()));
}

#[test]
fn reference_completion_uses_cursor_prefix_and_preserves_suffix() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "read @ suffix".to_string(),
        input_cursor: Some("read @".len()),
        ..AppState::default()
    });
    controller.set_references(vec![DialogItem {
        label: "@README.md".to_string(),
        detail: "file".to_string(),
    }]);

    // When
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('R'))),
        TuiEffect::None
    );

    // Then
    let Some(dialog) = controller.app.dialog.as_ref() else {
        panic!("reference popup");
    };
    assert_eq!(dialog.title, "references");
    assert_eq!(dialog.items[0].label, "@README.md");

    // When
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);

    // Then
    assert_eq!(controller.app.input, "read @README.md suffix");
    assert_eq!(controller.app.input_cursor, Some("read @README.md ".len()));
}

#[test]
fn reference_completion_popup_ignores_trailing_text_after_cursor() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "read @ suffix".to_string(),
        input_cursor: Some("read @".len()),
        ..AppState::default()
    });
    controller.set_references(vec![DialogItem {
        label: "@README.md".to_string(),
        detail: "file".to_string(),
    }]);

    // When
    type_text(&mut controller, "REA");

    // Then
    let Some(dialog) = controller.app.dialog.as_ref() else {
        panic!("reference popup");
    };
    assert_eq!(dialog.title, "references");
    assert_eq!(controller.app.input, "read @REA suffix");
}

#[test]
fn home_and_end_move_to_input_buffer_edges_when_prompt_is_not_empty() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "alpha\nbravo".to_string(),
        scroll_back: 12,
        ..AppState::default()
    });

    // When
    assert_eq!(controller.handle_key(key(KeyCode::Home)), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('>'))),
        TuiEffect::None
    );
    assert_eq!(controller.handle_key(key(KeyCode::End)), TuiEffect::None);
    assert_eq!(
        controller.handle_key(key(KeyCode::Char('!'))),
        TuiEffect::None
    );

    // Then
    assert_eq!(controller.app.input, ">alpha\nbravo!");
    assert_eq!(controller.app.scroll_back, 12);
}

#[test]
fn paste_inserts_text_at_cursor_and_preserves_suffix() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "a c".to_string(),
        input_cursor: Some("a ".len()),
        ..AppState::default()
    });

    // When
    assert_eq!(controller.handle_paste("b"), TuiEffect::None);

    // Then
    assert_eq!(controller.app.input, "a bc");
    assert_eq!(controller.app.input_cursor, Some("a b".len()));
}

#[test]
fn paste_placeholder_inserts_at_cursor_and_preserves_suffix() {
    // Given
    let mut controller = Controller::new(AppState {
        input: "send now".to_string(),
        input_cursor: Some("send ".len()),
        ..AppState::default()
    });
    let pasted = "one\ntwo\nthree";

    // When
    assert_eq!(controller.handle_paste(pasted), TuiEffect::None);

    // Then
    assert_eq!(controller.app.input, "send [Pasted Text #1] now");
    assert_eq!(
        controller.app.input_cursor,
        Some("send [Pasted Text #1] ".len())
    );
    assert_eq!(
        controller.handle_key(key(KeyCode::Enter)),
        TuiEffect::Submit("send one\ntwo\nthree now".to_string())
    );
}
