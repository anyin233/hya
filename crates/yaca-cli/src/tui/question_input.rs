use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use yaca_tool::QuestionAnswer;
use yaca_tui::AppState;

pub(super) fn handle_question_key(key: KeyEvent, app: &mut AppState) -> Option<QuestionAnswer> {
    let question = app.question.as_mut()?;
    let editable = question.options.is_empty() || question.allow_custom;
    match key.code {
        KeyCode::Esc => Some(QuestionAnswer::Cancelled),
        KeyCode::Enter => {
            if question.options.is_empty() || (question.allow_custom && !question.input.is_empty())
            {
                let input = std::mem::take(&mut question.input);
                question.input_cursor = None;
                Some(QuestionAnswer::FreeText(input))
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
        KeyCode::Backspace if editable => {
            backspace(question);
            None
        }
        KeyCode::Delete if editable => {
            delete(question);
            None
        }
        KeyCode::Left if editable => {
            move_left(question);
            None
        }
        KeyCode::Right if editable => {
            move_right(question);
            None
        }
        KeyCode::Home if editable => {
            question.input_cursor = Some(0);
            None
        }
        KeyCode::End if editable => {
            question.input_cursor = Some(question.input.len());
            None
        }
        KeyCode::Char(c) => {
            if let Some(answer) = select_numbered_choice(c, question) {
                return answer;
            }
            if editable {
                if handle_modified_edit_key(c, key.modifiers, question) {
                    return None;
                }
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    insert_char(question, c);
                }
            }
            None
        }
        _ => None,
    }
}

fn handle_modified_edit_key(
    c: char,
    modifiers: KeyModifiers,
    question: &mut yaca_tui::QuestionPrompt,
) -> bool {
    if modifiers != KeyModifiers::CONTROL {
        return false;
    }
    match c {
        'a' => question.input_cursor = Some(0),
        'b' => move_left(question),
        'd' => delete(question),
        'e' => question.input_cursor = Some(question.input.len()),
        'f' => move_right(question),
        'k' => delete_to_end(question),
        'u' => delete_to_start(question),
        'w' => delete_word_backward(question),
        _ => return false,
    }
    true
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

fn insert_char(question: &mut yaca_tui::QuestionPrompt, c: char) {
    let cursor = cursor_index(&question.input, question.input_cursor);
    question.input.insert(cursor, c);
    question.input_cursor = Some(cursor + c.len_utf8());
}

fn backspace(question: &mut yaca_tui::QuestionPrompt) {
    let cursor = cursor_index(&question.input, question.input_cursor);
    let previous = previous_boundary(&question.input, cursor);
    if previous == cursor {
        question.input_cursor = Some(cursor);
        return;
    }
    question.input.replace_range(previous..cursor, "");
    question.input_cursor = Some(previous);
}

fn delete(question: &mut yaca_tui::QuestionPrompt) {
    let cursor = cursor_index(&question.input, question.input_cursor);
    let next = next_boundary(&question.input, cursor);
    if next == cursor {
        question.input_cursor = Some(cursor);
        return;
    }
    question.input.replace_range(cursor..next, "");
    question.input_cursor = Some(cursor);
}

fn move_left(question: &mut yaca_tui::QuestionPrompt) {
    let cursor = cursor_index(&question.input, question.input_cursor);
    question.input_cursor = Some(previous_boundary(&question.input, cursor));
}

fn move_right(question: &mut yaca_tui::QuestionPrompt) {
    let cursor = cursor_index(&question.input, question.input_cursor);
    question.input_cursor = Some(next_boundary(&question.input, cursor));
}

fn delete_to_start(question: &mut yaca_tui::QuestionPrompt) {
    let cursor = cursor_index(&question.input, question.input_cursor);
    question.input.replace_range(0..cursor, "");
    question.input_cursor = Some(0);
}

fn delete_to_end(question: &mut yaca_tui::QuestionPrompt) {
    let cursor = cursor_index(&question.input, question.input_cursor);
    question.input.truncate(cursor);
    question.input_cursor = Some(cursor);
}

fn delete_word_backward(question: &mut yaca_tui::QuestionPrompt) {
    let cursor = cursor_index(&question.input, question.input_cursor);
    let trimmed = question.input[..cursor].trim_end();
    let end = trimmed.len();
    let start = trimmed
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(idx, c)| idx + c.len_utf8());
    question.input.replace_range(start..cursor, "");
    question.input_cursor = Some(start.min(end));
}

fn cursor_index(input: &str, cursor: Option<usize>) -> usize {
    let mut idx = cursor.unwrap_or(input.len()).min(input.len());
    while !input.is_char_boundary(idx) {
        idx = idx.saturating_sub(1);
    }
    idx
}

fn previous_boundary(input: &str, cursor: usize) -> usize {
    input[..cursor]
        .char_indices()
        .next_back()
        .map_or(cursor, |(idx, _)| idx)
}

fn next_boundary(input: &str, cursor: usize) -> usize {
    input[cursor..]
        .char_indices()
        .nth(1)
        .map_or(input.len(), |(idx, _)| cursor + idx)
}

#[cfg(test)]
mod tests;
