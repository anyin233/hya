use super::*;

impl Controller {
    pub(super) fn dispatch_slash(&mut self, command: &str) -> TuiEffect {
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

    pub(super) fn dispatch_palette_command(&mut self, label: Option<&str>) -> TuiEffect {
        let Some(command) = label.and_then(|label| label.strip_prefix('/')) else {
            return TuiEffect::None;
        };
        self.dispatch_slash(command)
    }
}
