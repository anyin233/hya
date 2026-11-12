//! Resident actor claim lifecycle against the store.

#![allow(clippy::unwrap_used)]

use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MailEndpoint, MailKind, MemberId, MemberRunStatus,
    MessageId, OperationId, PartId, PartProjection, Role, RosterStatus, SessionId, SubagentMode,
    ToolCallId, ToolName, ToolPartState,
};
use hya_store::{ActorClaim, AdmissionClaim, AdmissionState, OwnerRunId, SessionStore, StoreError};
use sqlx::{Connection, SqliteConnection};

async fn finalize_resident(
    store: &SessionStore,
    claim: &ActorClaim,
    root: SessionId,
    handle: &str,
    status: MemberRunStatus,
    note: &str,
) -> Result<(Vec<Envelope>, Vec<hya_store::AdmissionRecord>), StoreError> {
    let projection = store.read_projection(claim.actor_id).await?;
    let (outcome, envelopes, admissions) = store.finalize_resident_archive(hya_store::ResidentArchiveOwner::Claim(claim), root, handle, hya_store::ResidentArchive {
        actor: claim.actor_id,
        generation: projection.session.handoff.as_ref().map_or(1, |handoff| handoff.generation + 1),
        doc: "1. Goal - test task\n2. Current state - stopped\n3. Files and code - none\n4. Decisions - none\n5. Pending tasks - none\n6. Next step - none".into(),
        degraded: true, reason: hya_proto::ArchiveReason::Killed, note: note.into(), member_status: status,
        report: None, check_gate: false, expected_work: None,
    }, if status == MemberRunStatus::Failed { hya_proto::FinishCause::Other } else { hya_proto::FinishCause::UserCancel }).await?;
    assert_eq!(outcome, hya_store::ResidentArchiveOutcome::Committed);
    Ok((envelopes, admissions))
}

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
    let path = format!("main/{handle}");
    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: handle.clone(),
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
                to: MailEndpoint::Handle(path.clone()),
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
            agent: None,
            model: None,
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
            directive: String::new(),
            tool_call: None,
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
        handle: path,
        claim,
        operation,
    }
}

