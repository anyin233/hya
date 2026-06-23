#![allow(clippy::unwrap_used, clippy::expect_used)]

use yaca_proto::{
    Event, EventSeq, FinishReason, MessageId, PartId, PartProjection, Projection, Role, SessionId,
};
use yaca_store::SessionStore;

#[tokio::test]
async fn migration_applies_and_projection_is_correct() {
    let store = SessionStore::connect_memory().await.unwrap();
    let s = SessionId::new();
    let m = MessageId::new();
    let p = PartId::new();

    let events = [
        Event::SessionCreated {
            session: s,
            parent: None,
            agent: "build".into(),
            model: "fake".into(),
            workdir: "/tmp".into(),
        },
        Event::MessageStarted {
            session: s,
            message: m,
            role: Role::Assistant,
        },
        Event::TextStart {
            session: s,
            message: m,
            part: p,
        },
        Event::TextDelta {
            session: s,
            message: m,
            part: p,
            delta: "Hello ".into(),
        },
        Event::TextDelta {
            session: s,
            message: m,
            part: p,
            delta: "world".into(),
        },
        Event::MessageFinished {
            session: s,
            message: m,
            role: Role::Assistant,
            finish: FinishReason::Stop,
        },
    ];
    for e in &events {
        store.append_event(s, e).await.unwrap();
    }

    let envs = store.replay(s).await.unwrap();
    assert_eq!(envs.len(), 6);
    assert_eq!(envs[0].seq, EventSeq(1));
    assert_eq!(envs[5].seq, EventSeq(6));

    let proj = store.read_projection(s).await.unwrap();
    assert_eq!(proj.session.id, Some(s));
    assert_eq!(proj.session.messages.len(), 1);
    let msg = &proj.session.messages[0];
    assert_eq!(msg.finish, Some(FinishReason::Stop));
    assert_eq!(msg.parts.len(), 1);
    match &msg.parts[0] {
        PartProjection::Text { text, .. } => assert_eq!(text, "Hello world"),
        other => panic!("expected text part, got {other:?}"),
    }

    assert_eq!(Projection::from_events(&envs), proj);
}

#[tokio::test]
async fn reducer_is_idempotent_by_seq() {
    let s = SessionId::new();
    let env = yaca_proto::Envelope {
        seq: EventSeq(1),
        ts_millis: 0,
        event: Event::SessionTitled {
            session: s,
            title: "t".into(),
        },
    };
    let mut proj = Projection::default();
    proj.apply(&env);
    let once = proj.clone();
    proj.apply(&env);
    assert_eq!(proj, once);
}

#[tokio::test]
async fn replay_is_session_scoped() {
    let store = SessionStore::connect_memory().await.unwrap();
    let a = SessionId::new();
    let b = SessionId::new();
    store
        .append_event(
            a,
            &Event::SessionTitled {
                session: a,
                title: "a".into(),
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            b,
            &Event::SessionTitled {
                session: b,
                title: "b".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(store.replay(a).await.unwrap().len(), 1);
    assert_eq!(store.replay(b).await.unwrap().len(), 1);
}

#[tokio::test]
async fn list_sessions_returns_each_session_with_count() {
    let store = SessionStore::connect_memory().await.unwrap();
    let a = SessionId::new();
    let b = SessionId::new();
    for s in [a, b] {
        store
            .append_event(
                s,
                &Event::SessionTitled {
                    session: s,
                    title: "one".into(),
                },
            )
            .await
            .unwrap();
        store
            .append_event(
                s,
                &Event::SessionTitled {
                    session: s,
                    title: "two".into(),
                },
            )
            .await
            .unwrap();
    }
    let sessions = store.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 2);
    let ids: Vec<SessionId> = sessions.iter().map(|s| s.session).collect();
    assert!(ids.contains(&a) && ids.contains(&b));
    let info_a = sessions.iter().find(|s| s.session == a).unwrap();
    assert_eq!(info_a.events, 2);
}
