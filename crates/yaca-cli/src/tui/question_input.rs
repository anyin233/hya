use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tool::QuestionAnswer;
use yaca_tui::AppState;

pub(super) fn handle_question_key(key: KeyEvent, app: &mut AppState) -> Option<QuestionAnswer> {
    let question = app.question.as_mut()?;
    match key.code {
        KeyCode::Esc => Some(QuestionAnswer::Cancelled),
        KeyCode::Enter => {
            if question.options.is_empty() || (question.allow_custom && !question.input.is_empty())
            {
                Some(QuestionAnswer::FreeText(std::mem::take(
                    &mut question.input,
                )))
            } else if question.allow_custom && question.selected == question.options.len() {
                None
            } else {
                Some(QuestionAnswer::Selected(question.selected))
            }
        }
        KeyCode::Up => {
            question.selected = question.selected.saturating_sub(1);
            None
        }
        KeyCode::Down => {
            if !question.options.is_empty() {
                let max_selected = if question.allow_custom {
                    question.options.len()
                } else {
                    question.options.len().saturating_sub(1)
                };
                question.selected = (question.selected + 1).min(max_selected);
            }
            None
        }
        KeyCode::Backspace => {
            question.input.pop();
            None
        }
        KeyCode::Char(c) => {
            if let Some(answer) = select_numbered_choice(c, question) {
                return answer;
            }
            if (question.options.is_empty() || question.allow_custom)
                && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            {
                question.input.push(c);
            }
            None
        }
        _ => None,
    }
}

fn select_numbered_choice(
    c: char,
    question: &mut yaca_tui::QuestionPrompt,
) -> Option<Option<QuestionAnswer>> {
    let digit = c.to_digit(10)?;
    if digit == 0 || question.options.is_empty() {
        return None;
    }
    let selected = usize::try_from(digit - 1).ok()?;
    let max = question.options.len() + usize::from(question.allow_custom);
    if selected >= max || selected >= 9 {
        return None;
    }
    question.selected = selected;
    if question.allow_custom && selected == question.options.len() {
        Some(None)
    } else {
        Some(Some(QuestionAnswer::Selected(selected)))
    }
}

#[cfg(test)]
mod tests {
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
}
