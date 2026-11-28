use super::Projection;
use crate::event::StepCut;
use crate::ids::MessageId;
use crate::message::{TokenUsage, UsagePurpose};
use crate::model::ModelRef;
use crate::usage::MessageUsage;

impl Projection {
    pub(super) fn apply_context_compacted(
        &mut self,
        marker: &MessageId,
        to_message: &MessageId,
        step_cut: &Option<StepCut>,
    ) {
        // MessageStarted is append-only, so the marker is recorded after
        // the in-flight assistant message. Move it to the boundary named
        // by the compaction record; otherwise request reconstruction
        // drops the assistant's retained tail on the next round. A step
        // cut keeps the turn's head and the in-flight message after the
        // marker, minus the folded leading parts.
        if let Some(marker_index) = self
            .session
            .messages
            .iter()
            .position(|row| row.id == *marker)
        {
            let marker_row = self.session.messages.remove(marker_index);
            let boundary = match step_cut {
                Some(cut) => {
                    if let Some(row) = self.message_mut(*to_message)
                        && let Some(at) = row
                            .parts
                            .iter()
                            .position(|part| part.id() == cut.through_part)
                    {
                        row.folded_parts = at + 1;
                    }
                    self.session
                        .messages
                        .iter()
                        .position(|row| row.id == cut.kept_head)
                }
                None => self
                    .session
                    .messages
                    .iter()
                    .position(|row| row.id == *to_message)
                    .map(|to_index| to_index + 1),
            };
            if let Some(boundary) = boundary {
                self.session.messages.insert(boundary, marker_row);
            } else {
                self.session
                    .messages
                    .insert(marker_index.min(self.session.messages.len()), marker_row);
            }
        }
    }

    pub(super) fn apply_usage_recorded(
        &mut self,
        message: &Option<MessageId>,
        model: &ModelRef,
        purpose: &UsagePurpose,
        tokens: &TokenUsage,
    ) {
        self.session.usage.record(model, *purpose, tokens);
        if let Some(m) = message.and_then(|message| self.message_mut(message)) {
            match &mut m.usage {
                Some(existing) => existing.add(model, tokens),
                None => m.usage = Some(MessageUsage::first(model, tokens)),
            }
        }
    }
}
