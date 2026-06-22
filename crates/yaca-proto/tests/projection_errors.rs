use yaca_proto::{Envelope, Event, EventSeq, Projection, Role, SessionId};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

#[test]
fn event_errors_are_projected_as_stable_system_rows() {
    let session = SessionId::new();
    let events = vec![
        env(
            1,
            Event::SessionCreated {
                session,
                parent: None,
                agent: "build".into(),
                model: "fake".into(),
                workdir: "/tmp".into(),
            },
        ),
        env(
            2,
            Event::Error {
                session: Some(session),
                code: "provider".to_string(),
                message: "quota exhausted".to_string(),
            },
        ),
    ];

    // Given: a replayable session event log containing a real protocol error.
    let first = Projection::from_events(&events);
    let second = Projection::from_events(&events);

    // When: the projection is rebuilt from the same events.
    // Then: the error becomes a stable system row instead of disappearing.
    assert_eq!(first, second, "error projection should be replay-stable");
    assert_eq!(first.session.messages.len(), 1);
    let message = &first.session.messages[0];
    assert_eq!(message.role, Role::System);
    assert_eq!(message.parts.len(), 1);
    assert!(
        format!("{:?}", message.parts[0]).contains("provider: quota exhausted"),
        "protocol error text should be preserved"
    );
}
