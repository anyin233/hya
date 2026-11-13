//! The final provider step boundary is replayed and cached with the transcript.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_proto::{Envelope, Event, EventSeq, MessageId, PartId, Projection, Role, SessionId};

#[test]
fn step_boundary_survives_snapshot_resume_and_prior_part_deletion() {
    let session = SessionId::new();
    let message = MessageId::new();
    let prior = PartId::new();
    let current = PartId::new();
    let events = [
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
        Event::StepStarted {
            session,
            message,
            step: 0,
        },
        Event::TextStart {
            session,
            message,
            part: prior,
        },
        Event::StepStarted {
            session,
            message,
            step: 1,
        },
        Event::TextStart {
            session,
            message,
            part: current,
        },
        Event::PartDeleted {
            session,
            message,
            part: prior,
        },
    ];
    let log: Vec<_> = events
        .into_iter()
        .enumerate()
        .map(|(index, event)| Envelope {
            seq: EventSeq(index as u64 + 1),
            ts_millis: index as i64,
            event,
        })
        .collect();
    let prefix = Projection::from_events(&log[..4]);
    assert_eq!(prefix.session.messages[0].last_step_start, Some(1));
    assert_eq!(
        prefix.session.messages[0].time_updated,
        Some(2),
        "step markers do not change content timestamps"
    );
    let mut restored = Projection::decode_snapshot(&prefix.encode_snapshot().unwrap()).unwrap();
    for envelope in &log[4..] {
        restored.apply(envelope);
    }
    let full = Projection::from_events(&log);
    assert_eq!(restored, full);
    assert_eq!(full.session.messages[0].last_step_start, Some(0));
    assert_eq!(full.session.messages[0].parts.len(), 1);
    assert_eq!(full.session.messages[0].parts[0].id(), current);
}