#[tokio::test]
async fn terminal_takeover_archives_an_active_claim() {
    let temp_db = TempDb::new("takeover");
    let fixture = stop_fixture(temp_db.path()).await;
    let archive = || hya_store::ResidentArchive {
        actor: fixture.actor,
        generation: 1,
        doc: "orphan handoff".into(),
        degraded: true,
        reason: hya_proto::ArchiveReason::ArchivedByParent,
        note: "archive orphan".into(),
        member_status: MemberRunStatus::Cancelled,
        report: None,
        check_gate: false,
        expected_work: None,
    };
    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", temp_db.path()))
        .await
        .unwrap();
    sqlx::query(
        r#"CREATE TRIGGER fail_takeover_archive BEFORE INSERT ON event_log
           WHEN instr(NEW.payload, '"type":"agent_archived"') > 0
           BEGIN SELECT RAISE(ABORT, 'takeover archive failed'); END"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let failed = fixture
        .store
        .finalize_resident_archive(
            hya_store::ResidentArchiveOwner::Takeover,
            fixture.root,
            &fixture.handle,
            archive(),
            hya_proto::FinishCause::Archived,
        )
        .await;
    assert!(matches!(failed, Err(StoreError::Sqlite(_))));
    fixture
        .store
        .validate_actor_claim(&fixture.claim)
        .await
        .unwrap();
    assert_eq!(
        fixture.store.replay(fixture.root).await.unwrap(),
        fixture.before_root
    );
    assert_eq!(
        fixture.store.replay(fixture.actor).await.unwrap(),
        fixture.before_actor
    );
    assert_eq!(
        fixture
            .store
            .admission(fixture.operation)
            .await
            .unwrap()
            .unwrap()
            .state,
        AdmissionState::Started
    );
    sqlx::query("DROP TRIGGER fail_takeover_archive")
        .execute(&mut connection)
        .await
        .unwrap();
    let (outcome, events, admissions) = fixture
        .store
        .finalize_resident_archive(
            hya_store::ResidentArchiveOwner::Takeover,
            fixture.root,
            &fixture.handle,
            archive(),
            hya_proto::FinishCause::Archived,
        )
        .await
        .unwrap();
    assert_eq!(outcome, hya_store::ResidentArchiveOutcome::Committed);
    assert!(events.iter().any(|envelope| matches!(
        &envelope.event,
        Event::AgentArchived { child, reason: hya_proto::ArchiveReason::ArchivedByParent, .. }
            if *child == fixture.actor
    )));
    assert_eq!(admissions.len(), 1);
    assert_eq!(admissions[0].operation_id, fixture.operation);
    assert_eq!(admissions[0].state, AdmissionState::Aborted);
    assert!(admissions[0].logical_released);
    assert!(fixture.store.active_actor_ids().await.unwrap().is_empty());
    let projection = fixture.store.read_projection(fixture.root).await.unwrap();
    assert!(!projection.team.roster.contains_key(&fixture.handle));
    assert_eq!(
        projection.team.archived[&fixture.handle].session,
        fixture.actor
    );
    assert!(matches!(
        fixture.store.validate_actor_claim(&fixture.claim).await,
        Err(StoreError::StaleActorClaim { actor_id }) if actor_id == fixture.actor
    ));
    let epoch: i64 =
        sqlx::query_scalar("SELECT epoch FROM resident_actor_claim WHERE actor_id = ?")
            .bind(fixture.actor.storage_key())
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(epoch as u64, fixture.claim.epoch.get() + 1);
}

#[tokio::test]
async fn refused_report_commits_only_a_harness_notice() {
    for refusal in [
        "unread mail",
        "live subagent",
        "work changed",
        "generation changed",
    ] {
        let temp_db = TempDb::new("refusal");
        let fixture = stop_fixture(temp_db.path()).await;
        if refusal != "unread mail" {
            fixture
                .store
                .append_event(
                    fixture.root,
                    &Event::MailConsumed {
                        session: fixture.root,
                        handle: fixture.handle.clone(),
                        through: 1,
                    },
                )
                .await
                .unwrap();
        }
        if refusal == "live subagent" {
            fixture
                .store
                .append_event(
                    fixture.root,
                    &Event::AgentRegistered {
                        session: fixture.root,
                        agent_session: SessionId::new(),
                        handle: "nested".into(),
                        parent: Some(fixture.handle.clone()),
                        agent_type: AgentName::new("resident"),
                        mode: SubagentMode::Resident,
                    },
                )
                .await
                .unwrap();
        }
        let before_root = fixture.store.replay(fixture.root).await.unwrap();
        let archive = || hya_store::ResidentArchive {
            actor: fixture.actor,
            generation: if refusal == "generation changed" {
                2
            } else {
                1
            },
            doc: "prepared report handoff".into(),
            degraded: false,
            reason: hya_proto::ArchiveReason::Reported,
            note: "premature report".into(),
            member_status: MemberRunStatus::Done,
            report: Some((hya_proto::ReportOutcome::Done, "premature report".into())),
            check_gate: true,
            expected_work: (refusal == "work changed").then_some((5, None)),
        };
        let mut connection = SqliteConnection::connect(&format!("sqlite://{}", temp_db.path()))
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TRIGGER fail_refusal_notice BEFORE INSERT ON event_log
               WHEN instr(NEW.payload, '"type":"mail_sent"') > 0
               BEGIN SELECT RAISE(ABORT, 'notice insert failed'); END"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let failed = fixture
            .store
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Claim(&fixture.claim),
                fixture.root,
                &fixture.handle,
                archive(),
                hya_proto::FinishCause::UserCancel,
            )
            .await;
        assert!(matches!(failed, Err(StoreError::Sqlite(_))));
        assert_eq!(
            fixture.store.replay(fixture.root).await.unwrap(),
            before_root
        );
        assert_eq!(
            fixture.store.replay(fixture.actor).await.unwrap(),
            fixture.before_actor
        );
        fixture
            .store
            .validate_actor_claim(&fixture.claim)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER fail_refusal_notice")
            .execute(&mut connection)
            .await
            .unwrap();
        let (outcome, events, admissions) = fixture
            .store
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Claim(&fixture.claim),
                fixture.root,
                &fixture.handle,
                archive(),
                hya_proto::FinishCause::UserCancel,
            )
            .await
            .unwrap();
        assert_eq!(outcome, hya_store::ResidentArchiveOutcome::Refused);
        assert!(admissions.is_empty());
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0].event,
                Event::MailSent { session, from, to: MailEndpoint::Handle(handle), body, .. }
                    if *session == fixture.root && from == hya_proto::scope::HARNESS_HANDLE
                        && handle == &fixture.handle && body.contains("Your report was not accepted")
                        && body.contains(if refusal.ends_with("changed") { "episode changed" } else { refusal })
            ),
            "{refusal}: {:?}",
            events[0]
        );
        let mut expected = before_root;
        expected.extend(events);
        assert_eq!(fixture.store.replay(fixture.root).await.unwrap(), expected);
        assert_eq!(
            fixture.store.replay(fixture.actor).await.unwrap(),
            fixture.before_actor
        );
        fixture
            .store
            .validate_actor_claim(&fixture.claim)
            .await
            .unwrap();
        assert_eq!(
            fixture
                .store
                .admission(fixture.operation)
                .await
                .unwrap()
                .unwrap()
                .state,
            AdmissionState::Started
        );
        let projection = fixture.store.read_projection(fixture.root).await.unwrap();
        assert!(projection.team.roster.contains_key(&fixture.handle));
        assert!(!projection.team.archived.contains_key(&fixture.handle));
        let entry = &projection.team.roster[&fixture.handle];
        assert!(
            entry.resident_cursor < projection.team.inboxes[&fixture.handle].len() as u64,
            "the notice must remain unread and owe a durable wake"
        );
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
    // Projection maps are keyed by canonical path; the event carries the leaf.
    let path = "main/resident-1";

    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: handle.to_string(),
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
                to: MailEndpoint::Handle(path.to_string()),
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
            agent: None,
            model: None,
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
            directive: String::new(),
            tool_call: None,
        },
        Event::MemberStatusChanged {
            session: actor,
            member: running_member,
            status: MemberRunStatus::Running,
        },
    ] {
        store.append_event(actor, &event).await.unwrap();
    }

    let (events, admissions) = finalize_resident(
        &store,
        &actor_claim,
        root,
        path,
        MemberRunStatus::Cancelled,
        "resident stopped",
    )
    .await
    .unwrap();

    assert_eq!(events.len(), 6);
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
        } if *session == root && event_handle == path && task == "resident stopped"
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
                    && event_handle == path
                    && task == "resident stopped"
            )
        })
        .count();
    assert_eq!(failed_events, 1);
    assert_eq!(admissions.len(), 1);
    assert_eq!(admissions[0].state, AdmissionState::Aborted);
    assert!(admissions[0].logical_released);

    let projection = store.read_projection(root).await.unwrap();
    assert!(!projection.team.roster.contains_key(path));
    assert_eq!(projection.team.archived[path].session, actor);
    let inbox_len = projection.team.inboxes.get(path).map_or(0, Vec::len);
    assert_eq!(inbox_len, 1);
    let mut before_archive = hya_proto::Projection::default();
    for envelope in store.replay(root).await.unwrap() {
        if matches!(&envelope.event, Event::AgentArchived { child, .. } if *child == actor) {
            break;
        }
        before_archive.apply(&envelope);
    }
    assert_eq!(
        before_archive.team.roster[path].status,
        RosterStatus::Failed
    );
    assert_eq!(
        before_archive.team.roster[path].resident_cursor, 0,
        "terminal activity must leave unread mail unread"
    );
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
    // Projection maps are keyed by canonical path; the event carries the leaf.
    let path = "main/resident-1";
    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: handle.to_string(),
                parent: Some("main".to_string()),
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
            && recipient == path
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
    assert_eq!(projection.team.inboxes.get(path).unwrap().len(), 1);
    assert_eq!(projection.team.roster.get(path).unwrap().resident_cursor, 0);

    let (events, admissions) = finalize_resident(
        &store,
        &recovered.claim,
        root,
        path,
        MemberRunStatus::Cancelled,
        "resident stopped",
    )
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
                    && event_handle == path
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
    assert_eq!(projection.team.inboxes.get(path).unwrap().len(), 1);
    assert!(!projection.team.roster.contains_key(path));
    assert_eq!(projection.team.archived[path].session, actor);

    let (events, admissions) = finalize_resident(
        &store,
        &recovered.claim,
        root,
        path,
        MemberRunStatus::Cancelled,
        "resident stopped",
    )
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
                    && event_handle == path
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
            "handoff",
            r#"BEFORE INSERT ON event_log WHEN instr(NEW.payload, '"type":"handoff_committed"') > 0"#,
        ),
        (
            "archive",
            r#"BEFORE INSERT ON event_log WHEN instr(NEW.payload, '"type":"agent_archived"') > 0"#,
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

        let failed = finalize_resident(
            &fixture.store,
            &fixture.claim,
            fixture.root,
            &fixture.handle,
            MemberRunStatus::Cancelled,
            "resident stopped",
        )
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
        let (events, admissions) = finalize_resident(
            &fixture.store,
            &fixture.claim,
            fixture.root,
            &fixture.handle,
            MemberRunStatus::Cancelled,
            "resident stopped",
        )
        .await
        .unwrap();
        assert_eq!(events.len(), 6, "{boundary} retry terminalizes once");
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

        let (events, admissions) = finalize_resident(
            &fixture.store,
            &fixture.claim,
            fixture.root,
            &fixture.handle,
            MemberRunStatus::Cancelled,
            "resident stopped",
        )
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
                None,
            )
            .await;
        assert!(
            matches!(postcommit, Err(StoreError::MailRevivalRequired {
                ref sender, ref channel, ref target,
            }) if sender == "main" && channel == &format!("direct:{}", fixture.handle)
                && target == &fixture.handle),
            "{postcommit:?}"
        );
        assert_eq!(
            fixture.store.replay(fixture.root).await.unwrap(),
            replay_after_retry,
            "post-stop mail rejection must not append MailSent"
        );
    }
}

