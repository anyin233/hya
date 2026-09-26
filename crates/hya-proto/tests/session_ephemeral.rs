//! Ephemeral sessions (ADR-0023 amendment "the daemon drops unused
//! sessions"): `SessionEphemeralSet` marks a session a client created on
//! connect; the first message, a title, an archive, or an explicit
//! `SessionEphemeralSet { ephemeral: false }` (a fork taken from it) clears
//! the mark for good. Logs written before the event replay as not ephemeral.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_proto::{
    AgentName, Envelope, Event, EventSeq, MessageId, ModelRef, Projection, Role, SessionId,
    SessionKind,
};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn created(session: SessionId) -> Event {
    Event::SessionCreated {
        session,
        parent: None,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: "/tmp".to_string(),
        project: None,
        kind: SessionKind::Project,
    }
}

fn marked(session: SessionId) -> Vec<Envelope> {
    vec![
        env(1, created(session)),
        env(
            2,
            Event::SessionEphemeralSet {
                session,
                ephemeral: true,
            },
        ),
    ]
}

#[test]
fn the_mark_folds_and_old_logs_are_not_ephemeral() {
    let session = SessionId::new();
    assert!(Projection::from_events(&marked(session)).session.ephemeral);
    let old = Projection::from_events(&[env(1, created(session))]);
    assert!(!old.session.ephemeral);
}

#[test]
fn first_use_clears_the_mark_for_good() {
    let session = SessionId::new();
    let uses = [
        Event::MessageStarted {
            session,
            message: MessageId::new(),
            role: Role::User,
            agent: None,
            model: None,
        },
        Event::SessionTitled {
            session,
            title: "Named".to_owned(),
        },
        Event::SessionArchived {
            session,
            archived: serde_json::Number::from(1_700_000_000_000_i64),
        },
        Event::SessionEphemeralSet {
            session,
            ephemeral: false,
        },
    ];
    for used in uses {
        let mut events = marked(session);
        events.push(env(3, used.clone()));
        let projection = Projection::from_events(&events);
        assert!(
            !projection.session.ephemeral,
            "{used:?} must clear the mark"
        );
        // Unarchiving (or anything else later) never brings it back.
        let mut later = projection.clone();
        later.apply(&env(4, Event::SessionUnarchived { session }));
        assert!(!later.session.ephemeral);
    }
}

#[test]
fn a_legacy_zero_archive_stamp_does_not_clear_the_mark() {
    let session = SessionId::new();
    let mut events = marked(session);
    events.push(env(
        3,
        Event::SessionArchived {
            session,
            archived: serde_json::Number::from(0),
        },
    ));
    assert!(Projection::from_events(&events).session.ephemeral);
}

#[test]
fn the_mark_survives_a_snapshot_round_trip_and_is_omitted_when_unset() {
    let session = SessionId::new();
    let projection = Projection::from_events(&marked(session));
    let decoded = Projection::decode_snapshot(&projection.encode_snapshot().unwrap()).unwrap();
    assert_eq!(decoded, projection);
    let plain = Projection::from_events(&[env(1, created(session))]);
    let wire = serde_json::to_value(&plain).unwrap();
    assert!(wire["session"].get("ephemeral").is_none(), "{wire}");
}

#[test]
fn the_event_round_trips_and_names_its_session() {
    let session = SessionId::new();
    let event = Event::SessionEphemeralSet {
        session,
        ephemeral: true,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "session_ephemeral_set");
    assert_eq!(serde_json::from_value::<Event>(json).unwrap(), event);
    assert_eq!(event.session(), Some(session));
}
