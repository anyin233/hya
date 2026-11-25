use serde::{Deserialize, Serialize};

use super::{PartProjection, Projection};
use crate::event::{EvictedPart, StepCut};
use crate::ids::MessageId;
use crate::message::{TokenUsage, UsagePurpose};
use crate::model::ModelRef;
use crate::tokens::{TokenAccountingMode, TokenSource};
use crate::usage::MessageUsage;

/// Latest window-occupancy report, mirroring the newest `ContextStatus` event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextStatusProjection {
    /// Tokens the last measured request is believed to occupy.
    pub tokens: u64,
    /// Whether the count was provider-anchored or locally estimated.
    pub source: TokenSource,
    /// Accounting mode in force for that round.
    pub mode: TokenAccountingMode,
    /// Resolved compaction threshold the count was judged against.
    pub threshold: u64,
}

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

    pub(super) fn apply_context_evicted(&mut self, parts: &[EvictedPart]) {
        for evicted in parts {
            if let Some(PartProjection::Tool {
                eviction_notice, ..
            }) = self
                .session
                .messages
                .iter_mut()
                .rev()
                .flat_map(|row| row.parts.iter_mut())
                .find(|part| part.id() == evicted.part)
            {
                *eviction_notice = Some(evicted.notice.clone());
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

    pub(super) fn apply_context_status(
        &mut self,
        tokens: &u64,
        source: &TokenSource,
        mode: &TokenAccountingMode,
        threshold: &u64,
    ) {
        self.session.context_status = Some(ContextStatusProjection {
            tokens: *tokens,
            source: *source,
            mode: *mode,
            threshold: *threshold,
        });
    }
}
