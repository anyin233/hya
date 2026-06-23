use super::*;
use yaca_tui::{DialogItem, DialogView};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DialogMode {
    Model,
    Agent,
    Resume,
    Help,
    Tools,
    Think,
    CommandPalette,
    CommandCompletion,
    ReferenceCompletion,
}

impl Controller {
    pub(super) fn handle_dialog_key(&mut self, key: KeyEvent) -> TuiEffect {
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
                let selected_label = dialog.items.get(selected).map(|item| item.label.clone());
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
                    Some(DialogMode::CommandPalette) => {
                        self.dispatch_palette_command(selected_label.as_deref())
                    }
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

    pub(super) fn open_command_palette_dialog(&mut self) {
        self.app.dialog = Some(DialogView {
            title: "commands".to_string(),
            subtitle: "select a command; enter runs".to_string(),
            items: commands::palette_items_with_custom(&self.custom_commands),
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::CommandPalette);
    }

    pub(super) fn open_command_completion_dialog(&mut self, items: Vec<DialogItem>) {
        self.app.dialog = Some(DialogView {
            title: "commands".to_string(),
            subtitle: "select a slash command".to_string(),
            items,
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::CommandCompletion);
    }

    pub(super) fn open_reference_completion_dialog(&mut self, items: Vec<DialogItem>) {
        self.app.dialog = Some(DialogView {
            title: "references".to_string(),
            subtitle: "select a file or reference".to_string(),
            items,
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::ReferenceCompletion);
    }
}
