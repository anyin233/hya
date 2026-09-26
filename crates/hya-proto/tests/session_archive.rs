//! Root-session archive state: `SessionArchived` (epoch-millis stamp) and
//! `SessionUnarchived` fold into `SessionProjection.archived`; logs written
//! before archiving existed (and legacy zero stamps) replay as not archived.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_proto::{
    AgentName, Envelope, Event, EventSeq, ModelRef, Projection, SessionArchiveError, SessionId,
    session_archive_event,
};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn created(session: SessionId, parent: Option<SessionId>) -> Event {
    Event::SessionCreated {
        session,
        parent,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: "/tmp".to_string(),
        project: None,
        kind: hya_proto::SessionKind::Project,
    }
}

#[test]
fn archive_then_unarchive_folds_the_flag_and_stamp() {
    let session = SessionId::new();
    let archived = Projection::from_events(&[
        env(1, created(session, None)),
        env(
            2,
            Event::SessionArchived {
                session,
                archived: serde_json::Number::from(1_700_000_000_123_i64),
            },
        ),
    ]);
    assert!(archived.session.is_archived());
    assert_eq!(
        archived.session.archived_at_millis(),
        Some(1_700_000_000_123)
    );

    let mut restored = archived.clone();
    restored.apply(&env(3, Event::SessionUnarchived { session }));
    assert!(!restored.session.is_archived());
    assert_eq!(restored.session.archived_at_millis(), None);
    assert_eq!(restored.session.archived, None);
}

#[test]
fn logs_without_archive_events_and_legacy_zero_stamps_are_not_archived() {
    let session = SessionId::new();
    let old = Projection::from_events(&[env(1, created(session, None))]);
    assert!(!old.session.is_archived());

    // The deleted Compat surface cleared the archive with a zero stamp.
    let legacy: Event = serde_json::from_value(serde_json::json!({
        "type": "session_archived",
        "session": session.to_string(),
        "archived": 0,
    }))
    .unwrap();
    let cleared = Projection::from_events(&[
        env(1, created(session, None)),
        env(
            2,
            Event::SessionArchived {
                session,
                archived: serde_json::Number::from(5),
            },
        ),
        env(3, legacy),
    ]);
    assert!(!cleared.session.is_archived());
    assert_eq!(cleared.session.archived, None);
}

#[test]
fn unarchive_decodes_from_its_wire_tag() {
    let session = SessionId::new();
    let event: Event = serde_json::from_value(serde_json::json!({
        "type": "session_unarchived",
        "session": session.to_string(),
    }))
    .unwrap();
    assert_eq!(event, Event::SessionUnarchived { session });
    assert_eq!(event.session(), Some(session));
    assert_eq!(
        serde_json::to_value(&event).unwrap()["type"],
        serde_json::json!("session_unarchived")
    );
}

#[test]
fn archive_event_is_root_only_and_idempotent() {
    let root = SessionId::new();
    let child = SessionId::new();
    let fresh = Projection::from_events(&[env(1, created(root, None))]);
    assert_eq!(
        session_archive_event(&Projection::default(), true, 10),
        Err(SessionArchiveError::NotFound)
    );
    let child_projection = Projection::from_events(&[env(1, created(child, Some(root)))]);
    assert_eq!(
        session_archive_event(&child_projection, true, 10),
        Err(SessionArchiveError::NotRoot)
    );
    // Unarchiving a child is a no-op, never an error: it is never archived.
    assert_eq!(
        session_archive_event(&child_projection, false, 10),
        Ok(None)
    );

    assert_eq!(session_archive_event(&fresh, false, 10), Ok(None));
    let archive = session_archive_event(&fresh, true, 10).unwrap().unwrap();
    assert_eq!(
        archive,
        Event::SessionArchived {
            session: root,
            archived: serde_json::Number::from(10),
        }
    );
    let mut archived = fresh.clone();
    archived.apply(&env(2, archive));
    assert_eq!(session_archive_event(&archived, true, 20), Ok(None));
    assert_eq!(
        session_archive_event(&archived, false, 20),
        Ok(Some(Event::SessionUnarchived { session: root }))
    );
}
