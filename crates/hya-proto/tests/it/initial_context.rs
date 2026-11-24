//! A session's recorded initial context (`InitialContextRecorded`): the
//! first record wins, until `InitialContextInvalidated` (`/reload-context`,
//! `RefreshSessionContext`) clears it so the next turn records it anew.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_proto::{
    AgentName, Envelope, Event, EventSeq, ModelRef, Projection, SessionId, SessionKind,
};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn recorded(session: SessionId, text: &str) -> Event {
    Event::InitialContextRecorded {
        session,
        text: text.to_string(),
    }
}

#[test]
fn the_first_record_wins_until_an_invalidation_clears_it() {
    let session = SessionId::new();
    let created = Event::SessionCreated {
        session,
        parent: None,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: "/tmp".to_string(),
        project: None,
        kind: SessionKind::Project,
    };
    let mut log = vec![
        env(1, created),
        env(2, recorded(session, "first")),
        env(3, recorded(session, "ignored")),
    ];
    let projection = Projection::from_events(&log);
    assert_eq!(projection.session.initial_context.as_deref(), Some("first"));

    log.push(env(4, Event::InitialContextInvalidated { session }));
    assert_eq!(Projection::from_events(&log).session.initial_context, None);

    log.push(env(5, recorded(session, "second")));
    log.push(env(6, recorded(session, "ignored again")));
    assert_eq!(
        Projection::from_events(&log)
            .session
            .initial_context
            .as_deref(),
        Some("second")
    );
}

#[test]
fn the_invalidation_round_trips_on_the_wire() {
    let session = SessionId::new();
    let event = Event::InitialContextInvalidated { session };
    let json = serde_json::to_value(&event).expect("serialize");
    assert_eq!(json["type"], "initial_context_invalidated");
    let back: Event = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back.session(), Some(session));
}
