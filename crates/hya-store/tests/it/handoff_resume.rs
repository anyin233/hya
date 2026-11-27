//! Shutdown cancellation is atomic; restore prompts survive until turn start.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{
    AgentName, Event, FinishCause, FinishReason, MessageId, ModelRef, OwnerRunId, PartId, Role,
    SessionId, ToolCallId, ToolName,
};
use hya_store::SessionStore;

async fn create(store: &SessionStore, session: SessionId) {
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent: None,
                project: None,
                kind: hya_proto::SessionKind::Project,
                agent: AgentName::new("test"),
                model: ModelRef::new("test/model"),
                workdir: "/tmp".into(),
            },
        )
        .await
        .unwrap();
}

async fn open_turn(store: &SessionStore, session: SessionId) -> MessageId {
    let message = MessageId::new();
    store
        .append_event(
            session,
            &Event::MessageStarted {
                session,
                message,
                role: Role::Assistant,
                agent: None,
                model: None,
            },
        )
        .await
        .unwrap();
    message
}

#[tokio::test]
async fn restore_lifecycle_survives_new_owner_replaces_unconsumed_and_clears_on_delete_or_archive()
{
    // 1. Atomic cancel+restore, survives new owner, reuses prompt until assistant turn starts.
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    let interrupted = open_turn(&store, session).await;
    store
        .close_open_turns(session, FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    assert_eq!(
        store.list_pending_resumes().await.unwrap()[0].message,
        interrupted
    );

    // Another shutdown cancel replaces an unconsumed restore without duplicating rows.
    let replaced = open_turn(&store, session).await;
    store
        .close_open_turns(session, FinishCause::Shutdown, "second stop")
        .await
        .unwrap();
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    assert_eq!(
        store.list_pending_resumes().await.unwrap()[0].message,
        replaced
    );

    store.release_runtime_owner(owner).unwrap();
    let next = OwnerRunId::new();
    store.claim_runtime_owner(next).unwrap();
    let (prompt, envelopes) = store
        .take_restart_resume(session, next)
        .await
        .unwrap()
        .unwrap();
    assert!(envelopes.iter().any(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta.contains("Your previous turn was interrupted by a restart."))));
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    assert_eq!(
        store
            .take_restart_resume(session, next)
            .await
            .unwrap()
            .unwrap(),
        (prompt, Vec::new())
    );
    let restored = open_turn(&store, session).await;
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    store
        .close_open_turns(session, FinishCause::Shutdown, "third stop")
        .await
        .unwrap();
    assert_eq!(
        store.list_pending_resumes().await.unwrap()[0].message,
        restored
    );

    // 2. Deleting a session clears its restore row.
    store.delete_session(session).await.unwrap();
    assert!(
        store
            .take_restart_resume(session, next)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!store.session_exists(session).await.unwrap());
    assert!(store.list_pending_resumes().await.unwrap().is_empty());

    // 3. Archived root is not restored at stop or start.
    let archived = SessionId::new();
    create(&store, archived).await;
    open_turn(&store, archived).await;
    store
        .close_open_turns(archived, FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
    store
        .append_event(
            archived,
            &Event::SessionArchived {
                session: archived,
                archived: 123.into(),
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .take_restart_resume(archived, next)
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    open_turn(&store, archived).await;
    store
        .close_open_turns(archived, FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
}

#[tokio::test]
async fn restore_reports_noncompleted_tools_and_interrupted_children() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();

    // 1. Non-completed tools appear in resume prompt; completed tools do not.
    let session = SessionId::new();
    create(&store, session).await;
    let message = open_turn(&store, session).await;
    let mut events = Vec::new();
    for name in ["completed", "errored", "denied", "pending", "running"] {
        let part = PartId::new();
        let call = ToolCallId::new();
        events.push(Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: ToolName::new(name),
        });
        if name == "pending" {
            continue;
        }
        events.push(Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name: ToolName::new(name),
            input: serde_json::json!({}),
            provider_data: None,
        });
        if name == "completed" {
            events.push(Event::ToolResult {
                session,
                message,
                part,
                call,
                output: serde_json::json!({"ok":true}),
                time_ms: 1,
            });
        } else if name != "running" {
            events.push(Event::ToolError {
                session,
                message,
                part,
                call,
                message_text: name.into(),
                value: None,
            });
        }
    }
    events.push(Event::MessageFinished {
        session,
        message,
        role: Role::Assistant,
        finish: FinishReason::Cancelled,
        tokens: None,
        cause: Some(FinishCause::Shutdown),
    });
    store.append_events(session, events).await.unwrap();
    let (_, envelopes) = store
        .take_restart_resume(session, owner)
        .await
        .unwrap()
        .unwrap();
    let text = envelopes
        .iter()
        .find_map(|e| match &e.event {
            Event::TextDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .unwrap();
    assert!(text.contains("Tool calls that did not complete successfully:"));
    assert!(!text.contains("- completed (call "));
    for name in ["errored", "denied", "pending", "running"] {
        assert!(text.contains(&format!("- {name} (call ")));
    }
}

#[tokio::test]
async fn shutdown_resume_honors_archive_changes_after_projection_snapshot() {
    for snapshot_archived in [true, false] {
        let store = SessionStore::connect_memory()
            .await
            .unwrap()
            .with_projection_snapshot_interval(1);
        let session = SessionId::new();
        create(&store, session).await;
        if snapshot_archived {
            store
                .append_event(
                    session,
                    &Event::SessionArchived {
                        session,
                        archived: 123.into(),
                    },
                )
                .await
                .unwrap();
        }
        let snapshot = store.read_projection_shared(session).await.unwrap();
        assert_eq!(snapshot.session.is_archived(), snapshot_archived);
        drop(snapshot);
        let archive_change = if snapshot_archived {
            Event::SessionUnarchived { session }
        } else {
            Event::SessionArchived {
                session,
                archived: 456.into(),
            }
        };
        store.append_event(session, &archive_change).await.unwrap();
        let message = open_turn(&store, session).await;
        store
            .append_event(
                session,
                &Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish: FinishReason::Cancelled,
                    tokens: None,
                    cause: Some(FinishCause::Shutdown),
                },
            )
            .await
            .unwrap();
        let resumes = store.list_pending_resumes().await.unwrap();
        assert_eq!(resumes.len(), usize::from(snapshot_archived));
        if snapshot_archived {
            assert_eq!(resumes[0].message, message);
        }
    }
}
