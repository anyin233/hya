use crate::ids::{MessageId, PartId, ToolCallId};
use crate::message::ToolPartState;
use crate::model::ToolName;
use crate::projection::{PartProjection, Projection};

pub(super) fn push_part(p: &mut Projection, msg: MessageId, part: PartProjection) {
    if let Some(m) = p.message_mut(msg)
        && !m.parts.iter().any(|x| x.id() == part.id())
    {
        m.parts.push(part);
    }
}

pub(super) fn find_part(
    p: &mut Projection,
    msg: MessageId,
    part: PartId,
) -> Option<&mut PartProjection> {
    // Deltas target the newest part; part ids are unique within a message.
    p.message_mut(msg)?
        .parts
        .iter_mut()
        .rev()
        .find(|x| x.id() == part)
}

pub(super) fn upsert_tool(
    p: &mut Projection,
    msg: MessageId,
    part: PartId,
    call: ToolCallId,
    name: ToolName,
    state: ToolPartState,
    provider_data: Option<serde_json::Value>,
) {
    if let Some(PartProjection::Tool {
        state: existing,
        provider_data: existing_data,
        ..
    }) = find_part(p, msg, part)
    {
        *existing = state;
        if provider_data.is_some() {
            *existing_data = provider_data;
        }
    } else {
        push_part(
            p,
            msg,
            PartProjection::Tool {
                id: part,
                call,
                name,
                state,
                eviction_notice: None,
                provider_data,
            },
        );
    }
}

pub(super) fn tool_input(state: &ToolPartState) -> serde_json::Value {
    match state {
        ToolPartState::Pending { input }
        | ToolPartState::Running { input }
        | ToolPartState::Completed { input, .. }
        | ToolPartState::Error { input, .. } => input.clone(),
    }
}
