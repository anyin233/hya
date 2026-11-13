//! Resident actor claim lifecycle against the store.

#![allow(clippy::unwrap_used)]

use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MailEndpoint, MailKind, MemberId, MemberRunStatus,
    MessageId, PartId, PartProjection, Role, RosterStatus, SessionId, SubagentMode, ToolCallId,
    ToolName, ToolPartState,
};
use hya_store::{ActorClaim, OwnerRunId, SessionStore, StoreError};
use sqlx::{Connection, SqliteConnection};

async fn finalize_resident(
    store: &SessionStore,
    claim: &ActorClaim,
    root: SessionId,
    handle: &str,
    status: MemberRunStatus,
    note: &str,
) -> Result<Vec<Envelope>, StoreError> {
    let projection = store.read_projection(claim.actor_id).await?;
    let (outcome, envelopes) = store.finalize_resident_archive(hya_store::ResidentArchiveOwner::Claim(claim), root, handle, hya_store::ResidentArchive {
        actor: claim.actor_id,
        generation: projection.session.handoff.as_ref().map_or(1, |handoff| handoff.generation + 1),
        doc: "1. Goal - test task\n2. Current state - stopped\n3. Files and code - none\n4. Decisions - none\n5. Pending tasks - none\n6. Next step - none".into(),
        degraded: true, reason: hya_proto::ArchiveReason::Killed, note: note.into(), member_status: status,
        report: None, check_gate: false, expected_work: None,
    },).await?;
    assert_eq!(outcome, hya_store::ResidentArchiveOutcome::Committed);
    Ok(envelopes)
}

struct StopFixture {
    store: SessionStore,
    root: SessionId,
    actor: SessionId,
    handle: String,
    claim: ActorClaim,
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
        for suffix in ["", "-wal", "-shm", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path));
        }
    }
}

async fn stop_fixture(path: &str) -> StopFixture {
    let store = SessionStore::connect(path).await.unwrap();
    store.claim_runtime_owner(OwnerRunId::new()).unwrap();
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
            &Event::ChannelCreated {
                session: root,
                channel: "DM-12345678".into(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), format!("main/{handle}")],
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
                to: MailEndpoint::Channel("DM-12345678".into()),
                kind: MailKind::Message,
                body: "accepted before stop".to_string(),
            },
        )
        .await
        .unwrap();

    let claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
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
    }
}

fn orphan_archive(actor: SessionId) -> hya_store::ResidentArchive {
    hya_store::ResidentArchive {
        actor,
        generation: 1,
        doc: "orphan handoff".into(),
        degraded: true,
        reason: hya_proto::ArchiveReason::ArchivedByParent,
        note: "archive orphan".into(),
        member_status: MemberRunStatus::Cancelled,
        report: None,
        check_gate: false,
        expected_work: None,
    }
}

#[tokio::test]
async fn terminal_takeover_is_idempotent_after_claim_or_takeover_archive() {
    for first_takeover in [false, true] {
        let temp_db = TempDb::new("takeover-repeat");
        let fixture = stop_fixture(temp_db.path()).await;
        let owner = if first_takeover {
            hya_store::ResidentArchiveOwner::Takeover
        } else {
            hya_store::ResidentArchiveOwner::Claim(&fixture.claim)
        };
        fixture
            .store
            .finalize_resident_archive(
                owner,
                fixture.root,
                &fixture.handle,
                orphan_archive(fixture.actor),
            )
            .await
            .unwrap();
        let before_root = fixture.store.replay(fixture.root).await.unwrap();
        let before_actor = fixture.store.replay(fixture.actor).await.unwrap();
        let (outcome, events) = fixture
            .store
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Takeover,
                fixture.root,
                &fixture.handle,
                orphan_archive(fixture.actor),
            )
            .await
            .unwrap();
        assert_eq!(outcome, hya_store::ResidentArchiveOutcome::Committed);
        assert!(events.is_empty());
        assert_eq!(
            fixture.store.replay(fixture.root).await.unwrap(),
            before_root
        );
        assert_eq!(
            fixture.store.replay(fixture.actor).await.unwrap(),
            before_actor
        );
    }
}

