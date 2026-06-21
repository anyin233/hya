use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use yaca_tui::{AppState, DialogItem, DialogView};

use super::commands::{self, CommandKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiEffect {
    None,
    Exit,
    Interrupt,
    Submit(String),
    SelectModel(String),
    ResumeSession(String),
    NewSession,
    SystemMessage(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogMode {
    Model,
    Resume,
    Help,
    CommandCompletion,
}

pub struct Controller {
    pub app: AppState,
    available_models: Vec<String>,
    sessions: Vec<SessionSummary>,
    dialog_mode: Option<DialogMode>,
    input_history: Vec<String>,
    history_cursor: Option<usize>,
}

impl Controller {
    #[cfg(test)]
    #[must_use]
    pub fn new(app: AppState) -> Self {
        Self::with_models_and_sessions(app, Vec::new(), Vec::new())
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_models(app: AppState, mut available_models: Vec<String>) -> Self {
        Self::with_models_and_sessions(app, std::mem::take(&mut available_models), Vec::new())
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_sessions(app: AppState, sessions: Vec<SessionSummary>) -> Self {
        Self::with_models_and_sessions(app, Vec::new(), sessions)
    }

    #[must_use]
    pub fn with_models_and_sessions(
        app: AppState,
        mut available_models: Vec<String>,
        sessions: Vec<SessionSummary>,
    ) -> Self {
        available_models.sort();
        available_models.dedup();
        Self {
            app,
            available_models,
            sessions,
            dialog_mode: None,
            input_history: Vec::new(),
            history_cursor: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TuiEffect {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return self.handle_ctrl_c();
        }
        if self.app.dialog.is_some() {
            return self.handle_dialog_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('p') => {
                    self.open_help_dialog();
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
            KeyCode::F(2) => {
                self.open_model_dialog();
                TuiEffect::None
            }
            KeyCode::Tab | KeyCode::Down if self.app.input.starts_with('/') => {
                self.complete_slash_input()
            }
            KeyCode::Enter => self.submit_input(),
            KeyCode::Backspace => {
                self.app.input.pop();
                self.history_cursor = None;
                TuiEffect::None
            }
            KeyCode::PageUp => {
                self.app.scroll_up(5);
                TuiEffect::None
            }
            KeyCode::PageDown => {
                self.app.scroll_down(5);
                TuiEffect::None
            }
            KeyCode::Home => {
                self.app.scroll_back = u16::MAX;
                TuiEffect::None
            }
            KeyCode::End => {
                self.app.scroll_back = 0;
                TuiEffect::None
            }
            KeyCode::Up => {
                self.previous_input_history();
                TuiEffect::None
            }
            KeyCode::Down => {
                self.next_input_history();
                TuiEffect::None
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.app.input.push(c);
                self.history_cursor = None;
                TuiEffect::None
            }
            _ => TuiEffect::None,
        }
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

    fn handle_ctrl_c(&mut self) -> TuiEffect {
        if self.app.dialog.is_some() {
            self.app.dialog = None;
            self.dialog_mode = None;
            return TuiEffect::None;
        }
        if !self.app.input.is_empty() {
            self.app.input.clear();
            return TuiEffect::None;
        }
        if self.app.running {
            return TuiEffect::Interrupt;
        }
        TuiEffect::Exit
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) -> TuiEffect {
        let Some(dialog) = self.app.dialog.as_mut() else {
            return TuiEffect::None;
        };
        match key.code {
            KeyCode::Esc => {
                self.app.dialog = None;
                self.dialog_mode = None;
                TuiEffect::None
            }
            KeyCode::Tab
                if self.dialog_mode == Some(DialogMode::CommandCompletion)
                    && key.modifiers != KeyModifiers::SHIFT =>
            {
                let selected = dialog.selected;
                self.app.dialog = None;
                self.dialog_mode = None;
                self.apply_command_completion(selected);
                TuiEffect::None
            }
            KeyCode::Up => {
                dialog.selected = dialog.selected.saturating_sub(1);
                TuiEffect::None
            }
            KeyCode::Down | KeyCode::Tab if key.modifiers != KeyModifiers::SHIFT => {
                dialog.selected = (dialog.selected + 1).min(dialog.items.len().saturating_sub(1));
                TuiEffect::None
            }
            KeyCode::BackTab | KeyCode::Tab if key.modifiers == KeyModifiers::SHIFT => {
                dialog.selected = dialog.selected.saturating_sub(1);
                TuiEffect::None
            }
            KeyCode::Home => {
                dialog.selected = 0;
                TuiEffect::None
            }
            KeyCode::End => {
                dialog.selected = dialog.items.len().saturating_sub(1);
                TuiEffect::None
            }
            KeyCode::PageUp => {
                dialog.selected = dialog.selected.saturating_sub(5);
                TuiEffect::None
            }
            KeyCode::PageDown => {
                dialog.selected = (dialog.selected + 5).min(dialog.items.len().saturating_sub(1));
                TuiEffect::None
            }
            KeyCode::Enter => {
                let selected = dialog.selected;
                self.app.dialog = None;
                let mode = self.dialog_mode.take();
                match mode {
                    Some(DialogMode::Model) => self
                        .available_models
                        .get(selected)
                        .cloned()
                        .map(|model| {
                            self.app.model = model.clone();
                            TuiEffect::SelectModel(model)
                        })
                        .unwrap_or(TuiEffect::None),
                    Some(DialogMode::Resume) => self
                        .sessions
                        .get(selected)
                        .map(|session| TuiEffect::ResumeSession(session.id.clone()))
                        .unwrap_or(TuiEffect::None),
                    Some(DialogMode::CommandCompletion) => {
                        self.apply_command_completion(selected);
                        TuiEffect::None
                    }
                    Some(DialogMode::Help) | None => TuiEffect::None,
                }
            }
            _ => TuiEffect::None,
        }
    }

    fn submit_input(&mut self) -> TuiEffect {
        let input = std::mem::take(&mut self.app.input);
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
            }
        } else {
            self.history_cursor = None;
            self.app.input.clear();
        }
    }

    fn dispatch_slash(&mut self, command: &str) -> TuiEffect {
        match commands::resolve_slash(command) {
            Some(CommandKind::Model) => {
                self.open_model_dialog();
                TuiEffect::None
            }
            Some(CommandKind::Resume) => {
                self.open_resume_dialog();
                TuiEffect::None
            }
            Some(CommandKind::NewSession) => TuiEffect::NewSession,
            Some(CommandKind::Help) => {
                self.open_help_dialog();
                TuiEffect::None
            }
            None if command.trim().is_empty() => TuiEffect::None,
            None => {
                let unknown = command.split_whitespace().next().unwrap_or_default();
                TuiEffect::SystemMessage(format!("unknown command /{unknown}; try /help"))
            }
        }
    }

    fn complete_slash_input(&mut self) -> TuiEffect {
        let items = commands::completion_items(&self.app.input);
        match items.len() {
            0 => TuiEffect::None,
            1 => {
                if let Some(item) = items.first() {
                    self.app.input = format!("{} ", item.label);
                }
                TuiEffect::None
            }
            _ => {
                self.open_command_completion_dialog(items);
                TuiEffect::None
            }
        }
    }

    fn apply_command_completion(&mut self, selected: usize) {
        let items = commands::completion_items(&self.app.input);
        if let Some(item) = items.get(selected) {
            self.app.input = format!("{} ", item.label);
        }
    }

    fn open_command_completion_dialog(&mut self, items: Vec<DialogItem>) {
        self.app.dialog = Some(DialogView {
            title: "commands".to_string(),
            subtitle: "select a slash command".to_string(),
            items,
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::CommandCompletion);
    }

    fn open_model_dialog(&mut self) {
        let items = self
            .available_models
            .iter()
            .map(|model| DialogItem {
                label: model.clone(),
                detail: if *model == self.app.model {
                    "current".to_string()
                } else {
                    "available".to_string()
                },
            })
            .collect::<Vec<_>>();
        let selected = items
            .iter()
            .position(|item| item.label == self.app.model)
            .unwrap_or(0);
        self.app.dialog = Some(DialogView {
            title: "select model".to_string(),
            subtitle: "next turn uses the selected model".to_string(),
            items,
            selected,
        });
        self.dialog_mode = Some(DialogMode::Model);
    }

    fn open_resume_dialog(&mut self) {
        let items = if self.sessions.is_empty() {
            vec![DialogItem {
                label: "no sessions".to_string(),
                detail: "start with /new or send a prompt".to_string(),
            }]
        } else {
            self.sessions
                .iter()
                .map(|session| DialogItem {
                    label: session.title.clone(),
                    detail: session.detail.clone(),
                })
                .collect()
        };
        self.app.dialog = Some(DialogView {
            title: "resume session".to_string(),
            subtitle: "select a previous conversation".to_string(),
            items,
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::Resume);
    }

    fn open_help_dialog(&mut self) {
        self.app.dialog = Some(DialogView {
            title: "commands".to_string(),
            subtitle: "slash commands and shortcuts".to_string(),
            items: commands::help_items(),
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::Help);
    }
}

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
    fn ctrl_c_clears_input_without_exit() {
        let mut controller = Controller::new(AppState {
            input: "hello".to_string(),
            ..AppState::default()
        });

        assert_eq!(controller.handle_key(ctrl_c()), TuiEffect::None);
        assert_eq!(controller.app.input, "");
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
    fn ctrl_c_exits_only_when_idle_empty_and_no_dialog() {
        let mut controller = Controller::new(AppState::default());

        assert_eq!(controller.handle_key(ctrl_c()), TuiEffect::Exit);
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
    fn tab_completes_slash_command_prefixes() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/m");

        assert_eq!(controller.handle_key(key(KeyCode::Tab)), TuiEffect::None);
        assert_eq!(controller.app.input, "/model ");
        assert!(controller.app.dialog.is_none());
    }

    #[test]
    fn tab_opens_slash_completion_when_prefix_is_ambiguous() {
        let mut controller = Controller::new(AppState::default());

        type_text(&mut controller, "/");

        assert_eq!(controller.handle_key(key(KeyCode::Tab)), TuiEffect::None);
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
