use super::*;
use yaca_tui::QuestionPrompt;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn down_selects_custom_answer_when_custom_answers_are_allowed() {
    // Given: an allow-custom question is focused on the last fixed option.
    let mut app = AppState {
        question: Some(QuestionPrompt {
            prompt: "Pick a mode".to_string(),
            options: vec!["fast".to_string(), "careful".to_string()],
            selected: 1,
            input: String::new(),
            input_cursor: None,
            allow_custom: true,
        }),
        ..AppState::default()
    };

    // When: the user moves down one row.
    let answer = handle_question_key(key(KeyCode::Down), &mut app);

    // Then: selection reaches OpenCode's numbered custom-answer row.
    assert_eq!(answer, None);
    assert_eq!(
        app.question.as_ref().map(|question| question.selected),
        Some(2)
    );
}

#[test]
fn enter_keeps_empty_custom_answer_open_when_custom_row_is_selected() {
    // Given: the custom answer row is selected but no custom text exists yet.
    let mut app = AppState {
        question: Some(QuestionPrompt {
            prompt: "Pick a mode".to_string(),
            options: vec!["fast".to_string(), "careful".to_string()],
            selected: 2,
            input: String::new(),
            input_cursor: None,
            allow_custom: true,
        }),
        ..AppState::default()
    };

    // When: the user presses Enter.
    let answer = handle_question_key(key(KeyCode::Enter), &mut app);

    // Then: the prompt stays open instead of submitting an out-of-range option.
    assert_eq!(answer, None);
}

#[test]
fn digit_key_submits_numbered_fixed_option() {
    // Given: a numbered single-select question.
    let mut app = AppState {
        question: Some(QuestionPrompt {
            prompt: "Pick a mode".to_string(),
            options: vec!["fast".to_string(), "careful".to_string()],
            selected: 0,
            input: String::new(),
            input_cursor: None,
            allow_custom: false,
        }),
        ..AppState::default()
    };

    // When: the user presses the displayed option number.
    let answer = handle_question_key(key(KeyCode::Char('2')), &mut app);

    // Then: yaca submits the matching option like OpenCode.
    assert_eq!(answer, Some(QuestionAnswer::Selected(1)));
}

#[test]
fn digit_key_selects_custom_row_without_typing_digit() {
    // Given: an allow-custom question with two fixed options.
    let mut app = AppState {
        question: Some(QuestionPrompt {
            prompt: "Pick a mode".to_string(),
            options: vec!["fast".to_string(), "careful".to_string()],
            selected: 0,
            input: String::new(),
            input_cursor: None,
            allow_custom: true,
        }),
        ..AppState::default()
    };

    // When: the user presses the displayed custom-row number.
    let answer = handle_question_key(key(KeyCode::Char('3')), &mut app);

    // Then: selection moves to the custom row instead of inserting "3".
    assert_eq!(answer, None);
    assert!(matches!(
        app.question.as_ref(),
        Some(question) if question.selected == 2 && question.input.is_empty()
    ));
}

#[test]
fn custom_answer_supports_opencode_cursor_editing() {
    // Given: an allow-custom question with an existing free-text answer.
    let mut app = AppState {
        question: Some(QuestionPrompt {
            prompt: "Pick a mode".to_string(),
            options: vec!["fast".to_string(), "careful".to_string()],
            selected: 2,
            input: "slow and safe".to_string(),
            input_cursor: None,
            allow_custom: true,
        }),
        ..AppState::default()
    };

    // When: the user moves left inside the answer and types a character.
    for _ in 0..5 {
        assert_eq!(handle_question_key(key(KeyCode::Left), &mut app), None);
    }
    assert_eq!(handle_question_key(key(KeyCode::Char('X')), &mut app), None);

    // Then: the character is inserted at the cursor instead of appended.
    assert!(matches!(
        app.question.as_ref(),
        Some(question) if question.input == "slow andX safe"
    ));
}
