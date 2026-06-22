use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use yaca_tui::{AppState, DialogItem, DialogView};

use super::agent_cycle::next_agent_label;
use super::block_action::{SelectedBlockAction, selected_block_action};
use super::commands::{self, CommandKind, CustomCommand};
use super::prompt::{PromptState, mention_trigger_index};
use super::selection::{MessageSelectionStep, next_selected_message};
use crate::config::ModelEntry;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogMode {
    Model,
    Agent,
    Resume,
    Help,
    Tools,
    Think,
    CommandCompletion,
    ReferenceCompletion,
}

fn provider_label_for_model(models: &[ModelEntry], model: &str) -> Option<String> {
    models
        .iter()
        .find(|entry| entry.id == model)
        .map(|entry| entry.provider.clone())
        .filter(|label| !label.trim().is_empty())
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
            last_ctrl_c: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TuiEffect {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return self.handle_ctrl_c();
        }
        if self.app.dialog.is_some() {
            return self.handle_dialog_key(key);
        }
        if let Some(action) =
            selected_block_action(self.app.selected_message, &self.app.input, &key)
        {
            return TuiEffect::SelectedBlock(action);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Up => return self.select_previous_message(),
                KeyCode::Down => return self.select_next_message(),
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
            KeyCode::Esc if self.app.running => TuiEffect::Interrupt,
            KeyCode::F(2) => {
                self.open_model_dialog();
                TuiEffect::None
            }
            KeyCode::Tab => {
                next_agent_label(&self.app.agent, &self.agents).map_or(TuiEffect::None, |agent| {
                    self.app.agent = agent.clone();
                    self.disarm_exit();
                    TuiEffect::SelectAgent(agent)
                })
            }
            KeyCode::Enter => self.submit_input(),
            KeyCode::Backspace => {
                self.app.input.pop();
                self.history_cursor = None;
                self.disarm_exit();
                self.refresh_inline_popup();
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
                self.disarm_exit();
                self.refresh_inline_popup();
                TuiEffect::None
            }
            _ => TuiEffect::None,
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

    fn handle_dialog_key(&mut self, key: KeyEvent) -> TuiEffect {
        if matches!(
            self.dialog_mode,
            Some(DialogMode::CommandCompletion | DialogMode::ReferenceCompletion)
        ) {
            return self.handle_completion_popup_key(key);
        }
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
                        .map(|entry| {
                            let model = entry.id.clone();
                            self.app
                                .set_model_identity(model.clone(), Some(entry.provider.clone()));
                            TuiEffect::SelectModel(model)
                        })
                        .unwrap_or(TuiEffect::None),
                    Some(DialogMode::Resume) => self
                        .sessions
                        .get(selected)
                        .map(|session| TuiEffect::ResumeSession(session.id.clone()))
                        .unwrap_or(TuiEffect::None),
                    Some(DialogMode::Agent) => self
                        .agents
                        .get(selected)
                        .map(|agent| {
                            self.app.agent = agent.label.clone();
                            TuiEffect::SelectAgent(agent.label.clone())
                        })
                        .unwrap_or(TuiEffect::None),
                    Some(DialogMode::Think) => ["off", "low", "medium", "high"]
                        .get(selected)
                        .map(|level| TuiEffect::SelectReasoning((*level).to_string()))
                        .unwrap_or(TuiEffect::None),
                    Some(DialogMode::CommandCompletion) => {
                        self.apply_command_completion(selected);
                        TuiEffect::None
                    }
                    Some(
                        DialogMode::Help | DialogMode::Tools | DialogMode::ReferenceCompletion,
                    )
                    | None => TuiEffect::None,
                }
            }
            _ => TuiEffect::None,
        }
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
                self.refresh_inline_popup();
            }
        } else {
            self.history_cursor = None;
            self.app.input.clear();
            self.refresh_inline_popup();
        }
    }

    fn dispatch_slash(&mut self, command: &str) -> TuiEffect {
        let mut pieces = command.splitn(2, char::is_whitespace);
        let name = pieces.next().unwrap_or_default();
        let arguments = pieces.next().unwrap_or_default().trim();
        match commands::resolve_slash(command) {
            Some(CommandKind::Model) if !arguments.is_empty() => {
                let model = arguments.to_string();
                let provider = provider_label_for_model(&self.available_models, &model);
                self.app.set_model_identity(model.clone(), provider);
                TuiEffect::SelectModel(model)
            }
            Some(CommandKind::Model) => {
                self.open_model_dialog();
                TuiEffect::None
            }
            Some(CommandKind::Resume) => {
                self.open_resume_dialog();
                TuiEffect::None
            }
            Some(CommandKind::NewSession) => TuiEffect::NewSession,
            Some(CommandKind::Compact) => TuiEffect::CompactTranscript,
            Some(CommandKind::Init) => TuiEffect::InitProject,
            Some(CommandKind::Agent) => {
                self.open_agent_dialog();
                TuiEffect::None
            }
            Some(CommandKind::Tools) => {
                self.open_tools_dialog();
                TuiEffect::None
            }
            Some(CommandKind::Yolo) => {
                self.app.yolo = match arguments {
                    "on" | "true" => true,
                    "off" | "false" => false,
                    _ => !self.app.yolo,
                };
                let state = if self.app.yolo { "enabled" } else { "disabled" };
                TuiEffect::SystemMessage(format!("yolo mode {state}"))
            }
            Some(CommandKind::Think) if !arguments.is_empty() => {
                TuiEffect::SelectReasoning(arguments.to_string())
            }
            Some(CommandKind::Think) => {
                self.open_think_dialog();
                TuiEffect::None
            }
            Some(CommandKind::Export) => TuiEffect::ExportTranscript,
            Some(CommandKind::Quit) => TuiEffect::Exit,
            Some(CommandKind::Help) => {
                self.open_help_dialog();
                TuiEffect::None
            }
            None if command.trim().is_empty() => TuiEffect::None,
            None => {
                if let Some(custom) = commands::find_custom(&self.custom_commands, name) {
                    let prompt = custom.expand(arguments);
                    if custom.agent.is_some() || custom.model.is_some() {
                        TuiEffect::SubmitConfigured {
                            prompt,
                            agent: custom.agent.clone(),
                            model: custom.model.clone(),
                        }
                    } else {
                        TuiEffect::Submit(prompt)
                    }
                } else {
                    TuiEffect::SystemMessage(format!("unknown command /{name}; try /help"))
                }
            }
        }
    }

    fn apply_command_completion(&mut self, selected: usize) {
        let items = commands::completion_items_with_custom(&self.app.input, &self.custom_commands);
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

    fn open_reference_completion_dialog(&mut self, items: Vec<DialogItem>) {
        self.app.dialog = Some(DialogView {
            title: "references".to_string(),
            subtitle: "select a file or reference".to_string(),
            items,
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::ReferenceCompletion);
    }

    fn handle_completion_popup_key(&mut self, key: KeyEvent) -> TuiEffect {
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
            KeyCode::Backspace => {
                self.app.input.pop();
                self.refresh_inline_popup();
                TuiEffect::None
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.app.input.push(c);
                self.refresh_inline_popup();
                TuiEffect::None
            }
            _ => TuiEffect::None,
        }
    }

    fn complete_popup_selection(&mut self, selected: usize) {
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

    fn complete_reference(&mut self, label: &str) {
        let Some(idx) = mention_trigger_index(&self.app.input) else {
            return;
        };
        self.app.input.truncate(idx);
        self.app.input.push_str(label);
        self.app.input.push(' ');
    }

    fn refresh_inline_popup(&mut self) {
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
        if let Some(idx) = mention_trigger_index(&self.app.input) {
            let prefix = &self.app.input[idx + 1..];
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

    fn open_model_dialog(&mut self) {
        let items = self
            .available_models
            .iter()
            .map(|entry| DialogItem {
                label: entry.id.clone(),
                detail: if entry.id == self.app.model {
                    format!("{} · current", entry.provider)
                } else {
                    entry.provider.clone()
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

    fn open_agent_dialog(&mut self) {
        let items = if self.agents.is_empty() {
            vec![DialogItem {
                label: "build".to_string(),
                detail: "default coding agent".to_string(),
            }]
        } else {
            self.agents.clone()
        };
        self.app.dialog = Some(DialogView {
            title: "agents".to_string(),
            subtitle: "select active agent profile".to_string(),
            items,
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::Agent);
    }

    fn open_tools_dialog(&mut self) {
        self.app.dialog = Some(DialogView {
            title: "tools".to_string(),
            subtitle: "builtin tools and MCP status".to_string(),
            items: vec![
                DialogItem {
                    label: "read".to_string(),
                    detail: "builtin · auto-allowed".to_string(),
                },
                DialogItem {
                    label: "write".to_string(),
                    detail: "builtin · asks permission".to_string(),
                },
                DialogItem {
                    label: "edit".to_string(),
                    detail: "builtin · asks permission".to_string(),
                },
                DialogItem {
                    label: "glob".to_string(),
                    detail: "builtin · auto-allowed".to_string(),
                },
                DialogItem {
                    label: "grep".to_string(),
                    detail: "builtin · auto-allowed".to_string(),
                },
                DialogItem {
                    label: "shell".to_string(),
                    detail: "builtin · asks permission".to_string(),
                },
                DialogItem {
                    label: "mcp".to_string(),
                    detail: "configured MCP tools · permission-gated".to_string(),
                },
            ],
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::Tools);
    }

    fn open_think_dialog(&mut self) {
        let current = self.app.reasoning_effort.as_deref().unwrap_or("off");
        let items = ["off", "low", "medium", "high"]
            .iter()
            .map(|level| DialogItem {
                label: (*level).to_string(),
                detail: if *level == current {
                    "current".to_string()
                } else {
                    "reasoning effort".to_string()
                },
            })
            .collect::<Vec<_>>();
        let selected = items
            .iter()
            .position(|item| item.label == current)
            .unwrap_or(0);
        self.app.dialog = Some(DialogView {
            title: "reasoning effort".to_string(),
            subtitle: "future turns use the selected thinking level".to_string(),
            items,
            selected,
        });
        self.dialog_mode = Some(DialogMode::Think);
    }

    fn open_help_dialog(&mut self) {
        self.app.dialog = Some(DialogView {
            title: "commands".to_string(),
            subtitle: "slash commands and shortcuts".to_string(),
            items: commands::help_items_with_custom(&self.custom_commands),
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::Help);
    }
}

fn is_exact_slash_command(input: &str, custom_commands: &[CustomCommand]) -> bool {
    input.strip_prefix('/').is_some_and(|command| {
        commands::resolve_slash(command).is_some()
            || commands::find_custom(custom_commands, command).is_some()
    })
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
    fn tab_cycles_agents_when_no_popup_is_active() {
        let mut controller = Controller::new(AppState {
            agent: "build".to_string(),
            ..AppState::default()
        });
        controller.set_agents(
            ["build", "plan"]
                .into_iter()
                .map(|label| DialogItem {
                    label: label.to_string(),
                    detail: "agent profile".to_string(),
                })
                .collect(),
        );

        assert_eq!(
            controller.handle_key(key(KeyCode::Tab)),
            TuiEffect::SelectAgent("plan".to_string())
        );
        assert_eq!(controller.app.agent, "plan");
        assert!(!controller.app.yolo);
        assert_eq!(
            controller.handle_key(key(KeyCode::Tab)),
            TuiEffect::SelectAgent("build".to_string())
        );
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
