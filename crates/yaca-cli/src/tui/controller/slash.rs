use super::super::model_identity;
use super::*;

impl Controller {
    pub(super) fn dispatch_slash(&mut self, command: &str) -> TuiEffect {
        let mut pieces = command.splitn(2, char::is_whitespace);
        let name = pieces.next().unwrap_or_default();
        let arguments = pieces.next().unwrap_or_default().trim();
        match commands::resolve_slash(command) {
            Some(CommandKind::Model) if !arguments.is_empty() => {
                match model_identity::resolve_model_argument(&self.available_models, arguments) {
                    Ok(identity) => {
                        self.app
                            .set_model_identity(identity.model.clone(), identity.provider.clone());
                        TuiEffect::SelectModel {
                            model: identity.model,
                            provider: identity.provider,
                        }
                    }
                    Err(ambiguous) => TuiEffect::SystemMessage(format!(
                        "ambiguous model {}; use provider/model",
                        ambiguous.model
                    )),
                }
            }
            Some(CommandKind::Model) => {
                self.open_model_dialog();
                TuiEffect::None
            }
            Some(CommandKind::Connect) => {
                self.open_provider_dialog();
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
            Some(CommandKind::Skills) => {
                self.open_skills_dialog();
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

    pub(super) fn dispatch_palette_command(&mut self, label: Option<&str>) -> TuiEffect {
        let Some(command) = label.and_then(|label| label.strip_prefix('/')) else {
            return TuiEffect::None;
        };
        self.dispatch_slash(command)
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use yaca_tui::AppState;

    use crate::config::ModelEntry;

    use super::super::{Controller, TuiEffect};

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

    fn duplicate_model_controller() -> Controller {
        Controller::with_models_and_sessions(
            AppState {
                model: "claude-sonnet".to_string(),
                model_provider_label: Some("anthropic".to_string()),
                ..AppState::default()
            },
            vec![
                ModelEntry {
                    id: "gpt-5".to_string(),
                    provider: "anthropic".to_string(),
                },
                ModelEntry {
                    id: "gpt-5".to_string(),
                    provider: "openai".to_string(),
                },
                ModelEntry {
                    id: "claude-sonnet".to_string(),
                    provider: "anthropic".to_string(),
                },
            ],
            Vec::new(),
        )
    }

    #[test]
    fn slash_model_accepts_provider_qualified_model_identity() {
        // Given
        let mut controller = duplicate_model_controller();

        // When
        type_text(&mut controller, "/model openai/gpt-5");
        let effect = controller.handle_key(key(KeyCode::Enter));

        // Then
        assert_eq!(
            effect,
            TuiEffect::SelectModel {
                model: "gpt-5".to_string(),
                provider: Some("openai".to_string()),
            }
        );
        assert_eq!(controller.app.model, "gpt-5");
        assert_eq!(
            controller.app.model_provider_label.as_deref(),
            Some("openai")
        );
    }

    #[test]
    fn slash_model_rejects_ambiguous_bare_model_identity() {
        // Given
        let mut controller = duplicate_model_controller();

        // When
        type_text(&mut controller, "/model gpt-5");
        let effect = controller.handle_key(key(KeyCode::Enter));

        // Then
        assert_eq!(
            effect,
            TuiEffect::SystemMessage("ambiguous model gpt-5; use provider/model".to_string())
        );
        assert_eq!(controller.app.model, "claude-sonnet");
        assert_eq!(
            controller.app.model_provider_label.as_deref(),
            Some("anthropic")
        );
    }
}
