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
    p.message_mut(msg)?
        .parts
        .iter_mut()
        .find(|x| x.id() == part)
}

pub(super) fn upsert_tool(
    p: &mut Projection,
    msg: MessageId,
    part: PartId,
    call: ToolCallId,
    name: ToolName,
    state: ToolPartState,
) {
    if let Some(PartProjection::Tool {
        state: existing, ..
    }) = find_part(p, msg, part)
    {
        *existing = state;
    } else {
        push_part(
            p,
            msg,
            PartProjection::Tool {
                id: part,
                call,
                name,
                state,
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