#[tokio::test]
async fn terminal_takeover_requires_a_released_claim_and_matching_archive_for_replay() {
    for mismatch in [
        "missing claim",
        "not archived",
        "wrong root",
        "wrong handle",
        "wrong actor",
    ] {
        let temp_db = TempDb::new("takeover-mismatch");
        let fixture = stop_fixture(temp_db.path()).await;
        let mut connection = SqliteConnection::connect(&format!("sqlite://{}", temp_db.path()))
            .await
            .unwrap();
        match mismatch {
            "missing claim" => {
                sqlx::query("DELETE FROM resident_actor_claim WHERE actor_id = ?")
                    .bind(fixture.actor.storage_key())
                    .execute(&mut connection)
                    .await
                    .unwrap();
            }
            "not archived" => {
                fixture.store.release_claim(&fixture.claim).await.unwrap();
            }
            _ => {
                fixture
                    .store
                    .finalize_resident_archive(
                        hya_store::ResidentArchiveOwner::Claim(&fixture.claim),
                        fixture.root,
                        &fixture.handle,
                        orphan_archive(fixture.actor),
                    )
                    .await
                    .unwrap();
            }
        }
        let actor = if mismatch == "wrong actor" {
            let other = SessionId::new();
            let claim = fixture
                .store
                .try_claim_new(other, other, OwnerRunId::new())
                .await
                .unwrap();
            fixture.store.release_claim(&claim).await.unwrap();
            other
        } else {
            fixture.actor
        };
        let before_claim: Option<(i64, Vec<u8>, String)> = sqlx::query_as(
            "SELECT epoch, owner_run_id, state FROM resident_actor_claim WHERE actor_id = ?",
        )
        .bind(actor.storage_key())
        .fetch_optional(&mut connection)
        .await
        .unwrap();
        let before_root = fixture.store.replay(fixture.root).await.unwrap();
        let before_actor = fixture.store.replay(actor).await.unwrap();
        let result = fixture
            .store
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Takeover,
                if mismatch == "wrong root" {
                    SessionId::new()
                } else {
                    fixture.root
                },
                if mismatch == "wrong handle" {
                    "main/other"
                } else {
                    &fixture.handle
                },
                orphan_archive(actor),
            )
            .await;
        assert!(
            matches!(result, Err(StoreError::ActorClaimUnavailable { actor_id }) if actor_id == actor),
            "{mismatch}"
        );
        let after_claim: Option<(i64, Vec<u8>, String)> = sqlx::query_as(
            "SELECT epoch, owner_run_id, state FROM resident_actor_claim WHERE actor_id = ?",
        )
        .bind(actor.storage_key())
        .fetch_optional(&mut connection)
        .await
        .unwrap();
        assert_eq!(after_claim, before_claim, "{mismatch}");
        assert_eq!(
            fixture.store.replay(fixture.root).await.unwrap(),
            before_root
        );
        assert_eq!(fixture.store.replay(actor).await.unwrap(), before_actor);
    }
}

#[tokio::test]
async fn terminal_takeover_requires_the_owning_store_handle() {
    let temp_db = TempDb::new("takeover-owner");
    let fixture = stop_fixture(temp_db.path()).await;
    let observer = SessionStore::connect(temp_db.path()).await.unwrap();
    assert!(matches!(
        observer.claim_runtime_owner(OwnerRunId::new()),
        Err(StoreError::RuntimeOwnerBusy)
    ));
    let result = observer
        .finalize_resident_archive(
            hya_store::ResidentArchiveOwner::Takeover,
            fixture.root,
            &fixture.handle,
            orphan_archive(fixture.actor),
        )
        .await;
    assert!(matches!(result, Err(StoreError::RuntimeOwnerClaimRequired)));
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
}