/// A resident with a member row on its parent's log: stopping it closes that
/// row `cancelled`, failing it closes it `failed` — inside the same
/// finalization transaction, once.
#[tokio::test]
async fn resident_finalization_closes_its_member_row_on_the_parent_log() {
    for (stop, expected) in [
        (true, MemberRunStatus::Cancelled),
        (false, MemberRunStatus::Failed),
    ] {
        let store = SessionStore::connect_memory().await.unwrap();
        let root = SessionId::new();
        let actor = SessionId::new();
        let member = MemberId::new();
        for (session, parent) in [(root, None), (actor, Some(root))] {
            store
                .append_event(
                    session,
                    &Event::SessionCreated {
                        session,
                        parent,
                        agent: AgentName::new("resident"),
                        model: hya_proto::ModelRef::new("fake"),
                        workdir: "/tmp".to_string(),
                        project: None,
                        kind: hya_proto::SessionKind::Project,
                    },
                )
                .await
                .unwrap();
        }
        for event in [
            Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: "resident-1".to_string(),
                parent: Some("main".to_string()),
                agent_type: AgentName::new("resident"),
                mode: SubagentMode::Resident,
            },
            Event::MemberSpawned {
                session: root,
                member,
                child: Some(actor),
                subagent_type: AgentName::new("resident"),
                description: "worker".to_string(),
                depth: 1,
                directive: String::new(),
                tool_call: Some(ToolCallId::new()),
            },
            Event::MemberStatusChanged {
                session: root,
                member,
                status: MemberRunStatus::Running,
            },
        ] {
            store.append_event(root, &event).await.unwrap();
        }
        let claim = store.try_claim_new(actor, OwnerRunId::new()).await.unwrap();

        let (events, _) = if stop {
            finalize_resident(
                &store,
                &claim,
                root,
                "main/resident-1",
                MemberRunStatus::Cancelled,
                "resident stopped",
            )
            .await
            .unwrap()
        } else {
            finalize_resident(
                &store,
                &claim,
                root,
                "main/resident-1",
                MemberRunStatus::Failed,
                "team budget exceeded",
            )
            .await
            .unwrap()
        };
        let finishes: Vec<_> = events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::MemberFinished {
                    session,
                    member: row,
                    status,
                    child,
                    ..
                } if *row == member => Some((*session, *status, *child)),
                _ => None,
            })
            .collect();
        assert_eq!(finishes, vec![(root, expected, Some(actor))], "stop={stop}");
        let row = store
            .read_projection(root)
            .await
            .unwrap()
            .session
            .members
            .into_iter()
            .find(|row| row.member == member)
            .unwrap();
        assert_eq!(row.status, expected);
    }
}

