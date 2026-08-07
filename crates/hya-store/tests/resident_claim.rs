//! Resident actor claim lifecycle against the store.

#![allow(clippy::unwrap_used)]

use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MailEndpoint, MailKind, MemberId, MemberRunStatus,
    MessageId, OperationId, PartId, PartProjection, Role, RosterStatus, SessionId, SubagentMode,
    ToolCallId, ToolName, ToolPartState,
};
use hya_store::{ActorClaim, AdmissionClaim, AdmissionState, OwnerRunId, SessionStore, StoreError};
use sqlx::{Connection, SqliteConnection};

struct StopFixture {
    store: SessionStore,
    root: SessionId,
    actor: SessionId,
    handle: String,
    claim: ActorClaim,
    operation: OperationId,
    before_root: Vec<Envelope>,
    before_actor: Vec<Envelope>,
}

struct TempDb {
    path: String,
}

impl TempDb {
    fn new(boundary: &str) -> Self {
        let path = std::env::temp_dir()
            .join(format!(
                "hya-resident-stop-{boundary}-{}.db",
                SessionId::new()
            ))
            .to_string_lossy()
            .into_owned();
        Self { path }
    }

    fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path));
        }
    }
}

async fn stop_fixture(path: &str) -> StopFixture {
    let store = SessionStore::connect(path).await.unwrap();
    let root = SessionId::new();
    let actor = SessionId::new();
    let handle = "resident-1".to_string();
    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: handle.clone(),
                parent: None,
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
                to: MailEndpoint::Handle(handle.clone()),
                kind: MailKind::Message,
                body: "accepted before stop".to_string(),
            },
        )
        .await
        .unwrap();

    let claim = store.try_claim_new(actor, OwnerRunId::new()).await.unwrap();
    let source_tool_call_id = ToolCallId::new();
    let operation = OperationId::from_tool_call(source_tool_call_id);
    let admission = AdmissionClaim {
        operation_id: operation,
        source_tool_call_id,
        root_session: root,
        request_fingerprint: [7; 32],
        admission_units: 1,
        actor_claim: Some(claim),
    };
    store.claim_admission(&admission).await.unwrap();
    store
        .start_admission(operation, Some(&claim))
        .await
        .unwrap();

    let running_message = MessageId::new();
    let running_part = PartId::new();
    let running_call = ToolCallId::new();
    let running_member = MemberId::new();
    let running_child = SessionId::new();
    for event in [
        Event::MessageStarted {
            session: actor,
            message: running_message,
            role: Role::Assistant,
        },
        Event::ToolCallRequested {
            session: actor,
            message: running_message,
            part: running_part,
            call: running_call,
            name: ToolName::new("read"),
            input: serde_json::json!({"path": "README.md"}),
        },
        Event::MemberSpawned {
            session: actor,
            member: running_member,
            child: Some(running_child),
            subagent_type: AgentName::new("child"),
            description: "running child".to_string(),
            depth: 2,
        },
        Event::MemberStatusChanged {
            session: actor,
            member: running_member,
            status: MemberRunStatus::Running,
        },
    ] {
        store.append_event(actor, &event).await.unwrap();
    }

    StopFixture {
        before_root: store.replay(root).await.unwrap(),
        before_actor: store.replay(actor).await.unwrap(),
        store,
        root,
        actor,
        handle,
        claim,
        operation,
    }
}

