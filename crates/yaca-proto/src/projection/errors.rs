use crate::ids::{EventSeq, MessageId, PartId};
use crate::message::Role;

use super::{MessageProjection, PartProjection, Projection};

pub(super) fn push_error(p: &mut Projection, seq: EventSeq, code: &str, message: &str) {
    let message_id = synthetic_message_id(seq);
    if p.session.messages.iter().any(|m| m.id == message_id) {
        return;
    }

    p.session.messages.push(MessageProjection {
        id: message_id,
        role: Role::System,
        started_millis: None,
        completed_millis: None,
        finish: None,
        parts: vec![PartProjection::Text {
            id: synthetic_part_id(seq),
            text: format!("error: {code}: {message}"),
        }],
    });
}

fn synthetic_message_id(seq: EventSeq) -> MessageId {
    MessageId::from_uuid(synthetic_uuid(seq, b'm'))
}

fn synthetic_part_id(seq: EventSeq) -> PartId {
    PartId::from_uuid(synthetic_uuid(seq, b'p'))
}

fn synthetic_uuid(seq: EventSeq, kind: u8) -> uuid::Uuid {
    let mut bytes = *b"yacaerr\0\0\0\0\0\0\0\0\0";
    bytes[7] = kind;
    bytes[8..].copy_from_slice(&seq.0.to_be_bytes());
    uuid::Uuid::from_bytes(bytes)
}
