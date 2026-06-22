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
}
