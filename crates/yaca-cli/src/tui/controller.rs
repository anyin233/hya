use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use yaca_tui::{AppState, DialogItem};

use super::agent_cycle::previous_agent_label;
use super::block_action::{SelectedBlockAction, selected_block_action};
use super::commands::{self, CommandKind, CustomCommand};
use super::leader_key::{LeaderAction, LeaderKey};
use super::message_scroll::handle_message_scroll_key;
use super::prompt::{PromptState, cursor_index};
use super::selection::{MessageSelectionStep, next_selected_message};
use crate::config::ModelEntry;

mod completion;
mod dialog_open;
mod dialogs;
mod slash;

use self::dialogs::DialogMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiEffect {
    None,
    Exit,
    Interrupt,
    Submit(String),
    SubmitConfigured {
        prompt: String,
        agent: Option<String>,
        model: Option<String>,
    },
    SelectModel(String),
    SelectAgent(String),
    SelectReasoning(String),
    ResumeSession(String),
    NewSession,
    CompactTranscript,
    InitProject,
    ExportTranscript,
    SelectedBlock(SelectedBlockAction),
    SystemMessage(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub detail: String,
}

fn provider_label_for_model(models: &[ModelEntry], model: &str) -> Option<String> {
    models
        .iter()
        .find(|entry| entry.id == model)
        .map(|entry| entry.provider.clone())
        .filter(|label| !label.trim().is_empty())
}

fn is_ctrl_shift_d(key: &KeyEvent) -> bool {
    key.modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        && matches!(key.code, KeyCode::Char('d' | 'D'))
}

fn is_input_undo_key(key: &KeyEvent) -> bool {
    matches!(
        (key.code, key.modifiers),
        (KeyCode::Char('-'), KeyModifiers::CONTROL)
            | (KeyCode::Char('z' | 'Z'), KeyModifiers::SUPER)
    )
}

fn is_input_redo_key(key: &KeyEvent) -> bool {
    (key.code == KeyCode::Char('.') && key.modifiers == KeyModifiers::CONTROL)
        || (matches!(key.code, KeyCode::Char('z' | 'Z'))
            && key.modifiers == (KeyModifiers::SUPER | KeyModifiers::SHIFT))
}

pub struct Controller {
    pub app: AppState,
    available_models: Vec<ModelEntry>,
    sessions: Vec<SessionSummary>,
    references: Vec<DialogItem>,
    agents: Vec<DialogItem>,
    custom_commands: Vec<CustomCommand>,
    dialog_mode: Option<DialogMode>,
    input_history: Vec<String>,
    history_cursor: Option<usize>,
    prompt: PromptState,
    leader_key: LeaderKey,
    last_ctrl_c: Option<Instant>,
}

impl Controller {
    #[cfg(test)]
    #[must_use]
    pub fn new(app: AppState) -> Self {
        Self::with_models_and_sessions(app, Vec::new(), Vec::new())
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_models(app: AppState, available_models: Vec<String>) -> Self {
        let entries = available_models
            .into_iter()
            .map(|id| ModelEntry {
                id,
                provider: "test".to_string(),
            })
            .collect();
        Self::with_models_and_sessions(app, entries, Vec::new())
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_sessions(app: AppState, sessions: Vec<SessionSummary>) -> Self {
        Self::with_models_and_sessions(app, Vec::new(), sessions)
    }

    #[must_use]
    pub fn with_models_and_sessions(
        mut app: AppState,
        mut available_models: Vec<ModelEntry>,
        sessions: Vec<SessionSummary>,
    ) -> Self {
        available_models.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.provider.cmp(&b.provider)));
        available_models.dedup();
        if app.model_provider_label.is_none() {
            app.model_provider_label = provider_label_for_model(&available_models, &app.model);
        }
        Self {
            app,
            available_models,
            sessions,
            references: Vec::new(),
            agents: Vec::new(),
            custom_commands: Vec::new(),
            dialog_mode: None,
            input_history: Vec::new(),
            history_cursor: None,
            prompt: PromptState::default(),
            leader_key: LeaderKey::default(),
            last_ctrl_c: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TuiEffect {
        if key.modifiers == KeyModifiers::CONTROL && matches!(key.code, KeyCode::Char('d')) {
            return self.handle_ctrl_d();
        }
        if self.app.dialog.is_some() {
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c'))
            {
                return self.handle_ctrl_c();
            }
            return self.handle_dialog_key(key);
        }
        if let Some(action) = self.leader_key.handle(&key) {
            return self.handle_leader_action(action);
        }
        let modified_enter = key.code == KeyCode::Enter
            && (key.modifiers == KeyModifiers::SHIFT
                || key.modifiers == KeyModifiers::CONTROL
                || key.modifiers == KeyModifiers::ALT);
        let ctrl_j = key.code == KeyCode::Char('j') && key.modifiers == KeyModifiers::CONTROL;
        if modified_enter || ctrl_j {
            return self.edit_prompt(|prompt, app| prompt.insert_char(app, '\n'));
        }
        if let Some(action) =
            selected_block_action(self.app.selected_message, &self.app.input, &key)
        {
            return TuiEffect::SelectedBlock(action);
        }
        if handle_message_scroll_key(&mut self.app, &key) {
            return TuiEffect::None;
        }
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
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Up => return self.select_previous_message(),
                KeyCode::Down => return self.select_next_message(),
                KeyCode::Left if key.modifiers == KeyModifiers::CONTROL => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_word_backward(app));
                }
                KeyCode::Right if key.modifiers == KeyModifiers::CONTROL => {
                    return self.edit_prompt(|prompt, app| prompt.move_cursor_word_forward(app));
                }
                KeyCode::Delete if key.modifiers == KeyModifiers::CONTROL => {
                    return self.edit_prompt(|prompt, app| prompt.delete_word_forward(app));
                }
                KeyCode::Backspace if key.modifiers == KeyModifiers::CONTROL => {
                    return self.edit_prompt(|prompt, app| prompt.delete_word_backward(app));
                }
                KeyCode::Char('c') => return self.handle_ctrl_c(),
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
                KeyCode::Char('p') => {
                    self.open_command_palette_dialog();
                    return TuiEffect::None;
                }
                KeyCode::Char('n') => return TuiEffect::NewSession,
                KeyCode::Char('r') => {
                    self.open_resume_dialog();
                    return TuiEffect::None;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc if self.app.running => TuiEffect::Interrupt,
            KeyCode::F(2) => {
                self.open_model_dialog();
                TuiEffect::None
            }
            KeyCode::BackTab => self.select_previous_agent(),
            KeyCode::Tab if key.modifiers == KeyModifiers::SHIFT => self.select_previous_agent(),
            KeyCode::Tab => {
                self.app.yolo = !self.app.yolo;
                self.disarm_exit();
                TuiEffect::None
            }
            KeyCode::Enter => self.submit_input(),
            KeyCode::Backspace => self.edit_prompt(|prompt, app| prompt.backspace(app)),
            KeyCode::Delete => self.edit_prompt(|prompt, app| prompt.delete(app)),
            KeyCode::Left => self.edit_prompt(|prompt, app| prompt.move_cursor_left(app)),
            KeyCode::Right => self.edit_prompt(|prompt, app| prompt.move_cursor_right(app)),
            KeyCode::PageUp => {
                self.app.scroll_up(5);
                TuiEffect::None
            }
            KeyCode::PageDown => {
                self.app.scroll_down(5);
                TuiEffect::None
            }
            KeyCode::Home if !self.app.input.is_empty() => {
                self.edit_prompt(|prompt, app| prompt.move_cursor_buffer_start(app))
            }
            KeyCode::Home => {
                self.app.scroll_back = u16::MAX;
                TuiEffect::None
            }
            KeyCode::End if !self.app.input.is_empty() => {
                self.edit_prompt(|prompt, app| prompt.move_cursor_buffer_end(app))
            }
            KeyCode::End => {
                self.app.scroll_back = 0;
                TuiEffect::None
            }
            KeyCode::Up => {
                if self.should_navigate_input_history() {
                    self.previous_input_history();
                } else {
                    self.move_prompt_cursor(|prompt, app| prompt.move_cursor_up(app));
                }
                TuiEffect::None
            }
            KeyCode::Down => {
                if self.should_navigate_input_history() {
                    self.next_input_history();
                } else {
                    self.move_prompt_cursor(|prompt, app| prompt.move_cursor_down(app));
                }
                TuiEffect::None
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.edit_prompt(|prompt, app| prompt.insert_char(app, c))
            }
            _ => TuiEffect::None,
        }
    }

    fn select_previous_agent(&mut self) -> TuiEffect {
        previous_agent_label(&self.app.agent, &self.agents).map_or(TuiEffect::None, |agent| {
            self.app.agent = agent.clone();
            self.disarm_exit();
            TuiEffect::SelectAgent(agent)
        })
    }

    fn handle_leader_action(&mut self, action: LeaderAction) -> TuiEffect {
        match action {
            LeaderAction::Arm | LeaderAction::Cancel => TuiEffect::None,
            LeaderAction::ModelList => {
                self.open_model_dialog();
                TuiEffect::None
            }
            LeaderAction::AgentList => {
                self.open_agent_dialog();
                TuiEffect::None
            }
            LeaderAction::SessionList => {
                self.open_resume_dialog();
                TuiEffect::None
            }
            LeaderAction::SessionNew => TuiEffect::NewSession,
            LeaderAction::SessionCompact => TuiEffect::CompactTranscript,
            LeaderAction::StatusView => {
                self.open_tools_dialog();
                TuiEffect::None
            }
            LeaderAction::SessionExport => TuiEffect::ExportTranscript,
            LeaderAction::Exit => TuiEffect::Exit,
        }
    }

    fn select_previous_message(&mut self) -> TuiEffect {
        self.app.selected_message = next_selected_message(
            self.app.selected_message,
            self.app.projection.session.messages.len(),
            MessageSelectionStep::Previous,
        );
        TuiEffect::None
    }

    fn select_next_message(&mut self) -> TuiEffect {
        self.app.selected_message = next_selected_message(
            self.app.selected_message,
            self.app.projection.session.messages.len(),
            MessageSelectionStep::Next,
        );
        TuiEffect::None
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> TuiEffect {
        match event.kind {
            MouseEventKind::ScrollUp => self.app.scroll_up(3),
            MouseEventKind::ScrollDown => self.app.scroll_down(3),
            _ => {}
        }
        TuiEffect::None
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionSummary>) {
        self.sessions = sessions;
    }

    pub fn set_references(&mut self, references: Vec<DialogItem>) {
        self.references = references;
    }

    pub fn set_agents(&mut self, agents: Vec<DialogItem>) {
        self.agents = agents;
    }

    pub fn set_custom_commands(&mut self, custom_commands: Vec<CustomCommand>) {
        self.custom_commands = custom_commands;
    }

    pub fn handle_paste(&mut self, text: &str) -> TuiEffect {
        self.disarm_exit();
        let outcome = self.prompt.handle_paste(&mut self.app, text);
        if outcome.refresh_popup {
            self.refresh_inline_popup();
        }
        TuiEffect::None
    }

    fn edit_prompt(&mut self, edit: impl FnOnce(&mut PromptState, &mut AppState)) -> TuiEffect {
        edit(&mut self.prompt, &mut self.app);
        self.history_cursor = None;
        self.disarm_exit();
        self.refresh_inline_popup();
        TuiEffect::None
    }

    fn move_prompt_cursor(
        &mut self,
        move_cursor: impl FnOnce(&mut PromptState, &mut AppState) -> bool,
    ) {
        if move_cursor(&mut self.prompt, &mut self.app) {
            self.disarm_exit();
            self.refresh_inline_popup();
        }
    }

    fn should_navigate_input_history(&self) -> bool {
        if self.app.input.is_empty() {
            return true;
        }
        self.history_cursor.is_some() && self.input_cursor_at_history_boundary()
    }

    fn input_cursor_at_history_boundary(&self) -> bool {
        let cursor = cursor_index(&self.app.input, self.app.input_cursor);
        cursor == 0 || cursor == self.app.input.len()
    }

    fn handle_ctrl_c(&mut self) -> TuiEffect {
        if self.app.dialog.is_some() {
            if matches!(
                self.dialog_mode,
                Some(DialogMode::CommandCompletion | DialogMode::ReferenceCompletion)
            ) && !self.app.input.is_empty()
            {
                self.clear_prompt();
                return self.arm_exit();
            }
            self.app.dialog = None;
            self.dialog_mode = None;
            return TuiEffect::None;
        }
        if !self.app.input.is_empty() {
            self.clear_prompt();
            return self.arm_exit();
        }
        if self.app.running {
            let effect = self.arm_exit();
            return if effect == TuiEffect::Exit {
                effect
            } else {
                TuiEffect::Interrupt
            };
        }
        self.arm_exit()
    }

    fn handle_ctrl_d(&mut self) -> TuiEffect {
        if self.app.dialog.is_some()
            && !matches!(
                self.dialog_mode,
                Some(DialogMode::CommandCompletion | DialogMode::ReferenceCompletion)
            )
        {
            return TuiEffect::Exit;
        }
        if !self.app.input.is_empty() {
            return self.edit_prompt(|prompt, app| prompt.delete(app));
        }
        TuiEffect::Exit
    }

    fn submit_input(&mut self) -> TuiEffect {
        let input = self
            .prompt
            .expanded_input(&self.app.input)
            .trim_end()
            .to_string();
        self.clear_prompt();
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return TuiEffect::None;
        }
        if let Some(command) = trimmed.strip_prefix('/') {
            return self.dispatch_slash(command);
        }
        self.app.scroll_back = 0;
        self.input_history.push(input.clone());
        self.history_cursor = None;
        self.disarm_exit();
        TuiEffect::Submit(input)
    }

    fn previous_input_history(&mut self) {
        if self.input_history.is_empty() {
            self.app.scroll_up(1);
            return;
        }
        let idx = self.history_cursor.map_or_else(
            || self.input_history.len().saturating_sub(1),
            |idx| idx.saturating_sub(1),
        );
        self.history_cursor = Some(idx);
        if let Some(value) = self.input_history.get(idx) {
            self.app.input = value.clone();
            self.app.input_cursor = Some(0);
            self.refresh_inline_popup();
        }
    }

    fn next_input_history(&mut self) {
        let Some(idx) = self.history_cursor else {
            if self.input_history.is_empty() {
                self.app.scroll_down(1);
            }
            return;
        };
        let next = idx + 1;
        if next < self.input_history.len() {
            self.history_cursor = Some(next);
            if let Some(value) = self.input_history.get(next) {
                self.app.input = value.clone();
                self.app.input_cursor = None;
                self.refresh_inline_popup();
            }
        } else {
            self.history_cursor = None;
            self.app.input.clear();
            self.app.input_cursor = None;
            self.refresh_inline_popup();
        }
    }

    fn clear_prompt(&mut self) {
        self.prompt.clear(&mut self.app);
        self.disarm_exit();
        if matches!(
            self.dialog_mode,
            Some(DialogMode::CommandCompletion | DialogMode::ReferenceCompletion)
        ) {
            self.app.dialog = None;
            self.dialog_mode = None;
        }
    }

    fn disarm_exit(&mut self) {
        self.app.exit_armed = false;
        self.last_ctrl_c = None;
    }

    fn arm_exit(&mut self) -> TuiEffect {
        const EXIT_WINDOW: Duration = Duration::from_millis(900);
        let now = Instant::now();
        if self
            .last_ctrl_c
            .is_some_and(|last| now.duration_since(last) <= EXIT_WINDOW)
        {
            self.app.exit_armed = false;
            self.last_ctrl_c = None;
            return TuiEffect::Exit;
        }
        self.app.exit_armed = true;
        self.last_ctrl_c = Some(now);
        TuiEffect::None
    }
}

#[cfg(test)]
mod scroll_tests;

#[cfg(test)]
mod leader_key_tests;

#[cfg(test)]
mod app_exit_tests;

#[cfg(test)]
mod command_palette_tests;

#[cfg(test)]
mod input_edit_tests;

#[cfg(test)]
mod tab_mode_tests;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn ctrl_c() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn ctrl(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
    }

    fn type_text(controller: &mut Controller, text: &str) {
        for c in text.chars() {
            assert_eq!(
                controller.handle_key(key(KeyCode::Char(c))),
                TuiEffect::None
            );
        }
    }

    #[test]
    fn input_newline_keybinds_insert_newline_without_submit() {
        for (label, key) in [
            (
                "shift+enter",
                KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            ),
            (
                "ctrl+enter",
                KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            ),
            (
                "alt+enter",
                KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            ),
            (
                "ctrl+j",
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            ),
        ] {
            let mut controller = Controller::new(AppState {
                input: "first".to_string(),
                ..AppState::default()
            });

            assert_eq!(controller.handle_key(key), TuiEffect::None, "{label}");
            assert_eq!(controller.app.input, "first\n", "{label}");
        }
    }

    #[test]
    fn ctrl_c_clears_input_without_exit() {
        let mut controller = Controller::new(AppState {
            input: "hello".to_string(),
            ..AppState::default()
        });

        assert_eq!(controller.handle_key(ctrl_c()), TuiEffect::None);
        assert_eq!(controller.app.input, "");
    }

    #[test]
    fn ctrl_c_clears_input_even_when_completion_popup_is_open() {
        let mut controller = Controller::new(AppState::default());
        type_text(&mut controller, "/");

        assert!(controller.app.dialog.is_some());
        assert_eq!(controller.handle_key(ctrl_c()), TuiEffect::None);
        assert_eq!(controller.app.input, "");
        assert!(controller.app.dialog.is_none());
        assert!(controller.app.exit_armed);
    }

    #[test]
    fn ctrl_c_interrupts_running_turn_without_exit() {
        let mut controller = Controller::new(AppState {
            running: true,
            ..AppState::default()
        });

        assert_eq!(controller.handle_key(ctrl_c()), TuiEffect::Interrupt);
        assert!(controller.app.running);
    }

    #[test]
    fn escape_interrupts_running_turn_without_exit() {
        let mut controller = Controller::new(AppState {
            running: true,
            ..AppState::default()
        });

        assert_eq!(
            controller.handle_key(key(KeyCode::Esc)),
            TuiEffect::Interrupt
        );
        assert!(controller.app.running);
    }

    #[test]
    fn ctrl_c_exits_only_when_idle_empty_and_no_dialog() {
        let mut controller = Controller::new(AppState::default());

        assert_eq!(controller.handle_key(ctrl_c()), TuiEffect::None);
        assert!(controller.app.exit_armed);
        assert_eq!(controller.handle_key(ctrl_c()), TuiEffect::Exit);
    }

    #[test]
    fn typing_after_ctrl_c_exit_arm_prevents_accidental_exit() {
        let mut controller = Controller::new(AppState::default());

        assert_eq!(controller.handle_key(ctrl_c()), TuiEffect::None);
        assert!(controller.app.exit_armed);
        type_text(&mut controller, "new input");

        assert_eq!(controller.handle_key(ctrl_c()), TuiEffect::None);
        assert_eq!(controller.app.input, "");
        assert!(controller.app.exit_armed);
    }

    #[test]
    fn slash_popup_opens_as_soon_as_trigger_is_typed() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/");

        let dialog = controller.app.dialog.as_ref().expect("slash popup");
        assert_eq!(dialog.title, "commands");
        assert!(dialog.items.iter().any(|item| item.label == "/model"));
    }

    #[test]
    fn at_popup_completes_reference_items() {
        let mut controller = Controller::new(AppState::default());
        controller.set_references(vec![DialogItem {
            label: "@README.md".to_string(),
            detail: "file".to_string(),
        }]);

        type_text(&mut controller, "read @");

        let dialog = controller.app.dialog.as_ref().expect("reference popup");
        assert_eq!(dialog.title, "references");
        assert_eq!(dialog.items[0].label, "@README.md");

        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
        assert_eq!(controller.app.input, "read @README.md ");
    }

    #[test]
    fn paste_placeholder_expands_on_submit() {
        let mut controller = Controller::new(AppState::default());
        let pasted = "one\ntwo\nthree";

        assert_eq!(controller.handle_paste(pasted), TuiEffect::None);
        assert_eq!(controller.app.input, "[Pasted Text #1] ");
        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::Submit(pasted.to_string())
        );
    }

    #[test]
    fn consecutive_paste_reveals_previous_raw_text() {
        let mut controller = Controller::new(AppState::default());

        assert_eq!(
            controller.handle_paste("alpha\nbeta\ngamma"),
            TuiEffect::None
        );
        assert_eq!(controller.handle_paste("second paste"), TuiEffect::None);

        assert!(
            controller.app.input.contains("alpha\nbeta\ngamma"),
            "second paste should reveal the previous original content"
        );
        assert!(controller.app.input.contains("second paste"));
    }

    #[test]
    fn image_path_paste_inserts_image_placeholder() {
        let mut controller = Controller::new(AppState::default());

        assert_eq!(
            controller.handle_paste("/tmp/screenshot.png"),
            TuiEffect::None
        );

        assert_eq!(controller.app.input, "[Image #1] ");
        assert_eq!(controller.app.attachments.len(), 1);
        assert_eq!(controller.app.attachments[0].placeholder, "[Image #1]");
        assert_eq!(
            controller.app.attachments[0].source_path.as_deref(),
            Some("/tmp/screenshot.png")
        );
    }

    #[test]
    fn markdown_image_paste_extracts_local_image_path() {
        let mut controller = Controller::new(AppState::default());

        assert_eq!(
            controller.handle_paste("![screenshot](/tmp/screenshot with spaces.png)"),
            TuiEffect::None
        );

        assert_eq!(controller.app.input, "[Image #1] ");
        assert_eq!(
            controller.app.attachments[0].source_path.as_deref(),
            Some("/tmp/screenshot with spaces.png")
        );
    }

    #[test]
    fn image_tag_paste_extracts_path_attribute() {
        let mut controller = Controller::new(AppState::default());

        assert_eq!(
            controller.handle_paste("<image name=[Image #1] path=\"/tmp/CleanShot.png\">"),
            TuiEffect::None
        );

        assert_eq!(controller.app.input, "[Image #1] ");
        assert_eq!(
            controller.app.attachments[0].source_path.as_deref(),
            Some("/tmp/CleanShot.png")
        );
    }

    #[test]
    fn slash_model_opens_model_dialog_without_submit() {
        let mut controller =
            Controller::with_models(AppState::default(), vec!["alpha".into(), "beta".into()]);

        type_text(&mut controller, "/model");

        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
        let dialog = controller.app.dialog.as_ref().expect("model dialog");
        assert_eq!(dialog.title, "select model");
        assert_eq!(dialog.items[0].label, "alpha");
        assert_eq!(dialog.items[1].label, "beta");
    }

    #[test]
    fn model_dialog_shows_provider_in_detail() {
        let mut controller = Controller::with_models(
            AppState {
                model: "alpha".to_string(),
                ..AppState::default()
            },
            vec!["alpha".into(), "beta".into()],
        );

        type_text(&mut controller, "/model");
        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
        let dialog = controller.app.dialog.as_ref().expect("model dialog");
        assert_eq!(dialog.items[0].detail, "test · current");
        assert_eq!(dialog.items[1].detail, "test");
    }

    #[test]
    fn tab_completes_slash_command_prefixes() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/m");

        assert_eq!(controller.handle_key(key(KeyCode::Tab)), TuiEffect::None);
        assert_eq!(controller.app.input, "/model ");
        assert!(controller.app.dialog.is_none());
    }

    #[test]
    fn slash_completion_popup_selects_ambiguous_commands() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/");

        let dialog = controller.app.dialog.as_ref().expect("slash dialog");
        assert_eq!(dialog.title, "commands");
        assert!(dialog.items.iter().any(|item| item.label == "/model"));

        assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
        assert_eq!(controller.app.input, "/resume ");
    }

    #[test]
    fn model_dialog_navigation_and_enter_selects_model() {
        let mut controller = Controller::with_models(
            AppState {
                model: "alpha".to_string(),
                ..AppState::default()
            },
            vec!["alpha".into(), "beta".into(), "gamma".into()],
        );
        type_text(&mut controller, "/model");
        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);

        assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
        assert_eq!(controller.handle_key(key(KeyCode::Tab)), TuiEffect::None);
        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::SelectModel("gamma".to_string())
        );
        assert_eq!(controller.app.model, "gamma");
        assert_eq!(controller.app.model_provider_label.as_deref(), Some("test"));
        assert!(controller.app.dialog.is_none());
    }

    #[test]
    fn slash_help_opens_help_dialog_from_registry() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/help");

        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
        let dialog = controller.app.dialog.as_ref().expect("help dialog");
        assert_eq!(dialog.title, "commands");
        assert!(dialog.items.iter().any(|item| item.label == "/model"));
        assert!(dialog.items.iter().any(|item| item.label == "/resume"));
    }

    #[test]
    fn slash_new_requests_new_session() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/new");

        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::NewSession
        );
    }

    #[test]
    fn slash_quit_requests_exit() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/quit");

        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::Exit);
    }

    #[test]
    fn slash_export_requests_transcript_export() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/export");

        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::ExportTranscript
        );
    }

    #[test]
    fn slash_compact_requests_context_compaction() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/compact");

        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::CompactTranscript
        );
    }

    #[test]
    fn slash_init_requests_project_initialization() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/init");

        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::InitProject
        );
    }

    #[test]
    fn slash_agent_opens_agent_dialog_and_selects_profile() {
        let mut controller = Controller::new(AppState::default());
        controller.set_agents(vec![DialogItem {
            label: "plan".to_string(),
            detail: "Plan before editing".to_string(),
        }]);

        type_text(&mut controller, "/agent");
        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::SelectAgent("plan".to_string())
        );
    }

    #[test]
    fn slash_tools_opens_tool_status_dialog() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/tools");
        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);

        let dialog = controller.app.dialog.as_ref().expect("tools dialog");
        assert_eq!(dialog.title, "tools");
        assert!(dialog.items.iter().any(|item| item.label == "read"));
        assert!(dialog.items.iter().any(|item| item.label == "mcp"));
    }

    #[test]
    fn custom_slash_command_submits_expanded_prompt() {
        let mut controller = Controller::new(AppState::default());
        controller.set_custom_commands(vec![CustomCommand {
            name: "component".to_string(),
            description: "Create a component".to_string(),
            template: "Create $1 in $2. Args: $ARGUMENTS".to_string(),
            agent: None,
            model: None,
            source: commands::CustomCommandSource::Markdown,
        }]);

        type_text(&mut controller, "/component Button src/ui");

        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::Submit("Create Button in src/ui. Args: Button src/ui".to_string())
        );
    }

    #[test]
    fn custom_slash_command_submits_agent_and_model_metadata() {
        let mut controller = Controller::new(AppState::default());
        controller.set_custom_commands(vec![CustomCommand {
            name: "planit".to_string(),
            description: "Plan work".to_string(),
            template: "Plan $ARGUMENTS".to_string(),
            agent: Some("plan".to_string()),
            model: Some("anthropic/claude-sonnet".to_string()),
            source: commands::CustomCommandSource::Markdown,
        }]);

        type_text(&mut controller, "/planit checkout flow");

        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::SubmitConfigured {
                prompt: "Plan checkout flow".to_string(),
                agent: Some("plan".to_string()),
                model: Some("anthropic/claude-sonnet".to_string()),
            }
        );
    }

    #[test]
    fn slash_resume_opens_resume_dialog_and_selects_session() {
        let mut controller = Controller::with_sessions(
            AppState::default(),
            vec![SessionSummary {
                id: "sess-1".to_string(),
                title: "Earlier task".to_string(),
                detail: "fake · just now".to_string(),
            }],
        );

        type_text(&mut controller, "/resume");
        assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::ResumeSession("sess-1".to_string())
        );
    }

    #[test]
    fn shortcuts_open_core_dialogs_and_session_effects() {
        let mut controller =
            Controller::with_models(AppState::default(), vec!["alpha".into(), "beta".into()]);

        assert_eq!(controller.handle_key(key(KeyCode::F(2))), TuiEffect::None);
        assert_eq!(
            controller.app.dialog.as_ref().expect("model dialog").title,
            "select model"
        );
        controller.app.dialog = None;

        assert_eq!(controller.handle_key(ctrl('p')), TuiEffect::None);
        assert_eq!(
            controller.app.dialog.as_ref().expect("help dialog").title,
            "commands"
        );
        controller.app.dialog = None;

        assert_eq!(controller.handle_key(ctrl('n')), TuiEffect::NewSession);
    }

    #[test]
    fn submitted_prompts_feed_input_history() {
        let mut controller = Controller::new(AppState::default());
        type_text(&mut controller, "first");
        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::Submit("first".to_string())
        );
        type_text(&mut controller, "second");
        assert_eq!(
            controller.handle_key(key(KeyCode::Enter)),
            TuiEffect::Submit("second".to_string())
        );

        assert_eq!(controller.handle_key(key(KeyCode::Up)), TuiEffect::None);
        assert_eq!(controller.app.input, "second");
        assert_eq!(controller.handle_key(key(KeyCode::Up)), TuiEffect::None);
        assert_eq!(controller.app.input, "first");
        assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
        assert_eq!(controller.app.input, "second");
    }

    #[test]
    fn home_end_and_mouse_wheel_scroll_transcript() {
        let mut controller = Controller::new(AppState::default());

        assert_eq!(controller.handle_key(key(KeyCode::Home)), TuiEffect::None);
        assert_eq!(controller.app.scroll_back, u16::MAX);
        assert_eq!(controller.handle_key(key(KeyCode::End)), TuiEffect::None);
        assert_eq!(controller.app.scroll_back, 0);

        controller.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        });
        assert_eq!(controller.app.scroll_back, 3);
        controller.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        });
        assert_eq!(controller.app.scroll_back, 0);
    }
}
