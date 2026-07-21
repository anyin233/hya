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

#[test]
fn reasoning_provider_data_survives_serde_and_projection_replay() {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let provider_data = serde_json::json!({
        "type": "reasoning",
        "id": "rs_123",
        "encrypted_content": "opaque",
    });
    let legacy: Event = serde_json::from_value(serde_json::json!({
        "type": "reasoning_end",
        "session": session,
        "message": message,
        "part": part,
    }))
    .expect("legacy reasoning event");
    assert!(matches!(
        legacy,
        Event::ReasoningEnd {
            provider_data: None,
            ..
        }
    ));

    let log = vec![
        env(
            1,
            Event::MessageStarted {
                session,
                message,
                role: Role::Assistant,
            },
        ),
        env(
            2,
            Event::ReasoningStart {
                session,
                message,
                part,
            },
        ),
        env(
            3,
            Event::ReasoningDelta {
                session,
                message,
                part,
                delta: "visible summary".to_string(),
            },
        ),
        env(
            4,
            Event::ReasoningEnd {
                session,
                message,
                part,
                provider_data: Some(provider_data.clone()),
            },
        ),
    ];
    let bytes = serde_json::to_vec(&log).expect("serialize reasoning log");
    let decoded: Vec<Envelope> = serde_json::from_slice(&bytes).expect("deserialize reasoning log");
    let projection = Projection::from_events(&decoded);
    let stored = projection.session.messages[0].parts[0].clone();

    assert_eq!(
        stored,
        PartProjection::Reasoning {
            id: part,
            text: "visible summary".to_string(),
            provider_data: Some(provider_data),
        }
    );
}
