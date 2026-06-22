use std::collections::BTreeMap;

use yaca_proto::{Event, MessageId, PartId, SessionId};

use super::SessionEngine;
use crate::hooks::{TextCompleteInput, TextCompleteOutcome};

#[derive(Default)]
pub(super) struct TextPartAccumulator {
    parts: BTreeMap<PartId, String>,
}

impl TextPartAccumulator {
    pub(super) fn apply(&mut self, event: &Event) -> Option<(PartId, String)> {
        match event {
            Event::TextStart { part, .. } => {
                self.parts.insert(*part, String::new());
                None
            }
            Event::TextDelta { part, delta, .. } => {
                if let Some(text) = self.parts.get_mut(part) {
                    text.push_str(delta);
                }
                None
            }
            Event::TextEnd { part, .. } => self.parts.get(part).map(|text| (*part, text.clone())),
            Event::SessionCreated { .. }
            | Event::SessionTitled { .. }
            | Event::SessionMetadataSet { .. }
            | Event::SessionPermissionSet { .. }
            | Event::SessionArchived { .. }
            | Event::SessionShareSet { .. }
            | Event::SessionShareCleared { .. }
            | Event::AgentSwitched { .. }
            | Event::ModelSwitched { .. }
            | Event::CommandExecuted { .. }
            | Event::MessageStarted { .. }
            | Event::MessageFinished { .. }
            | Event::MessageDeleted { .. }
            | Event::PartDeleted { .. }
            | Event::StepStarted { .. }
            | Event::StepFinished { .. }
            | Event::TextReplace { .. }
            | Event::ReasoningStart { .. }
            | Event::ReasoningDelta { .. }
            | Event::ReasoningEnd { .. }
            | Event::ReasoningReplace { .. }
            | Event::ToolInputStart { .. }
            | Event::ToolInputDelta { .. }
            | Event::ToolCallRequested { .. }
            | Event::ToolResult { .. }
            | Event::ToolError { .. }
            | Event::ToolPartUpdated { .. }
            | Event::Error { .. } => None,
        }
    }

    pub(super) fn replace(&mut self, part: PartId, text: String) {
        self.parts.insert(part, text);
    }
}

impl SessionEngine {
    pub(super) async fn complete_text_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Option<String> {
        let hooks = self.hooks.as_ref()?;
        let original = text.clone();
        let TextCompleteOutcome::Continue { text } = hooks
            .text_complete(TextCompleteInput {
                session,
                message,
                part,
                text,
            })
            .await;
        (text != original).then_some(text)
    }
}
