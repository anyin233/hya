use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::dialogs::DialogMode;
use super::{Controller, TuiEffect, is_ctrl_shift_d, is_input_redo_key, is_input_undo_key};
use crate::tui::commands::{self, CustomCommand};
use crate::tui::prompt::{cursor_index, mention_trigger_index_at};

impl Controller {
    pub(super) fn apply_command_completion(&mut self, selected: usize) {
        let items = commands::completion_items_with_custom(&self.app.input, &self.custom_commands);
        if let Some(item) = items.get(selected) {
            self.prompt.checkpoint_edit(&self.app);
            self.app.input = format!("{} ", item.label);
            self.app.input_cursor = None;
        }
    }

    pub(super) fn handle_completion_popup_key(&mut self, key: KeyEvent) -> TuiEffect {
        if is_input_undo_key(&key) {
            return self.edit_prompt(|prompt, app| prompt.undo(app));
        }
        if is_input_redo_key(&key) {
            return self.edit_prompt(|prompt, app| prompt.redo(app));
        }
        if is_ctrl_shift_d(&key) {
            return self.edit_prompt(|prompt, app| prompt.delete_current_line(app));
        }
        if key.modifiers == KeyModifiers::ALT {
            match key.code {
                KeyCode::Char('b') | KeyCode::Left => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_word_backward(app));
                }
                KeyCode::Char('f') | KeyCode::Right => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_word_forward(app));
                }
                KeyCode::Char('d') | KeyCode::Delete => {
                    return self.edit_prompt(|prompt, app| prompt.delete_word_forward(app));
                }
                KeyCode::Backspace => {
                    return self.edit_prompt(|prompt, app| prompt.delete_word_backward(app));
                }
                _ => {}
            }
        }
        if key.modifiers == KeyModifiers::CONTROL {
            match key.code {
                KeyCode::Left => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_word_backward(app));
                }
                KeyCode::Right => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_word_forward(app));
                }
                KeyCode::Delete => {
                    return self.edit_prompt(|prompt, app| prompt.delete_word_forward(app));
                }
                KeyCode::Backspace => {
                    return self.edit_prompt(|prompt, app| prompt.delete_word_backward(app));
                }
                KeyCode::Char('a') => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_line_start(app));
                }
                KeyCode::Char('b') => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_left(app));
                }
                KeyCode::Char('e') => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_line_end(app));
                }
                KeyCode::Char('f') => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_right(app));
                }
                KeyCode::Char('u') => {
                    return self.edit_prompt(|prompt, app| prompt.delete_to_line_start(app));
                }
                KeyCode::Char('k') => {
                    return self.edit_prompt(|prompt, app| prompt.delete_to_line_end(app));
                }
                KeyCode::Char('w') => {
                    return self.edit_prompt(|prompt, app| prompt.delete_word_backward(app));
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => {
                self.app.dialog = None;
                self.dialog_mode = None;
                TuiEffect::None
            }
            KeyCode::Enter
                if self.dialog_mode == Some(DialogMode::CommandCompletion)
                    && is_exact_slash_command(&self.app.input, &self.custom_commands) =>
            {
                self.app.dialog = None;
                self.dialog_mode = None;
                self.submit_input()
            }
            KeyCode::Enter | KeyCode::Tab if key.modifiers != KeyModifiers::SHIFT => {
                let selected = self
                    .app
                    .dialog
                    .as_ref()
                    .map(|dialog| dialog.selected)
                    .unwrap_or(0);
                self.complete_popup_selection(selected);
                TuiEffect::None
            }
            KeyCode::Up => {
                if let Some(dialog) = self.app.dialog.as_mut() {
                    dialog.selected = dialog.selected.saturating_sub(1);
                }
                TuiEffect::None
            }
            KeyCode::Down => {
                if let Some(dialog) = self.app.dialog.as_mut() {
                    dialog.selected =
                        (dialog.selected + 1).min(dialog.items.len().saturating_sub(1));
                }
                TuiEffect::None
            }
            KeyCode::Backspace => self.edit_prompt(|prompt, app| prompt.backspace(app)),
            KeyCode::Delete => self.edit_prompt(|prompt, app| prompt.delete(app)),
            KeyCode::Left => self.edit_prompt(|prompt, app| prompt.move_cursor_left(app)),
            KeyCode::Right => self.edit_prompt(|prompt, app| prompt.move_cursor_right(app)),
            KeyCode::Home => self.edit_prompt(|prompt, app| prompt.move_cursor_buffer_start(app)),
            KeyCode::End => self.edit_prompt(|prompt, app| prompt.move_cursor_buffer_end(app)),
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.edit_prompt(|prompt, app| prompt.insert_char(app, c))
            }
            _ => TuiEffect::None,
        }
    }

    pub(super) fn complete_popup_selection(&mut self, selected: usize) {
        match self.dialog_mode {
            Some(DialogMode::CommandCompletion) => {
                self.apply_command_completion(selected);
                self.app.dialog = None;
                self.dialog_mode = None;
            }
            Some(DialogMode::ReferenceCompletion) => {
                let label = self
                    .app
                    .dialog
                    .as_ref()
                    .and_then(|dialog| dialog.items.get(selected))
                    .map(|item| item.label.clone());
                if let Some(label) = label {
                    self.complete_reference(&label);
                }
                self.app.dialog = None;
                self.dialog_mode = None;
            }
            _ => {}
        }
    }

    pub(super) fn complete_reference(&mut self, label: &str) {
        let cursor = cursor_index(&self.app.input, self.app.input_cursor);
        let Some(idx) = mention_trigger_index_at(&self.app.input, self.app.input_cursor) else {
            return;
        };
        let suffix = &self.app.input[cursor..];
        let suffix_separator_len = suffix
            .chars()
            .next()
            .filter(|ch| ch.is_whitespace())
            .map_or(0, char::len_utf8);
        let mut next = String::with_capacity(
            self.app.input[..idx].len() + label.len() + 1 + self.app.input[cursor..].len(),
        );
        next.push_str(&self.app.input[..idx]);
        next.push_str(label);
        if suffix_separator_len == 0 {
            next.push(' ');
        }
        let cursor_after_completion = next.len() + suffix_separator_len;
        next.push_str(suffix);
        self.prompt.checkpoint_edit(&self.app);
        self.app.input = next;
        self.app.input_cursor = Some(cursor_after_completion);
    }

    pub(super) fn refresh_inline_popup(&mut self) {
        if self.app.input.starts_with('/') && !self.app.input.contains(char::is_whitespace) {
            let items =
                commands::completion_items_with_custom(&self.app.input, &self.custom_commands);
            if items.is_empty() {
                self.app.dialog = None;
                self.dialog_mode = None;
            } else {
                self.open_command_completion_dialog(items);
            }
            return;
        }
        if let Some(idx) = mention_trigger_index_at(&self.app.input, self.app.input_cursor) {
            let cursor = cursor_index(&self.app.input, self.app.input_cursor);
            let prefix = &self.app.input[idx + 1..cursor];
            let items = self
                .references
                .iter()
                .filter(|item| {
                    let label = item.label.strip_prefix('@').unwrap_or(&item.label);
                    label.starts_with(prefix)
                })
                .cloned()
                .collect::<Vec<_>>();
            if items.is_empty() {
                self.app.dialog = None;
                self.dialog_mode = None;
            } else {
                self.open_reference_completion_dialog(items);
            }
            return;
        }
        if matches!(
            self.dialog_mode,
            Some(DialogMode::CommandCompletion | DialogMode::ReferenceCompletion)
        ) {
            self.app.dialog = None;
            self.dialog_mode = None;
        }
    }
}

fn is_exact_slash_command(input: &str, custom_commands: &[CustomCommand]) -> bool {
    input.strip_prefix('/').is_some_and(|command| {
        commands::resolve_slash(command).is_some()
            || commands::find_custom(custom_commands, command).is_some()
    })
}