#[tokio::test]
async fn registration_rolls_back_claim_and_all_sessions_on_prompt_failure() {
    let db = TempDb::new("registration");
    let store = SessionStore::connect(db.path()).await.unwrap();
    let mut conn = SqliteConnection::connect(&format!("sqlite://{}", db.path()))
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_initial_prompt BEFORE INSERT ON event_log WHEN instr(NEW.payload, 'reject_initial_prompt') > 0 BEGIN SELECT RAISE(ABORT, 'prompt failure'); END")
        .execute(&mut conn).await.unwrap();
    let child = SessionId::new();
    let root = SessionId::new();
    let result = store
        .commit_resident_registration(
            child,
            OwnerRunId::new(),
            None,
            vec![
                Event::SessionCreated {
                    session: root,
                    parent: None,
                    agent: AgentName::new("main"),
                    model: hya_proto::ModelRef::new("fake"),
                    workdir: "/tmp".into(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
                Event::SessionCreated {
                    session: child,
                    parent: Some(root),
                    agent: AgentName::new("worker"),
                    model: hya_proto::ModelRef::new("fake"),
                    workdir: "/tmp".into(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
                Event::AgentRegistered {
                    session: root,
                    agent_session: child,
                    handle: "worker-1".into(),
                    parent: Some("main".into()),
                    agent_type: AgentName::new("worker"),
                    mode: SubagentMode::Resident,
                },
                Event::TextDelta {
                    session: child,
                    message: MessageId::new(),
                    part: PartId::new(),
                    delta: "reject_initial_prompt".into(),
                },
            ],
        )
        .await;
    assert!(result.unwrap_err().to_string().contains("prompt failure"));
    assert!(store.active_actor_ids().await.unwrap().is_empty());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_log")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn registration_writer_reserves_sqlite_before_any_read() {
    let db = TempDb::new("immediate-registration");
    let store = SessionStore::connect(db.path()).await.unwrap();
    let mut writer = SqliteConnection::connect(&format!("sqlite://{}", db.path()))
        .await
        .unwrap();
    let held = writer.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let waiting = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        store.begin_transaction(),
    )
    .await;
    assert!(
        waiting.is_err(),
        "begin_transaction returned without acquiring SQLite's writer reservation"
    );
    held.rollback().await.unwrap();
    store
        .begin_transaction()
        .await
        .unwrap()
        .rollback()
        .await
        .unwrap();
}