#[tokio::test]
async fn terminal_takeover_rejects_a_voluntary_report_gate_without_changing_the_claim() {
    let temp_db = TempDb::new("takeover-gate");
    let fixture = stop_fixture(temp_db.path()).await;
    let mut archive = orphan_archive(fixture.actor);
    archive.check_gate = true;
    archive.report = Some((hya_proto::ReportOutcome::Done, "report".into()));
    let result = fixture
        .store
        .finalize_resident_archive(
            hya_store::ResidentArchiveOwner::Takeover,
            fixture.root,
            &fixture.handle,
            archive,
        )
        .await;
    assert!(
        matches!(result, Err(StoreError::ActorClaimData(reason)) if reason == "voluntary report requires an episode claim")
    );
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
    sqlx::query("DROP TRIGGER fail_takeover_archive")
        .execute(&mut connection)
        .await
        .unwrap();
    let (outcome, events) = fixture
        .store
        .finalize_resident_archive(
            hya_store::ResidentArchiveOwner::Takeover,
            fixture.root,
            &fixture.handle,
            archive(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, hya_store::ResidentArchiveOutcome::Committed);
    assert!(events.iter().any(|envelope| matches!(
        &envelope.event,
        Event::AgentArchived { child, reason: hya_proto::ArchiveReason::ArchivedByParent, .. }
            if *child == fixture.actor
    )));
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
        let (outcome, events) = fixture
            .store
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Claim(&fixture.claim),
                fixture.root,
                &fixture.handle,
                archive(),
            )
            .await
            .unwrap();
        assert_eq!(outcome, hya_store::ResidentArchiveOutcome::Refused);
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
        store.try_claim_new(actor_id, actor_id, owner_a),
        store.try_claim_new(actor_id, actor_id, owner_b),
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
        .try_claim_new(actor_id, actor_id, OwnerRunId::new())
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
        .try_claim_new(actor_id, actor_id, OwnerRunId::new())
        .await
        .unwrap();

    store.release_claim(&first).await.unwrap();
    store.release_claim(&first).await.unwrap();
    let second = store
        .try_claim_new(actor_id, actor_id, OwnerRunId::new())
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
async fn resident_stop_finalization_commits_mail_and_claim_release_together() {
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
            &Event::ChannelCreated {
                session: root,
                channel: "DM-12345678".into(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), format!("main/{handle}")],
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
                to: MailEndpoint::Channel("DM-12345678".into()),
                kind: MailKind::Message,
                body: "accepted before stop".to_string(),
            },
        )
        .await
        .unwrap();

    let actor_claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
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

    let events = finalize_resident(
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
    store
        .append_event(
            root,
            &Event::ChannelCreated {
                session: root,
                channel: "DM-12345678".into(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), format!("main/{handle}")],
            },
        )
        .await
        .unwrap();

    let old_claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
        .await
        .unwrap();
    let mail = store
        .append_channel_mail(
            root,
            "main".to_string(),
            "DM-12345678".into(),
            MailKind::Message,
            "epoch independent".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        &mail.0.event,
        Event::MailSent {
            session,
            from,
            to: MailEndpoint::Channel(recipient),
            kind: MailKind::Message,
            body,
        } if *session == root
            && from == "main"
            && recipient == "DM-12345678"
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

    let events = finalize_resident(
        &store,
        &recovered.claim,
        root,
        path,
        MemberRunStatus::Cancelled,
        "resident stopped",
    )
    .await
    .unwrap();
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

    let events = finalize_resident(
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
        let events = finalize_resident(
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
        assert!(matches!(
            fixture.store.validate_actor_claim(&fixture.claim).await,
            Err(StoreError::StaleActorClaim { actor_id }) if actor_id == fixture.actor
        ));

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

        let events = finalize_resident(
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
        assert_eq!(
            fixture.store.replay(fixture.root).await.unwrap(),
            replay_after_retry
        );

        let postcommit = fixture
            .store
            .append_channel_mail(
                fixture.root,
                "main".to_string(),
                "DM-12345678".into(),
                MailKind::Message,
                "after stop".to_string(),
                None,
                None,
            )
            .await;
        assert!(
            matches!(postcommit, Err(StoreError::MailRevivalRequired {
                ref sender, ref channel, ref target,
            }) if sender == "main" && channel == "DM-12345678"
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
        let claim = store
            .try_claim_new(root, actor, OwnerRunId::new())
            .await
            .unwrap();

        let events = if stop {
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
async fn registration_rolls_back_claim_and_all_sessions_on_directive_mail_failure() {
    let db = TempDb::new("registration");
    let store = SessionStore::connect(db.path()).await.unwrap();
    let root = SessionId::new();
    store
        .append_event(
            root,
            &Event::SessionCreated {
                session: root,
                parent: None,
                agent: AgentName::new("main"),
                model: hya_proto::ModelRef::new("fake"),
                workdir: "/tmp".into(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            },
        )
        .await
        .unwrap();
    let before = store.replay(root).await.unwrap();
    let mut conn = SqliteConnection::connect(&format!("sqlite://{}", db.path()))
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_directive_mail BEFORE INSERT ON event_log WHEN instr(NEW.payload, 'reject_directive_mail') > 0 BEGIN SELECT RAISE(ABORT, 'prompt failure'); END")
        .execute(&mut conn).await.unwrap();
    let registrations = ["first", "reject_directive_mail"]
        .into_iter()
        .map(|directive| {
            let child = SessionId::new();
            let dm = hya_proto::mint_channel_id(hya_proto::ChannelKind::Dm);
            hya_store::EpisodeRegistration {
                actor: child,
                events: vec![
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
                        handle: child.to_string(),
                        parent: Some("main".into()),
                        agent_type: AgentName::new("worker"),
                        mode: SubagentMode::Resident,
                    },
                    Event::ChannelCreated {
                        session: root,
                        channel: dm.clone(),
                        kind: hya_proto::ChannelKind::Dm,
                        unit: None,
                        members: vec!["main".into(), format!("main/{child}")],
                    },
                    Event::MailSent {
                        session: root,
                        from: "main".into(),
                        to: MailEndpoint::Channel(dm),
                        kind: MailKind::Message,
                        body: directive.into(),
                    },
                ],
            }
        })
        .collect();
    let result = store
        .register_episodes(root, OwnerRunId::new(), None, 2, registrations)
        .await;
    assert!(result.unwrap_err().to_string().contains("prompt failure"));
    assert_eq!(store.replay(root).await.unwrap(), before);
    assert_eq!(store.list_sessions().await.unwrap().len(), 1);
    assert_eq!(store.live_member_count(root).await.unwrap(), 0);
    assert!(store.active_actor_ids().await.unwrap().is_empty());
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

async fn work_fixture(store: &SessionStore) -> (SessionId, ActorClaim) {
    let root = SessionId::new();
    let actor = SessionId::new();
    for (session, parent) in [(root, None), (actor, Some(root))] {
        store
            .append_event(
                session,
                &Event::SessionCreated {
                    session,
                    parent,
                    agent: AgentName::new("resident"),
                    model: hya_proto::ModelRef::new("fake"),
                    workdir: "/tmp".into(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
            )
            .await
            .unwrap();
    }
    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: "worker".into(),
                parent: Some("main".into()),
                agent_type: AgentName::new("resident"),
                mode: SubagentMode::Resident,
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            root,
            &Event::ChannelCreated {
                session: root,
                channel: "DM-12345678".into(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), "main/worker".into()],
            },
        )
        .await
        .unwrap();
    for body in ["first", "second"] {
        store
            .append_event(
                root,
                &Event::MailSent {
                    session: root,
                    from: "main".into(),
                    to: MailEndpoint::Channel("DM-12345678".into()),
                    kind: MailKind::Message,
                    body: body.into(),
                },
            )
            .await
            .unwrap();
    }
    let claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
        .await
        .unwrap();
    (root, claim)
}

#[tokio::test]
async fn begin_work_commits_one_prompt_and_its_work_boundary() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, claim) = work_fixture(&store).await;
    let (message, envelopes) = store
        .begin_work(
            Some(&claim),
            root,
            "main/worker",
            2,
            hya_store::ResidentWorkPrompt::Text(
                "[mail from main] first\n\n[mail from main] second".into(),
            ),
            "mail from main".into(),
        )
        .await
        .unwrap();
    assert_eq!(envelopes.len(), 7);
    let child = store.read_projection(claim.actor_id).await.unwrap();
    assert_eq!(child.session.messages.len(), 1);
    assert_eq!(child.session.messages[0].id, message);
    assert_eq!(child.session.messages[0].role, Role::User);
    let root_projection = store.read_projection(root).await.unwrap();
    let entry = &root_projection.team.roster["main/worker"];
    assert_eq!(entry.resident_work.as_ref().unwrap().inbox_through, 2);
    assert_eq!(entry.status, RosterStatus::Busy);
    store
        .append_event(
            root,
            &Event::ResidentWorkFinished {
                session: root,
                handle: "main/worker".into(),
                epoch: claim.epoch,
                inbox_through: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store.read_projection(root).await.unwrap().team.roster["main/worker"].resident_cursor,
        2
    );
}

#[tokio::test]
async fn begin_work_rejects_a_stale_claim_without_writes() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, claim) = work_fixture(&store).await;
    store
        .recover_claim(claim.actor_id, OwnerRunId::new())
        .await
        .unwrap();
    let before_root = store.replay(root).await.unwrap();
    let before_child = store.replay(claim.actor_id).await.unwrap();
    let result = store
        .begin_work(
            Some(&claim),
            root,
            "main/worker",
            2,
            hya_store::ResidentWorkPrompt::Text("stale work".into()),
            "mail".into(),
        )
        .await;
    assert!(
        matches!(result, Err(StoreError::StaleActorClaim { actor_id }) if actor_id == claim.actor_id)
    );
    assert_eq!(store.replay(root).await.unwrap(), before_root);
    assert_eq!(store.replay(claim.actor_id).await.unwrap(), before_child);
}

#[tokio::test]
async fn begin_work_rolls_back_prompt_consumption_and_activity_at_every_write() {
    for boundary in [
        "message_started",
        "text_start",
        "text_delta",
        "text_end",
        "message_finished",
        "resident_work_started",
        "agent_activity_changed",
    ] {
        let db = TempDb::new(boundary);
        let store = SessionStore::connect(db.path()).await.unwrap();
        let (root, claim) = work_fixture(&store).await;
        let before_root = store.replay(root).await.unwrap();
        let before_child = store.replay(claim.actor_id).await.unwrap();
        let mut conn = SqliteConnection::connect(&format!("sqlite://{}", db.path()))
            .await
            .unwrap();
        sqlx::query(&format!("CREATE TRIGGER reject_work BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = '{boundary}' BEGIN SELECT RAISE(ABORT, 'work failure'); END")).execute(&mut conn).await.unwrap();
        let result = store
            .begin_work(
                Some(&claim),
                root,
                "main/worker",
                2,
                hya_store::ResidentWorkPrompt::Text("coalesced work".into()),
                "mail".into(),
            )
            .await;
        assert!(result.is_err(), "{boundary}");
        assert_eq!(store.replay(root).await.unwrap(), before_root, "{boundary}");
        assert_eq!(
            store.replay(claim.actor_id).await.unwrap(),
            before_child,
            "{boundary}"
        );
        store.validate_actor_claim(&claim).await.unwrap();
    }
}

#[tokio::test]
async fn lead_begin_work_consumption_rolls_back_with_the_prompt() {
    let db = TempDb::new("lead-work");
    let store = SessionStore::connect(db.path()).await.unwrap();
    let (root, _) = work_fixture(&store).await;
    let before = store.replay(root).await.unwrap();
    let mut conn = SqliteConnection::connect(&format!("sqlite://{}", db.path()))
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_work BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = 'mail_consumed' BEGIN SELECT RAISE(ABORT, 'lead work failure'); END").execute(&mut conn).await.unwrap();
    assert!(
        store
            .begin_work(
                None,
                root,
                "main",
                2,
                hya_store::ResidentWorkPrompt::Text("lead work".into()),
                "mail".into()
            )
            .await
            .is_err()
    );
    assert_eq!(store.replay(root).await.unwrap(), before);
    sqlx::query("DROP TRIGGER reject_work")
        .execute(&mut conn)
        .await
        .unwrap();
    let (message, envelopes) = store
        .begin_work(
            None,
            root,
            "main",
            2,
            hya_store::ResidentWorkPrompt::Text("lead work".into()),
            "mail".into(),
        )
        .await
        .unwrap();
    assert!(
        envelopes
            .iter()
            .any(|e| matches!(&e.event, Event::MailConsumed { through: 2, .. }))
    );
    assert_eq!(
        store.read_projection(root).await.unwrap().session.messages[0].id,
        message
    );
}

#[tokio::test]
async fn begin_work_resumes_unanswered_prompt_and_rolls_back_its_mail_append() {
    let db = TempDb::new("registered-work");
    let store = SessionStore::connect(db.path()).await.unwrap();
    let (root, claim) = work_fixture(&store).await;
    let session = claim.actor_id;
    let message = MessageId::new();
    let part = PartId::new();
    store
        .append_events(
            session,
            vec![
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::User,
                    agent: None,
                    model: None,
                },
                Event::TextStart {
                    session,
                    message,
                    part,
                },
                Event::TextDelta {
                    session,
                    message,
                    part,
                    delta: "unfinished wake".into(),
                },
                Event::TextEnd {
                    session,
                    message,
                    part,
                },
                Event::MessageFinished {
                    session,
                    message,
                    role: Role::User,
                    finish: FinishReason::Stop,
                    tokens: None,
                    cause: None,
                },
            ],
        )
        .await
        .unwrap();
    let before_root = store.replay(root).await.unwrap();
    let before_child = store.replay(session).await.unwrap();
    let mut conn = SqliteConnection::connect(&format!("sqlite://{}", db.path()))
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_work BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = 'resident_work_started' BEGIN SELECT RAISE(ABORT, 'work failure'); END").execute(&mut conn).await.unwrap();
    assert!(
        store
            .begin_work(
                Some(&claim),
                root,
                "main/worker",
                2,
                hya_store::ResidentWorkPrompt::Resume {
                    message,
                    append: "first\n\nsecond".into()
                },
                "mail".into()
            )
            .await
            .is_err()
    );
    assert_eq!(store.replay(root).await.unwrap(), before_root);
    assert_eq!(store.replay(session).await.unwrap(), before_child);
    sqlx::query("DROP TRIGGER reject_work")
        .execute(&mut conn)
        .await
        .unwrap();
    let (owned, _) = store
        .begin_work(
            Some(&claim),
            root,
            "main/worker",
            2,
            hya_store::ResidentWorkPrompt::Resume {
                message,
                append: "first\n\nsecond".into(),
            },
            "mail".into(),
        )
        .await
        .unwrap();
    assert_eq!(owned, message);
    let projection = store.read_projection(session).await.unwrap();
    assert_eq!(projection.session.messages.len(), 1);
    assert!(
        matches!(&projection.session.messages[0].parts[0], PartProjection::Text { text, .. } if text == "unfinished wake\n\nfirst\n\nsecond")
    );
}

#[tokio::test]
async fn user_cancelled_work_recovers_its_prompt_even_after_mail_was_acknowledged() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, claim) = work_fixture(&store).await;
    let (prompt, _) = store
        .begin_work(
            Some(&claim),
            root,
            "main/worker",
            2,
            hya_store::ResidentWorkPrompt::Text("already admitted".into()),
            "working".into(),
        )
        .await
        .unwrap();
    for event in [
        Event::MailConsumed {
            session: root,
            handle: "main/worker".into(),
            through: 2,
        },
        Event::AgentActivityChanged {
            session: root,
            handle: "main/worker".into(),
            status: RosterStatus::Idle,
            current_task: None,
        },
    ] {
        store.append_event(root, &event).await.unwrap();
    }
    let projection = store.read_projection(root).await.unwrap();
    assert_eq!(projection.team.roster["main/worker"].resident_cursor, 2);
    assert!(
        projection.team.roster["main/worker"]
            .resident_work
            .is_some()
    );
    let recovered = store
        .recover_claim(claim.actor_id, OwnerRunId::new())
        .await
        .unwrap();
    let result = store
        .recover_resident_actor(&recovered, root, "main/worker")
        .await
        .unwrap();
    assert_eq!(
        result.work,
        hya_store::RecoveredResidentWork::Queued { inbox_cursor: 2 }
    );
    let actor = store.read_projection(claim.actor_id).await.unwrap();
    assert_eq!(actor.session.messages.len(), 1);
    assert_eq!(actor.session.messages[0].id, prompt);
}

async fn revival_fixture(store: &SessionStore) -> (SessionId, ActorClaim) {
    let (root, claim) = work_fixture(store).await;
    for event in [
        Event::AgentRegistered {
            session: root,
            agent_session: root,
            handle: "main".into(),
            parent: None,
            agent_type: AgentName::new("resident"),
            mode: SubagentMode::Transient,
        },
        Event::ChannelCreated {
            session: root,
            channel: "DM-12345678".into(),
            kind: hya_proto::ChannelKind::Dm,
            unit: None,
            members: vec!["main".into(), "main/worker".into()],
        },
        Event::ChannelCreated {
            session: root,
            channel: "announce-12345678".into(),
            kind: hya_proto::ChannelKind::Group,
            unit: Some("main".into()),
            members: vec!["main".into(), "main/worker".into()],
        },
        Event::MailConsumed {
            session: root,
            handle: "main/worker".into(),
            through: 1,
        },
    ] {
        store.append_event(root, &event).await.unwrap();
    }
    finalize_resident(
        store,
        &claim,
        root,
        "main/worker",
        MemberRunStatus::Cancelled,
        "archived",
    )
    .await
    .unwrap();
    (root, claim)
}

async fn revive_mail(
    store: &SessionStore,
    root: SessionId,
    actor: SessionId,
    channel: String,
    body: &str,
) -> Result<(Option<(ActorClaim, u64)>, Vec<Envelope>), StoreError> {
    store
        .revive_episode(
            root,
            OwnerRunId::new(),
            u64::MAX,
            None,
            actor,
            "main".into(),
            channel,
            MailKind::Message,
            body.into(),
            "HYA_COMPACTED_CONTEXT",
            None,
        )
        .await
}

#[tokio::test]
async fn archived_sender_cannot_append_to_its_retained_dm() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, claim) = work_fixture(&store).await;
    // Resolve first, then commit the archive before the send's writer transaction.
    let channel = store
        .read_projection(root)
        .await
        .unwrap()
        .team
        .resolve_dm("main/worker", "main")
        .unwrap();
    finalize_resident(
        &store,
        &claim,
        root,
        "main/worker",
        MemberRunStatus::Cancelled,
        "archived",
    )
    .await
    .unwrap();
    let before = store.replay(root).await.unwrap();
    let result = store
        .append_channel_mail(
            root,
            "main/worker".into(),
            channel,
            MailKind::Message,
            "stale sender".into(),
            None,
            None,
        )
        .await;
    assert!(
        matches!(result, Err(StoreError::MailAuthorizationRejected { .. })),
        "{result:?}"
    );
    assert_eq!(store.replay(root).await.unwrap(), before);
}

