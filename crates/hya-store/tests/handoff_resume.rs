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
async fn restore_is_appended_once_and_survives_a_new_owner() {
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
    store.release_runtime_owner(owner).unwrap();
    let next = OwnerRunId::new();
    store.claim_runtime_owner(next).unwrap();
    let (prompt, envelopes) = store
        .take_restart_resume(session, next)
        .await
        .unwrap()
        .unwrap();
    assert!(envelopes.iter().any(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta.contains("Your previous turn was interrupted by a restart."))));
    assert_eq!(
        store.list_pending_resumes().await.unwrap().len(),
        1,
        "a restore must remain pending until its assistant turn starts"
    );
    assert_eq!(
        store
            .take_restart_resume(session, next)
            .await
            .unwrap()
            .unwrap(),
        (prompt, Vec::new()),
        "a second start reuses the injected prompt"
    );
    let projection = store.read_projection(session).await.unwrap();
    assert_eq!(projection.session.messages.len(), 2);
    assert_eq!(projection.session.messages[1].role, Role::User);
    let restored = open_turn(&store, session).await;
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    store
        .close_open_turns(session, FinishCause::Shutdown, "second stop")
        .await
        .unwrap();
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    assert_eq!(
        store.list_pending_resumes().await.unwrap()[0].message,
        restored
    );
}

#[tokio::test]
async fn deleting_a_session_removes_its_restore() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    open_turn(&store, session).await;
    store
        .close_open_turns(session, FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
    store.delete_session(session).await.unwrap();
    assert!(
        store
            .take_restart_resume(session, owner)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!store.session_exists(session).await.unwrap());
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
}

#[tokio::test]
async fn shutdown_cancellation_and_restore_are_one_transaction() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    open_turn(&store, session).await;
    store
        .close_open_turns(session, FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
    assert_eq!(
        store.list_pending_resumes().await.unwrap().len(),
        1,
        "the transaction that cancels a root must also leave a restore row"
    );
}

#[tokio::test]
async fn archived_root_is_not_restored_at_stop_or_start() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    open_turn(&store, session).await;
    store
        .close_open_turns(session, FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
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
    assert!(
        store
            .take_restart_resume(session, owner)
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    open_turn(&store, session).await;
    store
        .close_open_turns(session, FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
}

#[tokio::test]
async fn another_cancel_replaces_an_unconsumed_restore() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    for prompt in ["first stop", "second stop"] {
        open_turn(&store, session).await;
        store
            .close_open_turns(session, FinishCause::Shutdown, prompt)
            .await
            .unwrap();
    }
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    let (_, injected) = store
        .take_restart_resume(session, owner)
        .await
        .unwrap()
        .unwrap();
    assert!(injected.iter().any(|entry| matches!(&entry.event,
        Event::TextDelta { delta, .. } if delta.contains("Your previous turn was interrupted by a restart."))));
}

#[tokio::test]
async fn restore_is_derived_from_the_indexed_messages_noncompleted_tools() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
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
    assert_eq!(
        store.list_pending_resumes().await.unwrap()[0].message,
        message
    );
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
async fn restore_lists_children_interrupted_before_the_root_message_started() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let root = SessionId::new();
    let child = SessionId::new();
    create(&store, root).await;
    create(&store, child).await;
    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: child,
                handle: "worker".into(),
                parent: Some("main".into()),
                agent_type: AgentName::new("test"),
                mode: hya_proto::SubagentMode::Resident,
            },
        )
        .await
        .unwrap();
    open_turn(&store, child).await;
    store
        .close_open_turns(child, FinishCause::Shutdown, "child stopped first")
        .await
        .unwrap();
    // Explicit timestamps keep the ordering deterministic without a sleep.
    let mut tx = store.begin_transaction().await.unwrap();
    sqlx::query("UPDATE event_log SET ts = 1 WHERE session_id = ?")
        .bind(child.storage_key())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    open_turn(&store, root).await;
    store
        .close_open_turns(root, FinishCause::Shutdown, "root closes later")
        .await
        .unwrap();
    let (_, prompt) = store
        .take_restart_resume(root, owner)
        .await
        .unwrap()
        .unwrap();
    assert!(prompt.iter().any(|e| matches!(&e.event,
        Event::TextDelta { delta, .. } if delta.contains("Interrupted subagents: main/worker."))));

    // An assistant answer after that shutdown removes the child from the list.
    let answer = open_turn(&store, child).await;
    store
        .append_event(
            child,
            &Event::MessageFinished {
                session: child,
                message: answer,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )
        .await
        .unwrap();
    open_turn(&store, root).await;
    store
        .close_open_turns(root, FinishCause::Shutdown, "next interruption")
        .await
        .unwrap();
    let (_, prompt) = store
        .take_restart_resume(root, owner)
        .await
        .unwrap()
        .unwrap();
    assert!(!prompt.iter().any(|e| matches!(&e.event,
        Event::TextDelta { delta, .. } if delta.contains("Interrupted subagents:"))));
}
