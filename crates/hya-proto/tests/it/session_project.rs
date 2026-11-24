//! ADR-0024: `SessionCreated` records the session's Project and kind. Both
//! fields are `#[serde(default)]`, so logs written before them still decode
//! (as `kind = project` with no Project), and the reducer folds them into
//! `SessionProjection.project` / `.kind`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_proto::{
    AgentName, Envelope, Event, EventSeq, ModelRef, ProjectId, Projection, SessionId, SessionKind,
};
use uuid::Uuid;

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

#[test]
fn pre_project_session_created_json_still_decodes() {
    let session = SessionId::new();
    // Exact shape written by binaries before ADR-0024.
    let legacy = serde_json::json!({
        "type": "session_created",
        "session": session.to_string(),
        "parent": null,
        "agent": "build",
        "model": "fake/model",
        "workdir": "/tmp/old",
    });
    let event: Event = serde_json::from_value(legacy).expect("legacy session_created decodes");
    match &event {
        Event::SessionCreated { project, kind, .. } => {
            assert_eq!(*project, None);
            assert_eq!(*kind, SessionKind::Project);
        }
        other => panic!("unexpected event {other:?}"),
    }
    let projection = Projection::from_events(&[env(1, event)]);
    assert_eq!(projection.session.project, None);
    assert_eq!(projection.session.kind, SessionKind::Project);
    assert_eq!(projection.session.workdir.as_deref(), Some("/tmp/old"));
}

#[test]
fn session_created_round_trips_project_and_kind() {
    let session = SessionId::new();
    let project = ProjectId::from_uuid(Uuid::from_u128(7));
    let event = Event::SessionCreated {
        session,
        parent: None,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake/model"),
        workdir: "/repo".to_string(),
        project: Some(project),
        kind: SessionKind::Project,
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["kind"], "project");
    let back: Event = serde_json::from_value(value).unwrap();
    assert_eq!(back, event);

    let temporary = Event::SessionCreated {
        session,
        parent: None,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake/model"),
        workdir: "/cache/hya/scratch/x".to_string(),
        project: None,
        kind: SessionKind::Temporary,
    };
    let value = serde_json::to_value(&temporary).unwrap();
    assert_eq!(value["kind"], "temporary");
    assert_eq!(serde_json::from_value::<Event>(value).unwrap(), temporary);
}

#[test]
fn projection_folds_project_and_kind() {
    let session = SessionId::new();
    let project = ProjectId::new();
    let folded = Projection::from_events(&[env(
        1,
        Event::SessionCreated {
            session,
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake/model"),
            workdir: "/repo/sub".to_string(),
            project: Some(project),
            kind: SessionKind::Project,
        },
    )]);
    assert_eq!(folded.session.project, Some(project));
    assert_eq!(folded.session.kind, SessionKind::Project);

    let temp = Projection::from_events(&[env(
        1,
        Event::SessionCreated {
            session,
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake/model"),
            workdir: "/scratch".to_string(),
            project: None,
            kind: SessionKind::Temporary,
        },
    )]);
    assert_eq!(temp.session.project, None);
    assert_eq!(temp.session.kind, SessionKind::Temporary);
}

#[test]
fn project_id_displays_with_prefix_and_parses_both_forms() {
    let id = ProjectId::from_uuid(Uuid::from_u128(42));
    let shown = id.to_string();
    assert!(shown.starts_with("prj_"), "{shown}");
    assert_eq!(shown.parse::<ProjectId>().unwrap(), id);
    assert_eq!(id.as_uuid().to_string().parse::<ProjectId>().unwrap(), id);
    assert_eq!(SessionKind::default(), SessionKind::Project);
}