#[tokio::test]
async fn archived_parent_cannot_revive_a_child_through_its_retained_dm() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, parent) = work_fixture(&store).await;
    let child = SessionId::new();
    for event in [
        Event::SessionCreated {
            session: child,
            parent: Some(parent.actor_id),
            agent: AgentName::new("resident"),
            model: hya_proto::ModelRef::new("fake"),
            workdir: "/tmp".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        },
        Event::AgentRegistered {
            session: root,
            agent_session: child,
            handle: "child".into(),
            parent: Some("main/worker".into()),
            agent_type: AgentName::new("resident"),
            mode: SubagentMode::Resident,
        },
        Event::ChannelCreated {
            session: root,
            channel: "DM-87654321".into(),
            kind: hya_proto::ChannelKind::Dm,
            unit: None,
            members: vec!["main/worker".into(), "main/worker/child".into()],
        },
    ] {
        store
            .append_event(event.session().unwrap(), &event)
            .await
            .unwrap();
    }
    let claim = store
        .try_claim_new(root, child, OwnerRunId::new())
        .await
        .unwrap();
    finalize_resident(
        &store,
        &claim,
        root,
        "main/worker/child",
        MemberRunStatus::Cancelled,
        "archived",
    )
    .await
    .unwrap();
    let channel = store
        .read_projection(root)
        .await
        .unwrap()
        .team
        .resolve_dm("main/worker", "child")
        .unwrap();
    finalize_resident(
        &store,
        &parent,
        root,
        "main/worker",
        MemberRunStatus::Cancelled,
        "archived",
    )
    .await
    .unwrap();
    let before_root = store.replay(root).await.unwrap();
    let before_child = store.replay(child).await.unwrap();
    let result = store
        .revive_episode(
            root,
            OwnerRunId::new(),
            u64::MAX,
            None,
            child,
            "main/worker".into(),
            channel,
            MailKind::Message,
            "stale steer".into(),
            "HYA_COMPACTED_CONTEXT",
            None,
        )
        .await;
    assert!(
        matches!(result, Err(StoreError::MailAuthorizationRejected { .. })),
        "{result:?}"
    );
    assert_eq!(store.replay(root).await.unwrap(), before_root);
    assert_eq!(store.replay(child).await.unwrap(), before_child);
    assert!(store.active_actor_ids().await.unwrap().is_empty());
}

