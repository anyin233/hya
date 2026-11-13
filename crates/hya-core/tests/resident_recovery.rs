//! Integration tests for `hya-core`: resident recovery.

#![allow(clippy::unwrap_used)]

mod support;

use std::path::Path;
use std::sync::Arc;

use hya_core::{CoreError, EventBus, ResidentRecovery, SessionEngine};
use hya_proto::{
    AgentName, Event, MailEndpoint, MailKind, MemberId, MemberRunStatus, MessageId, PartId,
    RosterStatus, SessionId, SubagentMode, ToolCallId, ToolName, ToolPartState,
};
use hya_provider::ProviderRouter;
use hya_store::{OwnerRunId, SessionStore, StoreError};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use sqlx::{Connection, SqliteConnection};

async fn engine(store: SessionStore) -> SessionEngine {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        store,
        Arc::new(ProviderRouter::new()),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    )
}

#[tokio::test]
async fn stale_tool_or_child_completion_cannot_append_or_advance_projection() {
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = engine(store.clone()).await;
    let root = SessionId::new();
    let old_binding = engine.runtime_registry().bind_turn(Path::new(".")).unwrap();
    let old_claim = store
        .try_claim_new(root, root, OwnerRunId::new())
        .await
        .unwrap();
    let recovered = store.recover_claim(root, OwnerRunId::new()).await.unwrap();
    let before = store.read_projection(root).await.unwrap();

    let result = engine
        .commit_resident_mutation(
            &old_claim,
            root,
            vec![Event::ToolResult {
                session: root,
                message: MessageId::new(),
                part: PartId::new(),
                call: ToolCallId::new(),
                output: serde_json::json!({"stale": true}),
                time_ms: 1,
            }],
        )
        .await;

    assert!(matches!(
        result,
        Err(CoreError::Store(StoreError::StaleActorClaim { actor_id }))
            if actor_id == root
    ));
    assert_eq!(store.read_projection(root).await.unwrap(), before);
    assert!(store.replay(root).await.unwrap().is_empty());
    assert!(old_binding.resolve_tool("read").is_some());
    let child_completion = engine
        .commit_resident_mutation(
            &old_claim,
            root,
            vec![Event::MemberFinished {
                session: root,
                member: MemberId::new(),
                status: MemberRunStatus::Done,
                summary: "late child".to_string(),
                child: Some(SessionId::new()),
            }],
        )
        .await;
    assert!(matches!(
        child_completion,
        Err(CoreError::Store(StoreError::StaleActorClaim { actor_id }))
            if actor_id == root
    ));
    assert!(store.replay(root).await.unwrap().is_empty());
    assert!(
        engine
            .commit_resident_mutation(
                &recovered.claim,
                root,
                vec![Event::ToolResult {
                    session: root,
                    message: MessageId::new(),
                    part: PartId::new(),
                    call: ToolCallId::new(),
                    output: serde_json::json!({"current": true}),
                    time_ms: 1,
                }],
            )
            .await
            .is_ok()
    );
    assert_eq!(store.replay(root).await.unwrap().len(), 1);
}