#[tokio::test]
async fn concurrent_claims_allow_exactly_one_owner() {
    let store = SessionStore::connect_memory().await.unwrap();
    let actor_id = SessionId::new();
    let owner_a = OwnerRunId::new();
    let owner_b = OwnerRunId::new();

    let (left, right) = tokio::join!(
        store.try_claim_new(actor_id, owner_a),
        store.try_claim_new(actor_id, owner_b),
    );

    let outcomes = [left, right];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(
                result,
                Err(StoreError::ActorAlreadyClaimed { actor_id: claimed })
                    if *claimed == actor_id
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn restart_recovery_increments_epoch_and_invalidates_old_claim() {
    let store = SessionStore::connect_memory().await.unwrap();
    let actor_id = SessionId::new();
    let old = store
        .try_claim_new(actor_id, OwnerRunId::new())
        .await
        .unwrap();

    let recovered = store
        .recover_claim(actor_id, OwnerRunId::new())
        .await
        .unwrap();

    assert_eq!(recovered.previous_epoch, old.epoch);
    assert_eq!(recovered.claim.epoch.get(), old.epoch.get() + 1);
    assert!(store.validate_actor_claim(&recovered.claim).await.is_ok());
    assert!(matches!(
        store.validate_actor_claim(&old).await,
        Err(StoreError::StaleActorClaim { actor_id: stale }) if stale == actor_id
    ));
}

#[tokio::test]
async fn release_requires_full_tuple_and_is_idempotent() {
    let store = SessionStore::connect_memory().await.unwrap();
    let actor_id = SessionId::new();
    let first = store
        .try_claim_new(actor_id, OwnerRunId::new())
        .await
        .unwrap();

    store.release_claim(&first).await.unwrap();
    store.release_claim(&first).await.unwrap();
    let second = store
        .try_claim_new(actor_id, OwnerRunId::new())
        .await
        .unwrap();

    assert_eq!(second.epoch.get(), first.epoch.get() + 1);
    assert!(matches!(
        store.release_claim(&first).await,
        Err(StoreError::StaleActorClaim { actor_id: stale }) if stale == actor_id
    ));
    assert!(store.validate_actor_claim(&second).await.is_ok());
}

#[tokio::test]
async fn resident_stop_finalization_commits_mail_admission_and_claim_release_together() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    let actor = SessionId::new();
    let handle = "resident-1";

    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: handle.to_string(),
                parent: None,
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
                body: "accepted before stop".to_string(),
            },
        )
        .await
        .unwrap();

    let actor_claim = store.try_claim_new(actor, OwnerRunId::new()).await.unwrap();
    let source_tool_call_id = ToolCallId::new();
    let admission = AdmissionClaim {
        operation_id: OperationId::from_tool_call(source_tool_call_id),
        source_tool_call_id,
        root_session: root,
        request_fingerprint: [7; 32],
        admission_units: 1,
        actor_claim: Some(actor_claim),
    };
    store.claim_admission(&admission).await.unwrap();
    store
        .start_admission(admission.operation_id, Some(&actor_claim))
        .await
        .unwrap();

    let running_message = MessageId::new();
    let running_part = PartId::new();
    let running_call = ToolCallId::new();
    let running_member = MemberId::new();
    let running_child = SessionId::new();
    for event in [
        Event::MessageStarted {
            session: actor,
            message: running_message,
            role: Role::Assistant,
        },
        Event::ToolCallRequested {
            session: actor,
            message: running_message,
            part: running_part,
            call: running_call,
            name: ToolName::new("read"),
            input: serde_json::json!({"path": "README.md"}),
        },
        Event::MemberSpawned {
            session: actor,
            member: running_member,
            child: Some(running_child),
            subagent_type: AgentName::new("child"),
            description: "running child".to_string(),
            depth: 2,
        },
        Event::MemberStatusChanged {
            session: actor,
            member: running_member,
            status: MemberRunStatus::Running,
        },
    ] {
        store.append_event(actor, &event).await.unwrap();
    }

    let (events, admissions) = store
        .finalize_resident_stop(&actor_claim, root, handle)
        .await
        .unwrap();

    assert_eq!(events.len(), 4);
    assert!(matches!(
        &events[0].event,
        Event::MemberFinished {
            session,
            member,
            status: MemberRunStatus::Cancelled,
            summary,
            child,
        } if *session == actor
            && *member == running_member
            && summary == "resident stopped"
            && *child == Some(running_child)
    ));
    assert!(matches!(
        &events[1].event,
        Event::ToolError {
            session,
            message,
            part,
            call,
            message_text,
            value,
        } if *session == actor
            && *message == running_message
            && *part == running_part
            && *call == running_call
            && message_text == "resident stopped"
            && value == &Some(serde_json::json!({"code": "STALE_ACTOR_CLAIM"}))
    ));
    assert!(matches!(
        &events[2].event,
        Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Cancelled,
            ..
        } if *session == actor && *message == running_message
    ));
    assert!(matches!(
        &events[3].event,
        Event::AgentActivityChanged {
            session,
            handle: event_handle,
            status: RosterStatus::Failed,
            current_task: Some(task),
        } if *session == root && event_handle == handle && task == "resident stopped"
    ));

    let failed_events = events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle: event_handle,
                    status: RosterStatus::Failed,
                    current_task: Some(task),
                } if *session == root
                    && event_handle == handle
                    && task == "resident stopped"
            )
        })
        .count();
    assert_eq!(failed_events, 1);
    assert_eq!(admissions.len(), 1);
    assert_eq!(admissions[0].state, AdmissionState::Aborted);
    assert!(admissions[0].logical_released);

    let projection = store.read_projection(root).await.unwrap();
    let entry = projection.team.roster.get(handle).unwrap();
    assert_eq!(entry.status, RosterStatus::Failed);
    let inbox_len = projection.team.inboxes.get(handle).map_or(0, Vec::len);
    assert_eq!(inbox_len, 1);
    assert_eq!(entry.resident_cursor, inbox_len as u64);
    assert!(!store.active_actor_ids().await.unwrap().contains(&actor));
    assert!(matches!(
        store.validate_actor_claim(&actor_claim).await,
        Err(StoreError::StaleActorClaim { actor_id }) if actor_id == actor
    ));

    let actor_projection = store.read_projection(actor).await.unwrap();
    let member = actor_projection
        .session
        .members
        .iter()
        .find(|member| member.member == running_member)
        .unwrap();
    assert_eq!(member.status, MemberRunStatus::Cancelled);
    let message = actor_projection
        .session
        .messages
        .iter()
        .find(|message| message.id == running_message)
        .unwrap();
    assert_eq!(message.finish, Some(FinishReason::Cancelled));
    let tool = message
        .parts
        .iter()
        .find(|part| part.id() == running_part)
        .unwrap();
    assert!(matches!(
        tool,
        PartProjection::Tool {
            state: ToolPartState::Error {
                message,
                value,
                ..
            },
            ..
        } if message == "resident stopped"
            && value == &Some(serde_json::json!({"code": "STALE_ACTOR_CLAIM"}))
    ));
}

