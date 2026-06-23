use super::dialogs::DialogMode;
use super::*;
use yaca_tui::{DialogItem, DialogView};

impl Controller {
    pub(super) fn open_model_dialog(&mut self) {
        self.open_model_dialog_for_provider(None);
    }

    pub(super) fn open_model_dialog_for_provider(&mut self, provider: Option<&str>) {
        let current_provider = self.app.model_provider_label.as_deref();
        let mut items = self
            .available_models
            .iter()
            .filter(|entry| provider.is_none_or(|provider| entry.provider == provider))
            .map(|entry| DialogItem {
                label: entry.id.clone(),
                detail: if entry.id == self.app.model
                    && Some(entry.provider.as_str()) == current_provider
                {
                    format!("{} · current", entry.provider)
                } else {
                    entry.provider.clone()
                },
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            items.push(DialogItem {
                label: "connect provider".to_string(),
                detail: "configure a provider or run yaca login, then restart the TUI".to_string(),
            });
        }
        let selected = items
            .iter()
            .position(|item| {
                item.label == self.app.model
                    && item
                        .detail
                        .split_once(" · ")
                        .map_or(item.detail.as_str(), |(provider, _)| provider)
                        == current_provider.unwrap_or_default()
            })
            .unwrap_or(0);
        self.app.dialog = Some(DialogView {
            title: provider.unwrap_or("select model").to_string(),
            subtitle: if provider.is_some() {
                "select a model from this provider".to_string()
            } else {
                "next turn uses the selected model".to_string()
            },
            items,
            selected,
        });
        self.dialog_mode = Some(DialogMode::Model);
    }

    pub(super) fn open_provider_dialog(&mut self) {
        let mut providers = self
            .available_models
            .iter()
            .map(|entry| entry.provider.clone())
            .collect::<Vec<_>>();
        providers.sort();
        providers.dedup();
        let items = if providers.is_empty() {
            vec![DialogItem {
                label: "configure provider".to_string(),
                detail: "configure ~/.config/yaca/config.yaml or run yaca login".to_string(),
            }]
        } else {
            providers
                .into_iter()
                .map(|provider| DialogItem {
                    label: provider,
                    detail: "configured provider".to_string(),
                })
                .collect()
        };
        self.app.dialog = Some(DialogView {
            title: "Connect a provider".to_string(),
            subtitle: "select or configure an AI provider".to_string(),
            items,
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::Provider);
    }

    pub(super) fn open_resume_dialog(&mut self) {
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

    pub(super) fn open_agent_dialog(&mut self) {
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

    pub(super) fn open_tools_dialog(&mut self) {
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

    pub(super) fn open_think_dialog(&mut self) {
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

    pub(super) fn open_help_dialog(&mut self) {
        self.app.dialog = Some(DialogView {
            title: "commands".to_string(),
            subtitle: "slash commands and shortcuts".to_string(),
            items: commands::help_items_with_custom(&self.custom_commands),
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::Help);
    }
}