#[tokio::test]
async fn revive_episode_preserves_channels_and_skips_all_prior_mail() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, old) = revival_fixture(&store).await;
    let channels = store.read_projection(root).await.unwrap().team.channels;
    let (claim, envelopes) =
        revive_mail(&store, root, old.actor_id, "DM-12345678".into(), "new work")
            .await
            .unwrap();
    assert_eq!(claim.unwrap().0.epoch.get(), old.epoch.get() + 1);
    assert_eq!(envelopes.len(), 8);
    let projection = store.read_projection(root).await.unwrap();
    assert_eq!(
        projection.team.channels.keys().collect::<Vec<_>>(),
        channels.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        projection.team.channels["DM-12345678"].members,
        channels["DM-12345678"].members
    );
    assert!(
        !channels["announce-12345678"]
            .members
            .contains("main/worker")
    );
    assert!(
        projection.team.channels["announce-12345678"]
            .members
            .contains("main/worker")
    );
    assert!(!projection.team.archived.contains_key("main/worker"));
    assert_eq!(projection.team.roster["main/worker"].resident_cursor, 2);
    let inbox = &projection.team.inboxes["main/worker"];
    assert_eq!(inbox.len(), 3);
    assert_eq!(inbox[2].body, "new work");
    assert_eq!(inbox[2].to, MailEndpoint::Channel("DM-12345678".into()));
    let actor = store.read_projection(old.actor_id).await.unwrap();
    assert_eq!(actor.session.messages.last().unwrap().role, Role::System);
    let (second_claim, _) = revive_mail(
        &store,
        root,
        old.actor_id,
        "DM-12345678".into(),
        "concurrent work",
    )
    .await
    .unwrap();
    assert!(second_claim.is_none());
    assert_eq!(
        store.read_projection(root).await.unwrap().team.inboxes["main/worker"].len(),
        4
    );
    assert_eq!(
        store
            .replay(root)
            .await
            .unwrap()
            .iter()
            .filter(|envelope| matches!(envelope.event, Event::AgentRestarted { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn revive_episode_rolls_back_claim_boundary_registration_and_mail_at_every_write() {
    for tag in [
        "claim",
        "agent_registered",
        "agent_restarted",
        "message_started",
        "text_start",
        "text_delta",
        "text_end",
        "message_finished",
        "mail_sent",
    ] {
        let db = TempDb::new(tag);
        let store = SessionStore::connect(db.path()).await.unwrap();
        let (root, old) = revival_fixture(&store).await;
        let before_root = store.replay(root).await.unwrap();
        let before_actor = store.replay(old.actor_id).await.unwrap();
        let mut connection = SqliteConnection::connect(&format!("sqlite://{}", db.path()))
            .await
            .unwrap();
        let sql = if tag == "claim" {
            "CREATE TRIGGER reject_revive BEFORE UPDATE ON resident_actor_claim BEGIN SELECT RAISE(ABORT, 'injected revive'); END".into()
        } else {
            format!(
                "CREATE TRIGGER reject_revive BEFORE INSERT ON event_log WHEN NEW.payload LIKE '%\"{tag}\"%' BEGIN SELECT RAISE(ABORT, 'injected revive'); END"
            )
        };
        sqlx::query(&sql).execute(&mut connection).await.unwrap();
        assert!(
            revive_mail(
                &store,
                root,
                old.actor_id,
                "DM-12345678".into(),
                "must not send"
            )
            .await
            .is_err(),
            "{tag}"
        );
        assert_eq!(store.replay(root).await.unwrap(), before_root, "{tag}");
        assert_eq!(
            store.replay(old.actor_id).await.unwrap(),
            before_actor,
            "{tag}"
        );
        assert!(store.active_actor_ids().await.unwrap().is_empty(), "{tag}");
        assert!(
            store
                .read_projection(root)
                .await
                .unwrap()
                .team
                .archived
                .contains_key("main/worker")
        );
        sqlx::query("DROP TRIGGER reject_revive")
            .execute(&mut connection)
            .await
            .unwrap();
        let (claim, _) = revive_mail(&store, root, old.actor_id, "DM-12345678".into(), "retry")
            .await
            .unwrap();
        assert_eq!(claim.unwrap().0.epoch.get(), old.epoch.get() + 1);
    }
}

#[tokio::test]
async fn revive_episode_rechecks_policy_and_parent_claim_without_any_writes() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, old) = revival_fixture(&store).await;
    let before = store.replay(root).await.unwrap();
    let denied = store
        .revive_episode(
            root,
            OwnerRunId::new(),
            u64::MAX,
            None,
            old.actor_id,
            "main".into(),
            "DM-12345678".into(),
            MailKind::Message,
            "denied".into(),
            "HYA_COMPACTED_CONTEXT",
            Some(hya_proto::mail::ChannelPolicySnapshot {
                dm_parent: 0,
                ..Default::default()
            }),
        )
        .await;
    assert!(matches!(
        denied,
        Err(StoreError::MailAuthorizationRejected { .. })
    ));
    let stale = store
        .revive_episode(
            root,
            OwnerRunId::new(),
            u64::MAX,
            Some(&old),
            old.actor_id,
            "main".into(),
            "DM-12345678".into(),
            MailKind::Message,
            "stale sender".into(),
            "HYA_COMPACTED_CONTEXT",
            None,
        )
        .await;
    assert!(matches!(stale, Err(StoreError::StaleActorClaim { .. })));
    assert_eq!(store.replay(root).await.unwrap(), before);
    assert!(store.active_actor_ids().await.unwrap().is_empty());
}

#[tokio::test]
async fn concurrent_registration_batches_share_the_roots_durable_capacity() {
    let db = TempDb::new("batch-capacity");
    let store = SessionStore::connect(db.path()).await.unwrap();
    let root = SessionId::new();
    store
        .append_event(
            root,
            &Event::SessionCreated {
                session: root,
                parent: None,
                agent: AgentName::new("main"),
                model: hya_proto::ModelRef::new("fake"),
                workdir: "/tmp".into(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            },
        )
        .await
        .unwrap();
    let batch = || {
        (0..2)
            .map(|_| {
                let actor = SessionId::new();
                hya_store::EpisodeRegistration {
                    actor,
                    events: vec![
                        Event::SessionCreated {
                            session: actor,
                            parent: Some(root),
                            agent: AgentName::new("worker"),
                            model: hya_proto::ModelRef::new("fake"),
                            workdir: "/tmp".into(),
                            project: None,
                            kind: hya_proto::SessionKind::Project,
                        },
                        Event::AgentRegistered {
                            session: root,
                            agent_session: actor,
                            handle: actor.to_string(),
                            parent: Some("main".into()),
                            agent_type: AgentName::new("worker"),
                            mode: SubagentMode::Resident,
                        },
                    ],
                }
            })
            .collect()
    };
    let (left, right) = tokio::join!(
        store.register_episodes(root, OwnerRunId::new(), None, 2, batch()),
        store.register_episodes(root, OwnerRunId::new(), None, 2, batch()),
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let error = if let Err(error) = left {
        error
    } else {
        right.unwrap_err()
    };
    assert!(matches!(
        error,
        StoreError::CapacityExceeded {
            live: 2,
            requested: 2,
            limit: 2,
            ..
        }
    ));
    assert_eq!(store.live_member_count(root).await.unwrap(), 2);
    assert_eq!(store.list_sessions().await.unwrap().len(), 3);
    assert_eq!(
        store.read_projection(root).await.unwrap().team.roster.len(),
        3
    );
    assert_eq!(
        store
            .replay(root)
            .await
            .unwrap()
            .iter()
            .filter(|envelope| matches!(envelope.event,
        Event::AgentRegistered { agent_session, .. } if agent_session == root))
            .count(),
        1
    );
}

#[tokio::test]
async fn aborted_first_wake_closes_unanswered_prompt_before_the_next_wake() {
    for assistant in [None, Some(""), Some("partial answer")] {
        let store = SessionStore::connect_memory().await.unwrap();
        let (root, claim) = work_fixture(&store).await;
        let (prompt, _) = store
            .begin_work(
                Some(&claim),
                root,
                "main/worker",
                1,
                hya_store::ResidentWorkPrompt::Text("aborted instruction".into()),
                "working".into(),
            )
            .await
            .unwrap();
        if let Some(text) = assistant {
            let message = MessageId::new();
            let part = PartId::new();
            store
                .append_event(
                    claim.actor_id,
                    &Event::MessageStarted {
                        session: claim.actor_id,
                        message,
                        role: Role::Assistant,
                        agent: None,
                        model: None,
                    },
                )
                .await
                .unwrap();
            store
                .append_event(
                    claim.actor_id,
                    &Event::TextStart {
                        session: claim.actor_id,
                        message,
                        part,
                    },
                )
                .await
                .unwrap();
            if !text.is_empty() {
                store
                    .append_event(
                        claim.actor_id,
                        &Event::TextDelta {
                            session: claim.actor_id,
                            message,
                            part,
                            delta: text.into(),
                        },
                    )
                    .await
                    .unwrap();
            }
        }
        let recovered = store
            .recover_claim(claim.actor_id, OwnerRunId::new())
            .await
            .unwrap();
        let result = store
            .recover_resident_actor(&recovered, root, "main/worker")
            .await
            .unwrap();
        assert!(matches!(
            result.work,
            hya_store::RecoveredResidentWork::AbortedRunning { .. }
        ));
        store
            .begin_work(
                Some(&recovered.claim),
                root,
                "main/worker",
                2,
                hya_store::ResidentWorkPrompt::Text("next instruction".into()),
                "working".into(),
            )
            .await
            .unwrap();
        let projection = store.read_projection(claim.actor_id).await.unwrap();
        let messages = &projection.session.messages;
        assert_eq!(messages[0].id, prompt);
        if assistant == Some("partial answer") {
            assert_eq!(
                messages.len(),
                3,
                "already answered work needs no synthetic answer"
            );
            continue;
        }
        let closing = messages
            .iter()
            .skip(1)
            .find(|message| {
                message.parts.iter().any(
                    |part| matches!(part, PartProjection::Text { text, .. } if !text.is_empty()),
                )
            })
            .unwrap();
        assert_eq!(
            closing.role,
            Role::Assistant,
            "aborted instruction needs a nonempty closing answer before the next user message"
        );
        assert_eq!(closing.finish, Some(FinishReason::Cancelled));
        assert_eq!(closing.cause, Some(hya_proto::FinishCause::Interrupted));
        assert_eq!(messages.last().unwrap().role, Role::User);
    }
}

#[tokio::test]
async fn review_b1_reasoning_only_crash_closes_the_unanswered_wake() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, claim) = work_fixture(&store).await;
    store
        .begin_work(
            Some(&claim),
            root,
            "main/worker",
            1,
            hya_store::ResidentWorkPrompt::Text("aborted instruction".into()),
            "working".into(),
        )
        .await
        .unwrap();
    let message = MessageId::new();
    let part = PartId::new();
    for event in [
        Event::MessageStarted {
            session: claim.actor_id,
            message,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
        Event::ReasoningStart {
            session: claim.actor_id,
            message,
            part,
            reason: None,
        },
        Event::ReasoningDelta {
            session: claim.actor_id,
            message,
            part,
            delta: "unpublished thought".into(),
        },
        Event::ReasoningEnd {
            session: claim.actor_id,
            message,
            part,
            provider_data: None,
        },
    ] {
        store.append_event(claim.actor_id, &event).await.unwrap();
    }
    let recovered = store
        .recover_claim(claim.actor_id, OwnerRunId::new())
        .await
        .unwrap();
    store
        .recover_resident_actor(&recovered, root, "main/worker")
        .await
        .unwrap();
    store
        .begin_work(
            Some(&recovered.claim),
            root,
            "main/worker",
            2,
            hya_store::ResidentWorkPrompt::Text("next instruction".into()),
            "working".into(),
        )
        .await
        .unwrap();
    let projection = store.read_projection(claim.actor_id).await.unwrap();
    let visible_roles = projection
        .session
        .messages
        .iter()
        .filter(|m| {
            m.role != Role::Assistant
                || m.parts.iter().any(|p| {
                    matches!(p, PartProjection::Text { text, .. } if !text.is_empty())
                        || matches!(p, PartProjection::Tool { .. })
                })
        })
        .map(|m| m.role)
        .collect::<Vec<_>>();
    assert_eq!(
        visible_roles,
        [Role::User, Role::Assistant, Role::User],
        "reasoning-only messages can be omitted by providers"
    );
}

#[tokio::test]
async fn recovery_does_not_queue_an_outgoing_dm_copy() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, claim) = work_fixture(&store).await;
    store
        .append_event(
            root,
            &Event::MailConsumed {
                session: root,
                handle: "main/worker".into(),
                through: 2,
            },
        )
        .await
        .unwrap();
    store
        .append_channel_mail(
            root,
            "main/worker".into(),
            "DM-12345678".into(),
            MailKind::Message,
            "outgoing status".into(),
            Some(&claim),
            None,
        )
        .await
        .unwrap();
    let recovered = store
        .recover_claim(claim.actor_id, OwnerRunId::new())
        .await
        .unwrap();
    let result = store
        .recover_resident_actor(&recovered, root, "main/worker")
        .await
        .unwrap();
    assert_eq!(result.work, hya_store::RecoveredResidentWork::Idle);
}

#[tokio::test]
async fn recovery_does_not_queue_an_outgoing_dm_copy_after_interrupted_work() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, claim) = work_fixture(&store).await;
    store
        .begin_work(
            Some(&claim),
            root,
            "main/worker",
            2,
            hya_store::ResidentWorkPrompt::Text("incoming work".into()),
            "working".into(),
        )
        .await
        .unwrap();
    store
        .append_channel_mail(
            root,
            "main/worker".into(),
            "DM-12345678".into(),
            MailKind::Message,
            "outgoing status".into(),
            Some(&claim),
            None,
        )
        .await
        .unwrap();
    let recovered = store
        .recover_claim(claim.actor_id, OwnerRunId::new())
        .await
        .unwrap();
    let result = store
        .recover_resident_actor(&recovered, root, "main/worker")
        .await
        .unwrap();
    assert_eq!(
        result.work,
        hya_store::RecoveredResidentWork::AbortedRunning {
            inbox_cursor: 2,
            queued_after: false,
        }
    );
}