#[tokio::test]
async fn resident_mail_remains_epoch_independent_until_explicit_stop() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    let actor = SessionId::new();
    let handle = "resident-1";
    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: handle.to_string(),
                parent: None,
                agent_type: AgentName::new("resident"),
                mode: SubagentMode::Resident,
            },
        )
        .await
        .unwrap();

    let old_claim = store.try_claim_new(actor, OwnerRunId::new()).await.unwrap();
    let mail = store
        .append_direct_mail(
            root,
            "main".to_string(),
            handle.to_string(),
            MailKind::Message,
            "epoch independent".to_string(),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        &mail.event,
        Event::MailSent {
            session,
            from,
            to: MailEndpoint::Handle(recipient),
            kind: MailKind::Message,
            body,
        } if *session == root
            && from == "main"
            && recipient == handle
            && body == "epoch independent"
    ));

    let recovered = store.recover_claim(actor, OwnerRunId::new()).await.unwrap();
    assert_eq!(recovered.previous_epoch, old_claim.epoch);
    assert!(matches!(
        store.validate_actor_claim(&old_claim).await,
        Err(StoreError::StaleActorClaim { actor_id }) if actor_id == actor
    ));
    assert!(store.validate_actor_claim(&recovered.claim).await.is_ok());

    let projection = store.read_projection(root).await.unwrap();
    assert_eq!(projection.team.inboxes.get(handle).unwrap().len(), 1);
    assert_eq!(
        projection.team.roster.get(handle).unwrap().resident_cursor,
        0
    );

    let (events, admissions) = store
        .finalize_resident_stop(&recovered.claim, root, handle)
        .await
        .unwrap();
    assert!(admissions.is_empty());
    assert_eq!(
        events
            .iter()
            .filter(|envelope| matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle: event_handle,
                    status: RosterStatus::Failed,
                    current_task: Some(task),
                } if *session == root
                    && event_handle == handle
                    && task == "resident stopped"
            ))
            .count(),
        1
    );
    assert!(!store.active_actor_ids().await.unwrap().contains(&actor));
    assert!(matches!(
        store.validate_actor_claim(&recovered.claim).await,
        Err(StoreError::StaleActorClaim { actor_id }) if actor_id == actor
    ));

    let projection = store.read_projection(root).await.unwrap();
    assert_eq!(projection.team.inboxes.get(handle).unwrap().len(), 1);
    assert_eq!(
        projection.team.roster.get(handle).unwrap().resident_cursor,
        1
    );

    let (events, admissions) = store
        .finalize_resident_stop(&recovered.claim, root, handle)
        .await
        .unwrap();
    assert!(events.is_empty());
    assert!(admissions.is_empty());

    let replay = store.replay(root).await.unwrap();
    assert_eq!(
        replay
            .iter()
            .filter(|envelope| matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    session,
                    handle: event_handle,
                    status: RosterStatus::Failed,
                    current_task: Some(task),
                } if *session == root
                    && event_handle == handle
                    && task == "resident stopped"
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn resident_stop_finalizer_rolls_back_at_each_write_boundary_and_is_exactly_once() {
    let cases = [
        (
            "member_finished",
            r#"BEFORE INSERT ON event_log
               WHEN instr(NEW.payload, '"type":"member_finished"') > 0"#,
        ),
        (
            "tool_error",
            r#"BEFORE INSERT ON event_log
               WHEN instr(NEW.payload, '"type":"tool_error"') > 0"#,
        ),
        (
            "message_finished",
            r#"BEFORE INSERT ON event_log
               WHEN instr(NEW.payload, '"type":"message_finished"') > 0"#,
        ),
        (
            "admission_abort",
            "BEFORE UPDATE OF state ON admission_journal\n               WHEN NEW.state = 'aborted' AND OLD.state = 'started'",
        ),
        (
            "root_activity",
            r#"BEFORE INSERT ON event_log
               WHEN instr(NEW.payload, '"type":"agent_activity_changed"') > 0
                 AND instr(NEW.payload, '"current_task":"resident stopped"') > 0"#,
        ),
        (
            "claim_release",
            "BEFORE UPDATE OF state ON resident_actor_claim\n               WHEN NEW.state = 'released' AND OLD.state = 'active'",
        ),
    ];

    for (boundary, trigger_body) in cases {
        let temp_db = TempDb::new(boundary);
        let fixture = stop_fixture(temp_db.path()).await;
        let mut connection = SqliteConnection::connect(&format!("sqlite://{}", temp_db.path()))
            .await
            .unwrap();
        let trigger_name = format!("test_stop_fail_{boundary}");
        let create_trigger = format!(
            "CREATE TRIGGER {trigger_name} {trigger_body} BEGIN SELECT RAISE(ABORT, 'test stop boundary'); END;"
        );
        sqlx::query(&create_trigger)
            .execute(&mut connection)
            .await
            .unwrap();

        let failed = fixture
            .store
            .finalize_resident_stop(&fixture.claim, fixture.root, &fixture.handle)
            .await;
        assert!(failed.is_err(), "{boundary} boundary must fail");
        assert_eq!(
            fixture.store.replay(fixture.root).await.unwrap(),
            fixture.before_root,
            "{boundary} must roll back root events"
        );
        assert_eq!(
            fixture.store.replay(fixture.actor).await.unwrap(),
            fixture.before_actor,
            "{boundary} must roll back actor events"
        );
        let admission = fixture
            .store
            .admission(fixture.operation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(admission.state, AdmissionState::Started);
        assert!(!admission.logical_released);
        assert!(
            fixture
                .store
                .validate_actor_claim(&fixture.claim)
                .await
                .is_ok()
        );
        assert!(
            fixture
                .store
                .active_actor_ids()
                .await
                .unwrap()
                .contains(&fixture.actor)
        );

        sqlx::query(&format!("DROP TRIGGER {trigger_name}"))
            .execute(&mut connection)
            .await
            .unwrap();
        let (events, admissions) = fixture
            .store
            .finalize_resident_stop(&fixture.claim, fixture.root, &fixture.handle)
            .await
            .unwrap();
        assert_eq!(events.len(), 4, "{boundary} retry terminalizes once");
        assert_eq!(admissions.len(), 1);
        assert_eq!(
            events
                .iter()
                .filter(|envelope| matches!(
                    &envelope.event,
                    Event::AgentActivityChanged {
                        session,
                        handle,
                        status: RosterStatus::Failed,
                        current_task: Some(task),
                    } if *session == fixture.root
                        && handle == &fixture.handle
                        && task == "resident stopped"
                ))
                .count(),
            1
        );
        assert_eq!(admissions[0].state, AdmissionState::Aborted);
        assert!(admissions[0].logical_released);
        assert!(matches!(
            fixture.store.validate_actor_claim(&fixture.claim).await,
            Err(StoreError::StaleActorClaim { actor_id }) if actor_id == fixture.actor
        ));

        let admission = fixture
            .store
            .admission(fixture.operation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(admission.state, AdmissionState::Aborted);
        assert!(admission.logical_released);
        let replay_after_retry = fixture.store.replay(fixture.root).await.unwrap();
        assert_eq!(
            replay_after_retry
                .iter()
                .filter(|envelope| matches!(
                    &envelope.event,
                    Event::AgentActivityChanged {
                        session,
                        handle,
                        status: RosterStatus::Failed,
                        current_task: Some(task),
                    } if *session == fixture.root
                        && handle == &fixture.handle
                        && task == "resident stopped"
                ))
                .count(),
            1
        );

        let (events, admissions) = fixture
            .store
            .finalize_resident_stop(&fixture.claim, fixture.root, &fixture.handle)
            .await
            .unwrap();
        assert!(events.is_empty());
        assert!(admissions.is_empty());
        assert_eq!(
            fixture.store.replay(fixture.root).await.unwrap(),
            replay_after_retry
        );

        let postcommit = fixture
            .store
            .append_direct_mail(
                fixture.root,
                "main".to_string(),
                fixture.handle.clone(),
                MailKind::Message,
                "after stop".to_string(),
                None,
            )
            .await;
        assert!(matches!(postcommit, Err(StoreError::MailboxRejected(_))));
        assert_eq!(
            fixture.store.replay(fixture.root).await.unwrap(),
            replay_after_retry,
            "post-stop mail rejection must not append MailSent"
        );
    }
}
