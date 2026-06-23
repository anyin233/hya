use yaca_tui::{AppState, KeyBindingGroup, KeyBindingItem, KeyBindingsView};

use super::{Controller, TuiEffect};
use crate::tui::leader_key::LeaderAction;

impl Controller {
    pub(super) fn handle_leader_action(&mut self, action: LeaderAction) -> TuiEffect {
        match action {
            LeaderAction::Arm => {
                self.app.keybindings = Some(leader_keybindings_view(&self.app));
                TuiEffect::None
            }
            LeaderAction::Cancel => {
                self.app.keybindings = None;
                TuiEffect::None
            }
            LeaderAction::ModelList => {
                self.clear_keybindings();
                self.open_model_dialog();
                TuiEffect::None
            }
            LeaderAction::AgentList => {
                self.clear_keybindings();
                self.open_agent_dialog();
                TuiEffect::None
            }
            LeaderAction::SessionList => {
                self.clear_keybindings();
                self.open_resume_dialog();
                TuiEffect::None
            }
            LeaderAction::SessionNew => {
                self.clear_keybindings();
                TuiEffect::NewSession
            }
            LeaderAction::SessionCompact => {
                self.clear_keybindings();
                TuiEffect::CompactTranscript
            }
            LeaderAction::SidebarToggle => {
                self.clear_keybindings();
                self.app.sidebar_hidden = !self.app.sidebar_hidden;
                TuiEffect::None
            }
            LeaderAction::StatusView => {
                self.clear_keybindings();
                self.open_tools_dialog();
                TuiEffect::None
            }
            LeaderAction::SubagentsView => {
                self.clear_keybindings();
                if self.app.has_active_team_members() {
                    self.open_subagents_dialog();
                } else {
                    self.open_tools_dialog();
                }
                TuiEffect::None
            }
            LeaderAction::SessionExport => {
                self.clear_keybindings();
                TuiEffect::ExportTranscript
            }
            LeaderAction::Exit => {
                self.clear_keybindings();
                TuiEffect::Exit
            }
        }
    }

    fn clear_keybindings(&mut self) {
        self.app.keybindings = None;
    }
}

fn leader_keybindings_view(app: &AppState) -> KeyBindingsView {
    let down_label = if app.has_active_team_members() {
        "View subagents"
    } else {
        "Status"
    };
    KeyBindingsView {
        title: "Key Bindings".to_string(),
        groups: vec![
            group(
                "System",
                [
                    ("b", "Toggle sidebar"),
                    ("s", "Status"),
                    ("↓", down_label),
                    ("q", "Exit"),
                ],
            ),
            group(
                "Session",
                [
                    ("l", "Resume session"),
                    ("n", "New session"),
                    ("c", "Compact context"),
                    ("x", "Export transcript"),
                ],
            ),
            group("Model", [("m", "Select model")]),
            group("Agent", [("a", "Select agent")]),
        ],
    }
}

fn group<const N: usize>(label: &str, items: [(&str, &str); N]) -> KeyBindingGroup {
    KeyBindingGroup {
        label: label.to_string(),
        items: items
            .into_iter()
            .map(|(key, item_label)| KeyBindingItem {
                key: key.to_string(),
                label: item_label.to_string(),
            })
            .collect(),
    }
}