#[tokio::test]
async fn queued_resident_message_resumes_but_running_message_aborts() {
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = engine(store.clone()).await;

    let queued_root = SessionId::new();
    let queued_actor = SessionId::new();
    let queued_claim = store
        .try_claim_new(queued_root, queued_actor, OwnerRunId::new())
        .await
        .unwrap();
    store
        .append_event(
            queued_root,
            &Event::AgentRegistered {
                session: queued_root,
                agent_session: queued_actor,
                handle: "queued-1".to_string(),
                parent: Some("main".to_string()),
                agent_type: AgentName::new("queued"),
                mode: SubagentMode::Resident,
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            queued_root,
            &Event::MailSent {
                session: queued_root,
                from: "main".to_string(),
                to: MailEndpoint::Handle("main/queued-1".to_string()),
                kind: MailKind::Message,
                body: "queued".to_string(),
            },
        )
        .await
        .unwrap();
    let queued_recovered = store
        .recover_claim(queued_actor, OwnerRunId::new())
        .await
        .unwrap();

    assert_eq!(
        engine
            .recover_resident_actor(&queued_recovered, queued_root, "main/queued-1")
            .await
            .unwrap()
            .work,
        ResidentRecovery::Queued { inbox_cursor: 0 }
    );
    assert_eq!(queued_claim.epoch, queued_recovered.previous_epoch);

    let running_root = SessionId::new();
    let running_actor = SessionId::new();
    let running_claim = store
        .try_claim_new(running_root, running_actor, OwnerRunId::new())
        .await
        .unwrap();
    let running_message = MessageId::new();
    let running_part = PartId::new();
    let running_call = ToolCallId::new();
    let running_member = MemberId::new();
    let running_child = SessionId::new();
    for event in [
        Event::MessageStarted {
            session: running_actor,
            message: running_message,
            role: hya_proto::Role::Assistant,
            agent: None,
            model: None,
        },
        Event::ToolCallRequested {
            session: running_actor,
            message: running_message,
            part: running_part,
            call: running_call,
            name: ToolName::new("read"),
            input: serde_json::json!({"path": "README.md"}),
        },
        Event::MemberSpawned {
            session: running_actor,
            member: running_member,
            child: Some(running_child),
            subagent_type: AgentName::new("child"),
            description: "running child".to_string(),
            depth: 2,
            directive: String::new(),
            tool_call: None,
        },
        Event::MemberStatusChanged {
            session: running_actor,
            member: running_member,
            status: MemberRunStatus::Running,
        },
    ] {
        store.append_event(running_actor, &event).await.unwrap();
    }
    store
        .append_event(
            running_root,
            &Event::AgentRegistered {
                session: running_root,
                agent_session: running_actor,
                handle: "running-1".to_string(),
                parent: Some("main".to_string()),
                agent_type: AgentName::new("running"),
                mode: SubagentMode::Resident,
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            running_root,
            &Event::MailSent {
                session: running_root,
                from: "main".to_string(),
                to: MailEndpoint::Handle("main/running-1".to_string()),
                kind: MailKind::Message,
                body: "running".to_string(),
            },
        )
        .await
        .unwrap();
    engine
        .commit_resident_mutation(
            &running_claim,
            running_root,
            vec![
                Event::ResidentWorkStarted {
                    session: running_root,
                    actor_session: running_actor,
                    handle: "main/running-1".to_string(),
                    epoch: running_claim.epoch,
                    inbox_through: 1,
                },
                Event::AgentActivityChanged {
                    session: running_root,
                    handle: "main/running-1".to_string(),
                    status: RosterStatus::Busy,
                    current_task: Some("mail from main".to_string()),
                },
            ],
        )
        .await
        .unwrap();
    store
        .append_event(
            running_root,
            &Event::MailSent {
                session: running_root,
                from: "main".to_string(),
                to: MailEndpoint::Handle("main/running-1".to_string()),
                kind: MailKind::Message,
                body: "queued after running".to_string(),
            },
        )
        .await
        .unwrap();
    let running_recovered = store
        .recover_claim(running_actor, OwnerRunId::new())
        .await
        .unwrap();

    assert_eq!(
        engine
            .recover_resident_actor(&running_recovered, running_root, "main/running-1")
            .await
            .unwrap()
            .work,
        ResidentRecovery::AbortedRunning {
            inbox_cursor: 1,
            queued_after: true,
        }
    );
    let projection = store.read_projection(running_root).await.unwrap();
    let entry = projection.team.roster.get("main/running-1").unwrap();
    assert_eq!(entry.status, RosterStatus::Failed);
    assert_eq!(entry.resident_cursor, 1);
    assert!(entry.resident_work.is_none());
    let actor_projection = store.read_projection(running_actor).await.unwrap();
    assert_eq!(
        actor_projection.session.members[0].status,
        MemberRunStatus::Cancelled
    );
    let assistant = actor_projection.session.messages.last().unwrap();
    assert_eq!(assistant.finish, Some(hya_proto::FinishReason::Cancelled));
    assert!(assistant.parts.iter().any(|part| matches!(
        part,
        hya_proto::PartProjection::Tool {
            state: ToolPartState::Error { message, .. },
            ..
        } if message == "aborted by resident recovery"
    )));
}

#[tokio::test]
async fn resident_recovery_rolls_back_actor_and_root_failure_atomically() {
    let db_dir = support::TestDir::new("resident-recovery-atomic");
    let db_path = db_dir.path().join("sessions.db");
    let db_path = db_path.to_string_lossy().into_owned();
    let store = SessionStore::connect(&db_path).await.unwrap();
    let engine = engine(store.clone()).await;
    let root = SessionId::new();
    let actor = SessionId::new();
    let handle = "main/recovery-atomic-1";

    let old_claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
        .await
        .unwrap();
    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: hya_proto::scope::leaf(handle).to_string(),
                parent: Some("main".to_string()),
                agent_type: AgentName::new("resident"),
                mode: SubagentMode::Resident,
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            root,
            &Event::MailSent {
                session: root,
                from: "main".to_string(),
                to: MailEndpoint::Handle(handle.to_string()),
                kind: MailKind::Message,
                body: "recovery".to_string(),
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            actor,
            &Event::MessageStarted {
                session: actor,
                message: MessageId::new(),
                role: hya_proto::Role::Assistant,
                agent: None,
                model: None,
            },
        )
        .await
        .unwrap();
    engine
        .commit_resident_mutation(
            &old_claim,
            root,
            vec![
                Event::ResidentWorkStarted {
                    session: root,
                    actor_session: actor,
                    handle: handle.to_string(),
                    epoch: old_claim.epoch,
                    inbox_through: 1,
                },
                Event::AgentActivityChanged {
                    session: root,
                    handle: handle.to_string(),
                    status: RosterStatus::Busy,
                    current_task: Some("mail from main".to_string()),
                },
            ],
        )
        .await
        .unwrap();

    let recovered = store.recover_claim(actor, OwnerRunId::new()).await.unwrap();
    let root_before = store.replay(root).await.unwrap();
    let actor_before = store.replay(actor).await.unwrap();

    let mut connection = SqliteConnection::connect(&format!("sqlite://{db_path}"))
        .await
        .unwrap();
    let trigger = format!(
        "CREATE TRIGGER test_resident_recovery_atomic_failure \
         BEFORE INSERT ON event_log \
         WHEN instr(NEW.payload, '\"type\":\"agent_activity_changed\"') > 0 \
           AND instr(NEW.payload, '\"session\":\"{root}\"') > 0 \
           AND instr(NEW.payload, '\"current_task\":\"aborted by resident recovery\"') > 0 \
         BEGIN SELECT RAISE(ABORT, 'test recovery root failure'); END;"
    );
    sqlx::query(&trigger)
        .execute(&mut connection)
        .await
        .unwrap();

    let result = engine
        .recover_resident_actor(&recovered, root, handle)
        .await;
    assert!(result.is_err());
    assert_eq!(store.replay(root).await.unwrap(), root_before);
    assert_eq!(store.replay(actor).await.unwrap(), actor_before);
    assert!(store.validate_actor_claim(&recovered.claim).await.is_ok());
    assert!(store.active_actor_ids().await.unwrap().contains(&actor));
}

#[tokio::test]
async fn repeated_startup_recovery_produces_identical_projection_and_no_duplicate_terminal_events()
{
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = engine(store.clone()).await;
    let root = SessionId::new();
    let actor = SessionId::new();
    let old_claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
        .await
        .unwrap();
    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: "repeat-1".to_string(),
                parent: Some("main".to_string()),
                agent_type: AgentName::new("repeat"),
                mode: SubagentMode::Resident,
            },
        )
        .await
        .unwrap();
    let user_message = MessageId::new();
    let user_part = PartId::new();
    for event in [
        Event::MessageStarted {
            session: actor,
            message: user_message,
            role: hya_proto::Role::User,
            agent: None,
            model: None,
        },
        Event::TextStart {
            session: actor,
            message: user_message,
            part: user_part,
        },
        Event::TextDelta {
            session: actor,
            message: user_message,
            part: user_part,
            delta: "started work".to_string(),
        },
        Event::TextEnd {
            session: actor,
            message: user_message,
            part: user_part,
        },
        Event::MessageFinished {
            session: actor,
            message: user_message,
            role: hya_proto::Role::User,
            finish: hya_proto::FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ] {
        store.append_event(actor, &event).await.unwrap();
    }
    engine
        .commit_resident_mutation(
            &old_claim,
            root,
            vec![Event::ResidentWorkStarted {
                session: root,
                actor_session: actor,
                handle: "main/repeat-1".to_string(),
                epoch: old_claim.epoch,
                inbox_through: 0,
            }],
        )
        .await
        .unwrap();

    let _first_claim = store.recover_claim(actor, OwnerRunId::new()).await.unwrap();
    let second_claim = store.recover_claim(actor, OwnerRunId::new()).await.unwrap();
    let first = engine
        .recover_resident_actor(&second_claim, root, "main/repeat-1")
        .await
        .unwrap();
    assert_eq!(
        first.work,
        ResidentRecovery::AbortedRunning {
            inbox_cursor: 0,
            queued_after: false,
        }
    );
    let projection_after_first = store.read_projection(root).await.unwrap();

    let third_claim = store.recover_claim(actor, OwnerRunId::new()).await.unwrap();
    let second = engine
        .recover_resident_actor(&third_claim, root, "main/repeat-1")
        .await
        .unwrap();
    assert_eq!(second.work, ResidentRecovery::Idle);
    assert_eq!(
        store.read_projection(root).await.unwrap(),
        projection_after_first
    );
    let terminal_events = store
        .replay(root)
        .await
        .unwrap()
        .into_iter()
        .filter(|envelope| {
            matches!(
                envelope.event,
                Event::AgentActivityChanged {
                    status: RosterStatus::Failed,
                    ..
                }
            )
        })
        .count();
    assert_eq!(terminal_events, 1);
}
