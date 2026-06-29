#![allow(clippy::expect_used)]

use hya_proto::{Envelope, Event, EventSeq};
use hya_proto::{FinishReason, MessageId, PartId, PartProjection, Projection, Role, SessionId};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

#[test]
fn live_zero_seq_events_apply_without_advancing_durable_cursor() {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let mut projection = Projection::default();

    projection.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    projection.apply(&env(
        0,
        Event::TextStart {
            session,
            message,
            part,
        },
    ));
    projection.apply(&env(
        0,
        Event::TextDelta {
            session,
            message,
            part,
            delta: "hello".to_string(),
        },
    ));

    let text = projection
        .session
        .messages
        .first()
        .expect("assistant message")
        .parts
        .first()
        .and_then(|part| match part {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            PartProjection::Reasoning { .. } | PartProjection::Tool { .. } => None,
        });

    assert_eq!(text, Some("hello"));
    assert_eq!(projection.last_seq, 1);

    projection.apply(&env(
        2,
        Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
        },
    ));
    assert_eq!(projection.last_seq, 2);
}
