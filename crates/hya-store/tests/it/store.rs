//! Migrations, idempotent replay by sequence, session-scoped projection, and deletion cascades.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{
    Event, EventSeq, FinishReason, MessageId, PartId, PartProjection, Projection, Role, SessionId,
};
use hya_store::{SessionStore, StoreError};

fn created(session: SessionId, parent: Option<SessionId>) -> Event {
    Event::SessionCreated {
        session,
        parent,
        agent: "build".into(),
        model: "fake".into(),
        workdir: "/tmp".into(),
        project: None,
        kind: hya_proto::SessionKind::Project,
    }
}

#[tokio::test]
async fn migration_projection_and_idempotent_reducer() {
    let store = SessionStore::connect_memory().await.unwrap();
    let s = SessionId::new();
    let m = MessageId::new();
    let p = PartId::new();

    for e in [
        created(s, None),
        Event::MessageStarted {
            session: s,
            message: m,
            role: Role::Assistant,
            agent: None,
            model: None,
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
            tokens: None,
            cause: None,
        },
    ] {
        store.append_event(s, &e).await.unwrap();
    }

    let envs = store.replay(s).await.unwrap();
    assert_eq!(envs.len(), 6);
    assert_eq!((envs[0].seq, envs[5].seq), (EventSeq(1), EventSeq(6)));

    let proj = store.read_projection(s).await.unwrap();
    assert_eq!(proj.session.id, Some(s));
    assert_eq!(proj.session.messages.len(), 1);
    assert_eq!(proj.session.messages[0].finish, Some(FinishReason::Stop));
    assert!(matches!(
        &proj.session.messages[0].parts[0],
        PartProjection::Text { text, .. } if text == "Hello world"
    ));
    assert_eq!(Projection::from_events(&envs), proj);

    // Reducer is idempotent when re-applying the same seq.
    let mut idemp = Projection::default();
    idemp.apply(&envs[0]);
    let once = idemp.clone();
    idemp.apply(&envs[0]);
    assert_eq!(idemp, once);
}

#[tokio::test]
async fn replay_list_and_session_info_are_scoped_and_handle_hysec_ids() {
    let store = SessionStore::connect_memory().await.unwrap();
    let a = SessionId::new();
    let b = SessionId::new();
    for (s, titles) in [(a, ["a1", "a2"].as_slice()), (b, ["b1"].as_slice())] {
        for title in titles {
            store
                .append_event(
                    s,
                    &Event::SessionTitled {
                        session: s,
                        title: (*title).into(),
                    },
                )
                .await
                .unwrap();
        }
    }
    assert_eq!(store.replay(a).await.unwrap().len(), 2);
    assert_eq!(store.replay(b).await.unwrap().len(), 1);

    let sessions = store.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 2);
    let info_a = sessions.iter().find(|s| s.session == a).unwrap().clone();
    assert_eq!(info_a.events, 2);
    assert_eq!(store.session_info(a).await.unwrap(), Some(info_a));
    assert_eq!(store.session_info(SessionId::new()).await.unwrap(), None);

    // `hysec_` session IDs round-trip through append, replay, list, and delete.
    let hysec: SessionId = "hysec_ABCDEFGHIJKLMNOPQRST".parse().unwrap();
    let ev = created(hysec, None);
    let (seq, ts) = store.append_event(hysec, &ev).await.unwrap();
    let envelopes = store.replay(hysec).await.unwrap();
    assert_eq!(envelopes.len(), 1);
    assert_eq!((envelopes[0].seq, envelopes[0].ts_millis), (seq, ts));
    assert_eq!(envelopes[0].event, ev);
    assert_eq!(store.session_info(hysec).await.unwrap().unwrap().events, 1);
    assert!(store.delete_session(hysec).await.unwrap());
    assert!(store.replay(hysec).await.unwrap().is_empty());
    assert!(
        store
            .list_sessions()
            .await
            .unwrap()
            .iter()
            .all(|info| info.session != hysec)
    );
}

#[tokio::test]
async fn delete_session_cascades_descendants_without_touching_unrelated_sessions() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    let child = SessionId::new();
    let grandchild = SessionId::new();
    let unrelated = SessionId::new();

    for (session, parent) in [
        (root, None),
        (child, Some(root)),
        (grandchild, Some(child)),
        (unrelated, None),
    ] {
        store
            .append_event(session, &created(session, parent))
            .await
            .unwrap();
    }
    assert_eq!(
        store.read_projection(child).await.unwrap().session.id,
        Some(child)
    );
    assert_eq!(
        store.read_projection(grandchild).await.unwrap().session.id,
        Some(grandchild)
    );

    assert!(store.delete_session(root).await.unwrap());
    for deleted in [root, child, grandchild] {
        assert!(!store.session_exists(deleted).await.unwrap());
        assert!(store.replay(deleted).await.unwrap().is_empty());
        assert_eq!(
            store.read_projection(deleted).await.unwrap(),
            Projection::default()
        );
    }
    assert!(store.session_exists(unrelated).await.unwrap());
    assert_eq!(store.replay(unrelated).await.unwrap().len(), 1);
}

#[tokio::test]
async fn delete_session_at_guards_root_growth_and_cascades_children() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    let child = SessionId::new();
    let other = SessionId::new();
    let (checked, _) = store
        .append_event(root, &created(root, None))
        .await
        .unwrap();
    store
        .append_event(child, &created(child, Some(root)))
        .await
        .unwrap();
    store
        .append_event(
            other,
            &Event::SessionTitled {
                session: other,
                title: "other".into(),
            },
        )
        .await
        .unwrap();
    let (grown, _) = store
        .append_event(
            root,
            &Event::SessionTitled {
                session: root,
                title: "grown".into(),
            },
        )
        .await
        .unwrap();

    assert!(!store.delete_session_at(root, checked).await.unwrap());
    assert!(store.session_exists(root).await.unwrap());
    assert!(store.session_exists(child).await.unwrap());

    assert!(store.delete_session_at(root, grown).await.unwrap());
    assert!(!store.session_exists(root).await.unwrap());
    assert!(!store.session_exists(child).await.unwrap());
    assert!(!store.delete_session_at(root, grown).await.unwrap());
    assert_eq!(store.replay(other).await.unwrap().len(), 1);
}

#[test]
fn commit_task_panic_maps_to_non_storage_error() {
    let err = StoreError::from(sqlx::Error::Protocol(
        "commit task failed: task panicked".into(),
    ));
    assert!(!err.is_storage_fault(), "{err}");
}
