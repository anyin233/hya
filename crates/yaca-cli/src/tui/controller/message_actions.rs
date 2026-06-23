use super::dialogs::DialogMode;
use super::{Controller, TuiEffect};
use crate::tui::block_action::{SelectedBlockAction, SelectedBlockActionKind};
use yaca_proto::{MessageProjection, PartProjection};
use yaca_tui::{DialogItem, DialogView};

impl Controller {
    pub(super) fn open_message_actions_dialog(&mut self) {
        self.app.dialog = Some(DialogView {
            title: "Message Actions".to_string(),
            subtitle: "selected message operations".to_string(),
            items: vec![
                DialogItem {
                    label: "Revert".to_string(),
                    detail: "undo messages and file changes".to_string(),
                },
                DialogItem {
                    label: "Copy".to_string(),
                    detail: "message text to clipboard".to_string(),
                },
                DialogItem {
                    label: "Fork".to_string(),
                    detail: "create a new session".to_string(),
                },
            ],
            selected: 0,
        });
        self.dialog_mode = Some(DialogMode::MessageActions);
    }

    pub(super) fn dispatch_message_action(
        &self,
        label: Option<&str>,
        selected: usize,
    ) -> TuiEffect {
        let Some(message_index) = self.app.selected_message else {
            return TuiEffect::None;
        };
        if matches!(label, Some("Copy")) || selected == 1 {
            return selected_message_text(&self.app.projection.session.messages, message_index)
                .map_or(TuiEffect::None, TuiEffect::CopyMessage);
        }
        let kind = match label {
            Some("Revert") => Some(SelectedBlockActionKind::Revert),
            Some("Fork") => Some(SelectedBlockActionKind::Branch),
            _ if selected == 0 => Some(SelectedBlockActionKind::Revert),
            _ if selected == 2 => Some(SelectedBlockActionKind::Branch),
            _ => None,
        };
        kind.map_or(TuiEffect::None, |kind| {
            TuiEffect::SelectedBlock(SelectedBlockAction {
                kind,
                message_index,
            })
        })
    }
}

fn selected_message_text(messages: &[MessageProjection], index: usize) -> Option<String> {
    let message = messages.get(index)?;
    let text = message
        .parts
        .iter()
        .filter_map(|part| match part {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            PartProjection::Reasoning { .. } | PartProjection::Tool { .. } => None,
        })
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}
