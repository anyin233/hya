//! Resident actor claim lifecycle, takeover, work boundaries, revival, and recovery.

#![allow(clippy::unwrap_used)]

use hya_proto::{
    AgentName, ChannelKind, Envelope, Event, FinishReason, MailEndpoint, MailKind, MemberId,
    MemberRunStatus, MessageId, ModelRef, PartId, PartProjection, Role, RosterStatus, SessionId,
    SessionKind, SubagentMode, ToolCallId, ToolName, ToolPartState,
};
use hya_store::{
    ActorClaim, OwnerRunId, ResidentArchive, ResidentArchiveOutcome, ResidentArchiveOwner,
    ResidentWorkPrompt, SessionStore, StoreError,
};
use sqlx::{Connection, SqliteConnection};

fn ev_created(session: SessionId, parent: Option<SessionId>, agent: &str) -> Event {
    Event::SessionCreated {
        session,
        parent,
        agent: AgentName::new(agent),
        model: ModelRef::new("fake"),
        workdir: "/tmp".into(),
        project: None,
        kind: SessionKind::Project,
    }
}

fn ev_registered(
    root: SessionId,
    agent_session: SessionId,
    handle: &str,
    parent: Option<&str>,
    agent_type: &str,
    mode: SubagentMode,
) -> Event {
    Event::AgentRegistered {
        session: root,
        agent_session,
        handle: handle.into(),
        parent: parent.map(Into::into),
        agent_type: AgentName::new(agent_type),
        mode,
    }
}

fn ev_dm(root: SessionId, channel: &str, a: &str, b: &str) -> Event {
    Event::ChannelCreated {
        session: root,
        channel: channel.into(),
        kind: ChannelKind::Dm,
        unit: None,
        members: vec![a.into(), b.into()],
    }
}

fn ev_mail(root: SessionId, from: &str, channel: &str, body: &str) -> Event {
    Event::MailSent {
        session: root,
        from: from.into(),
        to: MailEndpoint::Channel(channel.into()),
        kind: MailKind::Message,
        body: body.into(),
    }
}

fn ev_consumed(root: SessionId, handle: &str, through: u64) -> Event {
    Event::MailConsumed {
        session: root,
        handle: handle.into(),
        through,
    }
}

async fn exec_sql(store: &SessionStore, sql: &str) {
    let mut tx = store.begin_transaction().await.unwrap();
    sqlx::query(sql).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

async fn finalize_resident(
    store: &SessionStore,
    claim: &ActorClaim,
    root: SessionId,
    handle: &str,
    status: MemberRunStatus,
    note: &str,
) -> Result<Vec<Envelope>, StoreError> {
    let projection = store.read_projection(claim.actor_id).await?;
    let (outcome, envelopes) = store
        .finalize_resident_archive(
            ResidentArchiveOwner::Claim(claim),
            root,
            handle,
            ResidentArchive {
                actor: claim.actor_id,
                generation: projection.session.handoff.as_ref().map_or(1, |h| h.generation + 1),
                doc: "1. Goal - test task\n2. Current state - stopped\n3. Files and code - none\n4. Decisions - none\n5. Pending tasks - none\n6. Next step - none".into(),
                degraded: true,
                reason: hya_proto::ArchiveReason::Killed,
                note: note.into(),
                member_status: status,
                report: None,
                check_gate: false,
                expected_work: None,
            },
        )
        .await?;
    assert_eq!(outcome, ResidentArchiveOutcome::Committed);
    Ok(envelopes)
}

struct StopFixture {
    store: SessionStore,
    root: SessionId,
    actor: SessionId,
    handle: String,
    claim: ActorClaim,
    running_message: MessageId,
    running_part: PartId,
    running_call: ToolCallId,
    running_member: MemberId,
    running_child: SessionId,
    before_root: Vec<Envelope>,
    before_actor: Vec<Envelope>,
}

struct TempDb(String);

impl TempDb {
    fn new(boundary: &str) -> Self {
        Self(
            std::env::temp_dir()
                .join(format!(
                    "hya-resident-stop-{boundary}-{}.db",
                    SessionId::new()
                ))
                .to_string_lossy()
                .into_owned(),
        )
    }
    fn path(&self) -> &str {
        &self.0
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", ".runtime-owner.lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}

async fn stop_fixture_in(store: SessionStore) -> StopFixture {
    store.claim_runtime_owner(OwnerRunId::new()).unwrap();
    let root = SessionId::new();
    let actor = SessionId::new();
    let path = "main/resident-1".to_string();
    store
        .append_events(
            root,
            vec![
                ev_registered(
                    root,
                    actor,
                    "resident-1",
                    Some("main"),
                    "resident",
                    SubagentMode::Resident,
                ),
                ev_dm(root, "DM-12345678", "main", &path),
                ev_mail(root, "main", "DM-12345678", "accepted before stop"),
            ],
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
    store
        .append_events(
            actor,
            vec![
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
                    provider_data: None,
                },
                Event::MemberSpawned {
                    session: actor,
                    member: running_member,
                    child: Some(running_child),
                    subagent_type: AgentName::new("child"),
                    description: "running child".into(),
                    depth: 2,
                    directive: String::new(),
                    tool_call: None,
                },
                Event::MemberStatusChanged {
                    session: actor,
                    member: running_member,
                    status: MemberRunStatus::Running,
                },
            ],
        )
        .await
        .unwrap();

    StopFixture {
        before_root: store.replay(root).await.unwrap(),
        before_actor: store.replay(actor).await.unwrap(),
        store,
        root,
        actor,
        handle: path,
        claim,
        running_message,
        running_part,
        running_call,
        running_member,
        running_child,
    }
}

async fn stop_fixture_memory() -> StopFixture {
    stop_fixture_in(SessionStore::connect_memory().await.unwrap()).await
}

fn orphan_archive(actor: SessionId) -> ResidentArchive {
    ResidentArchive {
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

async fn work_fixture(store: &SessionStore) -> (SessionId, ActorClaim) {
    let root = SessionId::new();
    let actor = SessionId::new();
    store
        .append_event(root, &ev_created(root, None, "resident"))
        .await
        .unwrap();
    store
        .append_event(actor, &ev_created(actor, Some(root), "resident"))
        .await
        .unwrap();
    store
        .append_events(
            root,
            vec![
                ev_registered(
                    root,
                    actor,
                    "worker",
                    Some("main"),
                    "resident",
                    SubagentMode::Resident,
                ),
                ev_dm(root, "DM-12345678", "main", "main/worker"),
                ev_mail(root, "main", "DM-12345678", "first"),
                ev_mail(root, "main", "DM-12345678", "second"),
            ],
        )
        .await
        .unwrap();
    let claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
        .await
        .unwrap();
    (root, claim)
}

async fn revival_fixture(store: &SessionStore) -> (SessionId, ActorClaim) {
    let (root, claim) = work_fixture(store).await;
    store
        .append_events(
            root,
            vec![
                ev_registered(
                    root,
                    root,
                    "main",
                    None,
                    "resident",
                    SubagentMode::Transient,
                ),
                ev_dm(root, "DM-12345678", "main", "main/worker"),
                Event::ChannelCreated {
                    session: root,
                    channel: "announce-12345678".into(),
                    kind: ChannelKind::Group,
                    unit: Some("main".into()),
                    members: vec!["main".into(), "main/worker".into()],
                },
                ev_consumed(root, "main/worker", 1),
            ],
        )
        .await
        .unwrap();
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
    channel: &str,
    body: &str,
) -> Result<(Option<(ActorClaim, u64)>, Vec<Envelope>, usize), StoreError> {
    store
        .revive_episode(
            root,
            OwnerRunId::new(),
            u64::MAX,
            None,
            actor,
            "main".into(),
            channel.into(),
            MailKind::Message,
            body.into(),
            "HYA_COMPACTED_CONTEXT",
            None,
        )
        .await
}

#[tokio::test]
async fn claim_lifecycle_concurrency_recovery_release_and_non_owner_guard() {
    let store = SessionStore::connect_memory().await.unwrap();

    // 1. Concurrent claims allow exactly one owner.
    let actor_a = SessionId::new();
    let (left, right) = tokio::join!(
        store.try_claim_new(actor_a, actor_a, OwnerRunId::new()),
        store.try_claim_new(actor_a, actor_a, OwnerRunId::new()),
    );
    let outcomes = [left, right];
    assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| matches!(r, Err(StoreError::ActorAlreadyClaimed { actor_id }) if *actor_id == actor_a))
            .count(),
        1
    );

    // 2. Restart recovery increments epoch and invalidates old claim.
    let recovery_owner = OwnerRunId::new();
    store.claim_runtime_owner(recovery_owner).unwrap();
    let actor_b = SessionId::new();
    let old = store
        .try_claim_new(actor_b, actor_b, OwnerRunId::new())
        .await
        .unwrap();
    let recovered = store.recover_claim(actor_b, recovery_owner).await.unwrap();
    assert_eq!(recovered.previous_epoch, old.epoch);
    assert_eq!(recovered.claim.epoch.get(), old.epoch.get() + 1);
    assert!(store.validate_actor_claim(&recovered.claim).await.is_ok());
    assert!(matches!(
        store.validate_actor_claim(&old).await,
        Err(StoreError::StaleActorClaim { actor_id }) if actor_id == actor_b
    ));

    // 3. Release requires full tuple and is idempotent; re-claim bumps epoch.
    let actor_c = SessionId::new();
    let first = store
        .try_claim_new(actor_c, actor_c, OwnerRunId::new())
        .await
        .unwrap();
    store.release_claim(&first).await.unwrap();
    store.release_claim(&first).await.unwrap();
    let second = store
        .try_claim_new(actor_c, actor_c, OwnerRunId::new())
        .await
        .unwrap();
    assert_eq!(second.epoch.get(), first.epoch.get() + 1);
    assert!(matches!(
        store.release_claim(&first).await,
        Err(StoreError::StaleActorClaim { actor_id }) if actor_id == actor_c
    ));
    assert!(store.validate_actor_claim(&second).await.is_ok());

    // 4. Non-owner connection and mismatched owner id cannot take over a claim.
    let db = TempDb::new("non-owner-takeover");
    let rt = SessionStore::connect_runtime(db.path()).await.unwrap();
    let owner = OwnerRunId::new();
    rt.claim_runtime_owner(owner).unwrap();
    let actor_d = SessionId::new();
    let claim = rt.try_claim_new(actor_d, actor_d, owner).await.unwrap();
    let other = SessionStore::connect(db.path()).await.unwrap();
    for requested in [owner, OwnerRunId::new()] {
        assert!(matches!(
            other.recover_claim(actor_d, requested).await,
            Err(StoreError::RuntimeOwnerClaimRequired)
        ));
        rt.validate_actor_claim(&claim).await.unwrap();
    }
    assert!(matches!(
        rt.recover_claim(actor_d, OwnerRunId::new()).await,
        Err(StoreError::RuntimeOwnerClaimRequired)
    ));
    assert_eq!(
        rt.recover_claim(actor_d, owner)
            .await
            .unwrap()
            .previous_epoch,
        claim.epoch
    );
}

#[tokio::test]
async fn terminal_takeover_archives_active_claim_rolls_back_on_error_and_is_idempotent() {
    let f = stop_fixture_memory().await;
    exec_sql(
        &f.store,
        r#"CREATE TRIGGER fail_takeover_archive BEFORE INSERT ON event_log
           WHEN instr(NEW.payload, '"type":"agent_archived"') > 0
           BEGIN SELECT RAISE(ABORT, 'takeover archive failed'); END"#,
    )
    .await;
    assert!(matches!(
        f.store
            .finalize_resident_archive(
                ResidentArchiveOwner::Takeover,
                f.root,
                &f.handle,
                orphan_archive(f.actor)
            )
            .await,
        Err(StoreError::Sqlite(_))
    ));
    f.store.validate_actor_claim(&f.claim).await.unwrap();
    assert_eq!(f.store.replay(f.root).await.unwrap(), f.before_root);
    assert_eq!(f.store.replay(f.actor).await.unwrap(), f.before_actor);

    exec_sql(&f.store, "DROP TRIGGER fail_takeover_archive").await;
    let (outcome, events) = f
        .store
        .finalize_resident_archive(
            ResidentArchiveOwner::Takeover,
            f.root,
            &f.handle,
            orphan_archive(f.actor),
        )
        .await
        .unwrap();
    assert_eq!(outcome, ResidentArchiveOutcome::Committed);
    assert!(events.iter().any(|e| matches!(
        &e.event,
        Event::AgentArchived { child, reason: hya_proto::ArchiveReason::ArchivedByParent, .. } if *child == f.actor
    )));
    assert!(f.store.active_actor_ids().await.unwrap().is_empty());
    let proj = f.store.read_projection(f.root).await.unwrap();
    assert!(!proj.team.roster.contains_key(&f.handle));
    assert_eq!(proj.team.archived[&f.handle].session, f.actor);
    assert!(matches!(
        f.store.validate_actor_claim(&f.claim).await,
        Err(StoreError::StaleActorClaim { actor_id }) if actor_id == f.actor
    ));
    let mut tx = f.store.begin_transaction().await.unwrap();
    let epoch: i64 =
        sqlx::query_scalar("SELECT epoch FROM resident_actor_claim WHERE actor_id = ?")
            .bind(f.actor.storage_key())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(epoch as u64, f.claim.epoch.get() + 1);

    // Repeated takeover is idempotent after either Claim or Takeover archive.
    for first_takeover in [false, true] {
        let f = stop_fixture_memory().await;
        let owner = if first_takeover {
            ResidentArchiveOwner::Takeover
        } else {
            ResidentArchiveOwner::Claim(&f.claim)
        };
        f.store
            .finalize_resident_archive(owner, f.root, &f.handle, orphan_archive(f.actor))
            .await
            .unwrap();
        let before_root = f.store.replay(f.root).await.unwrap();
        let before_actor = f.store.replay(f.actor).await.unwrap();
        let (outcome, events) = f
            .store
            .finalize_resident_archive(
                ResidentArchiveOwner::Takeover,
                f.root,
                &f.handle,
                orphan_archive(f.actor),
            )
            .await
            .unwrap();
        assert_eq!(outcome, ResidentArchiveOutcome::Committed);
        assert!(events.is_empty());
        assert_eq!(f.store.replay(f.root).await.unwrap(), before_root);
        assert_eq!(f.store.replay(f.actor).await.unwrap(), before_actor);
    }
}

#[tokio::test]
async fn terminal_takeover_enforces_owner_gate_and_replay_preconditions() {
    // 1. Voluntary report gate is rejected without changing claim or logs.
    let f = stop_fixture_memory().await;
    let mut gated = orphan_archive(f.actor);
    gated.check_gate = true;
    gated.report = Some((hya_proto::ReportOutcome::Done, "report".into()));
    assert!(matches!(
        f.store.finalize_resident_archive(ResidentArchiveOwner::Takeover, f.root, &f.handle, gated).await,
        Err(StoreError::ActorClaimData(r)) if r == "voluntary report requires an episode claim"
    ));
    f.store.validate_actor_claim(&f.claim).await.unwrap();
    assert_eq!(f.store.replay(f.root).await.unwrap(), f.before_root);
    assert_eq!(f.store.replay(f.actor).await.unwrap(), f.before_actor);

    // 2. Non-owning store handle is refused.
    let temp_db = TempDb::new("takeover-owner");
    let ff = stop_fixture_in(SessionStore::connect(temp_db.path()).await.unwrap()).await;
    let observer = SessionStore::connect(temp_db.path()).await.unwrap();
    assert!(matches!(
        observer.claim_runtime_owner(OwnerRunId::new()),
        Err(StoreError::RuntimeOwnerBusy)
    ));
    assert!(matches!(
        observer
            .finalize_resident_archive(
                ResidentArchiveOwner::Takeover,
                ff.root,
                &ff.handle,
                orphan_archive(ff.actor)
            )
            .await,
        Err(StoreError::RuntimeOwnerClaimRequired)
    ));
    ff.store.validate_actor_claim(&ff.claim).await.unwrap();

    // 3. Released-claim replay requires matching claim, archive, root, handle, and actor.
    for mismatch in [
        "missing claim",
        "not archived",
        "wrong root",
        "wrong handle",
        "wrong actor",
    ] {
        let f = stop_fixture_memory().await;
        match mismatch {
            "missing claim" => {
                let mut tx = f.store.begin_transaction().await.unwrap();
                sqlx::query("DELETE FROM resident_actor_claim WHERE actor_id = ?")
                    .bind(f.actor.storage_key())
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            }
            "not archived" => f.store.release_claim(&f.claim).await.unwrap(),
            _ => {
                f.store
                    .finalize_resident_archive(
                        ResidentArchiveOwner::Claim(&f.claim),
                        f.root,
                        &f.handle,
                        orphan_archive(f.actor),
                    )
                    .await
                    .unwrap();
            }
        }
        let actor = if mismatch == "wrong actor" {
            let other = SessionId::new();
            let c = f
                .store
                .try_claim_new(other, other, OwnerRunId::new())
                .await
                .unwrap();
            f.store.release_claim(&c).await.unwrap();
            other
        } else {
            f.actor
        };
        let fetch_claim = || async {
            let mut tx = f.store.begin_transaction().await.unwrap();
            let row: Option<(i64, Vec<u8>, String)> = sqlx::query_as(
                "SELECT epoch, owner_run_id, state FROM resident_actor_claim WHERE actor_id = ?",
            )
            .bind(actor.storage_key())
            .fetch_optional(&mut *tx)
            .await
            .unwrap();
            tx.commit().await.unwrap();
            row
        };
        let before_claim = fetch_claim().await;
        let before_root = f.store.replay(f.root).await.unwrap();
        let before_actor = f.store.replay(actor).await.unwrap();
        let res = f
            .store
            .finalize_resident_archive(
                ResidentArchiveOwner::Takeover,
                if mismatch == "wrong root" {
                    SessionId::new()
                } else {
                    f.root
                },
                if mismatch == "wrong handle" {
                    "main/other"
                } else {
                    &f.handle
                },
                orphan_archive(actor),
            )
            .await;
        assert!(
            matches!(res, Err(StoreError::ActorClaimUnavailable { actor_id }) if actor_id == actor),
            "{mismatch}"
        );
        assert_eq!(fetch_claim().await, before_claim, "{mismatch}");
        assert_eq!(f.store.replay(f.root).await.unwrap(), before_root);
        assert_eq!(f.store.replay(actor).await.unwrap(), before_actor);
    }
}

#[tokio::test]
async fn resident_stop_finalization_commits_all_effects_and_matches_member_outcomes() {
    // 1. Full stop finalization closes member/tool/message, preserves unread mail, releases claim.
    let f = stop_fixture_memory().await;
    let events = finalize_resident(
        &f.store,
        &f.claim,
        f.root,
        &f.handle,
        MemberRunStatus::Cancelled,
        "resident stopped",
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 6);
    assert!(matches!(
        &events[0].event,
        Event::MemberFinished { session, member, status: MemberRunStatus::Cancelled, summary, child }
            if *session == f.actor && *member == f.running_member && summary == "resident stopped" && *child == Some(f.running_child)
    ));
    assert!(matches!(
        &events[1].event,
        Event::ToolError { session, message, part, call, message_text, value }
            if *session == f.actor && *message == f.running_message && *part == f.running_part
                && *call == f.running_call && message_text == "resident stopped"
                && value == &Some(serde_json::json!({"code": "STALE_ACTOR_CLAIM"}))
    ));
    assert!(matches!(
        &events[2].event,
        Event::MessageFinished { session, message, role: Role::Assistant, finish: FinishReason::Cancelled, .. }
            if *session == f.actor && *message == f.running_message
    ));
    assert!(matches!(
        &events[3].event,
        Event::AgentActivityChanged { session, handle, status: RosterStatus::Failed, current_task: Some(task) }
            if *session == f.root && handle == &f.handle && task == "resident stopped"
    ));

    let proj = f.store.read_projection(f.root).await.unwrap();
    assert!(!proj.team.roster.contains_key(&f.handle));
    assert_eq!(proj.team.archived[&f.handle].session, f.actor);
    assert_eq!(proj.team.inboxes.get(&f.handle).map_or(0, Vec::len), 1);
    let mut pre_archive = hya_proto::Projection::default();
    for env in f.store.replay(f.root).await.unwrap() {
        if matches!(&env.event, Event::AgentArchived { child, .. } if *child == f.actor) {
            break;
        }
        pre_archive.apply(&env);
    }
    assert_eq!(
        pre_archive.team.roster[&f.handle].status,
        RosterStatus::Failed
    );
    assert_eq!(pre_archive.team.roster[&f.handle].resident_cursor, 0);
    assert!(!f.store.active_actor_ids().await.unwrap().contains(&f.actor));
    assert!(matches!(
        f.store.validate_actor_claim(&f.claim).await,
        Err(StoreError::StaleActorClaim { actor_id }) if actor_id == f.actor
    ));
    let aproj = f.store.read_projection(f.actor).await.unwrap();
    assert_eq!(
        aproj
            .session
            .members
            .iter()
            .find(|m| m.member == f.running_member)
            .unwrap()
            .status,
        MemberRunStatus::Cancelled
    );
    let msg = aproj
        .session
        .messages
        .iter()
        .find(|m| m.id == f.running_message)
        .unwrap();
    assert_eq!(msg.finish, Some(FinishReason::Cancelled));
    assert!(matches!(
        msg.parts.iter().find(|p| p.id() == f.running_part).unwrap(),
        PartProjection::Tool { state: ToolPartState::Error { message, value, .. }, .. }
            if message == "resident stopped" && value == &Some(serde_json::json!({"code": "STALE_ACTOR_CLAIM"}))
    ));

    // 2. Finalizing a Done root resident still on roster is idempotent.
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    let handle = hya_proto::scope::ROOT_HANDLE;
    store
        .append_event(
            root,
            &ev_registered(root, root, handle, None, "resident", SubagentMode::Resident),
        )
        .await
        .unwrap();
    let claim = store
        .try_claim_new(root, root, OwnerRunId::new())
        .await
        .unwrap();
    let first = finalize_resident(
        &store,
        &claim,
        root,
        handle,
        MemberRunStatus::Done,
        "finished",
    )
    .await
    .unwrap();
    assert!(first.iter().any(|e| matches!(
        e.event,
        Event::AgentActivityChanged {
            status: RosterStatus::Done,
            ..
        }
    )));
    let history = store.replay(root).await.unwrap();
    assert!(
        finalize_resident(
            &store,
            &claim,
            root,
            handle,
            MemberRunStatus::Done,
            "finished"
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert_eq!(store.replay(root).await.unwrap(), history);

    // 3. Archive activity status matches member outcome (Done/Cancelled/Failed).
    for (status, expected) in [
        (MemberRunStatus::Done, RosterStatus::Done),
        (MemberRunStatus::Cancelled, RosterStatus::Failed),
        (MemberRunStatus::Failed, RosterStatus::Failed),
    ] {
        let f = stop_fixture_memory().await;
        let mut archive = orphan_archive(f.actor);
        archive.member_status = status;
        if status == MemberRunStatus::Done {
            f.store
                .append_event(f.root, &ev_consumed(f.root, &f.handle, 1))
                .await
                .unwrap();
            archive.check_gate = true;
            archive.reason = hya_proto::ArchiveReason::Reported;
            archive.report = Some((hya_proto::ReportOutcome::Done, "finished".into()));
        }
        let (outcome, events) = f
            .store
            .finalize_resident_archive(
                ResidentArchiveOwner::Claim(&f.claim),
                f.root,
                &f.handle,
                archive,
            )
            .await
            .unwrap();
        assert_eq!(outcome, ResidentArchiveOutcome::Committed);
        assert!(events.iter().any(|e| matches!(
            &e.event,
            Event::AgentActivityChanged { handle, status, .. } if handle == &f.handle && *status == expected
        )));
    }

    // 4. Finalization closes parent-log member row as Cancelled or Failed.
    for (status, note) in [
        (MemberRunStatus::Cancelled, "resident stopped"),
        (MemberRunStatus::Failed, "team budget exceeded"),
    ] {
        let store = SessionStore::connect_memory().await.unwrap();
        let (root, actor, member) = (SessionId::new(), SessionId::new(), MemberId::new());
        store
            .append_event(root, &ev_created(root, None, "resident"))
            .await
            .unwrap();
        store
            .append_event(actor, &ev_created(actor, Some(root), "resident"))
            .await
            .unwrap();
        store
            .append_events(
                root,
                vec![
                    ev_registered(
                        root,
                        actor,
                        "resident-1",
                        Some("main"),
                        "resident",
                        SubagentMode::Resident,
                    ),
                    Event::MemberSpawned {
                        session: root,
                        member,
                        child: Some(actor),
                        subagent_type: AgentName::new("resident"),
                        description: "worker".into(),
                        depth: 1,
                        directive: String::new(),
                        tool_call: Some(ToolCallId::new()),
                    },
                    Event::MemberStatusChanged {
                        session: root,
                        member,
                        status: MemberRunStatus::Running,
                    },
                ],
            )
            .await
            .unwrap();
        let claim = store
            .try_claim_new(root, actor, OwnerRunId::new())
            .await
            .unwrap();
        let events = finalize_resident(&store, &claim, root, "main/resident-1", status, note)
            .await
            .unwrap();
        let finishes: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.event {
                Event::MemberFinished {
                    session,
                    member: r,
                    status,
                    child,
                    ..
                } if *r == member => Some((*session, *status, *child)),
                _ => None,
            })
            .collect();
        assert_eq!(finishes, vec![(root, status, Some(actor))]);
        assert_eq!(
            store
                .read_projection(root)
                .await
                .unwrap()
                .session
                .members
                .into_iter()
                .find(|r| r.member == member)
                .unwrap()
                .status,
            status
        );
    }
}

#[tokio::test]
async fn resident_mail_remains_epoch_independent_until_explicit_stop() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let (root, actor, path) = (SessionId::new(), SessionId::new(), "main/resident-1");
    store
        .append_events(
            root,
            vec![
                ev_registered(
                    root,
                    actor,
                    "resident-1",
                    Some("main"),
                    "resident",
                    SubagentMode::Resident,
                ),
                ev_dm(root, "DM-12345678", "main", path),
            ],
        )
        .await
        .unwrap();

    let old = store
        .try_claim_new(root, actor, OwnerRunId::new())
        .await
        .unwrap();
    let mail = store
        .append_channel_mail(
            root,
            "main".into(),
            "DM-12345678".into(),
            MailKind::Message,
            "epoch independent".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        &mail.0.event,
        Event::MailSent { session, from, to: MailEndpoint::Channel(r), kind: MailKind::Message, body }
            if *session == root && from == "main" && r == "DM-12345678" && body == "epoch independent"
    ));

    let recovered = store.recover_claim(actor, owner).await.unwrap();
    assert_eq!(recovered.previous_epoch, old.epoch);
    assert!(
        matches!(store.validate_actor_claim(&old).await, Err(StoreError::StaleActorClaim { actor_id }) if actor_id == actor)
    );
    let proj = store.read_projection(root).await.unwrap();
    assert_eq!(proj.team.inboxes[path].len(), 1);
    assert_eq!(proj.team.roster[path].resident_cursor, 0);

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
            .filter(|e| matches!(
                &e.event,
                Event::AgentActivityChanged {
                    status: RosterStatus::Failed,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(!store.active_actor_ids().await.unwrap().contains(&actor));
    assert!(
        finalize_resident(
            &store,
            &recovered.claim,
            root,
            path,
            MemberRunStatus::Cancelled,
            "resident stopped"
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn resident_stop_finalizer_rolls_back_at_each_write_boundary_and_is_exactly_once() {
    let cases = [
        (
            "member_finished",
            r#"BEFORE INSERT ON event_log WHEN instr(NEW.payload, '"type":"member_finished"') > 0"#,
        ),
        (
            "tool_error",
            r#"BEFORE INSERT ON event_log WHEN instr(NEW.payload, '"type":"tool_error"') > 0"#,
        ),
        (
            "message_finished",
            r#"BEFORE INSERT ON event_log WHEN instr(NEW.payload, '"type":"message_finished"') > 0"#,
        ),
        (
            "root_activity",
            r#"BEFORE INSERT ON event_log WHEN instr(NEW.payload, '"type":"agent_activity_changed"') > 0 AND instr(NEW.payload, '"current_task":"resident stopped"') > 0"#,
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
            "BEFORE UPDATE OF state ON resident_actor_claim WHEN NEW.state = 'released' AND OLD.state = 'active'",
        ),
    ];

    for (boundary, trigger_body) in cases {
        let f = stop_fixture_memory().await;
        let trig = format!("test_stop_fail_{boundary}");
        exec_sql(&f.store, &format!("CREATE TRIGGER {trig} {trigger_body} BEGIN SELECT RAISE(ABORT, 'test stop boundary'); END;")).await;

        assert!(
            finalize_resident(
                &f.store,
                &f.claim,
                f.root,
                &f.handle,
                MemberRunStatus::Cancelled,
                "resident stopped"
            )
            .await
            .is_err(),
            "{boundary}"
        );
        assert_eq!(
            f.store.replay(f.root).await.unwrap(),
            f.before_root,
            "{boundary}"
        );
        assert_eq!(
            f.store.replay(f.actor).await.unwrap(),
            f.before_actor,
            "{boundary}"
        );
        f.store.validate_actor_claim(&f.claim).await.unwrap();
        assert!(f.store.active_actor_ids().await.unwrap().contains(&f.actor));

        exec_sql(&f.store, &format!("DROP TRIGGER {trig}")).await;
        assert_eq!(
            finalize_resident(
                &f.store,
                &f.claim,
                f.root,
                &f.handle,
                MemberRunStatus::Cancelled,
                "resident stopped"
            )
            .await
            .unwrap()
            .len(),
            6,
            "{boundary}"
        );
        assert!(
            matches!(f.store.validate_actor_claim(&f.claim).await, Err(StoreError::StaleActorClaim { actor_id }) if actor_id == f.actor)
        );

        let after_retry = f.store.replay(f.root).await.unwrap();
        assert!(
            finalize_resident(
                &f.store,
                &f.claim,
                f.root,
                &f.handle,
                MemberRunStatus::Cancelled,
                "resident stopped"
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert_eq!(f.store.replay(f.root).await.unwrap(), after_retry);

        assert!(matches!(
            f.store.append_channel_mail(f.root, "main".into(), "DM-12345678".into(), MailKind::Message, "after stop".into(), None, None).await,
            Err(StoreError::MailRevivalRequired { ref sender, ref channel, ref target })
                if sender == "main" && channel == "DM-12345678" && target == &f.handle
        ));
        assert_eq!(f.store.replay(f.root).await.unwrap(), after_retry);
    }
}

#[tokio::test]
async fn refused_report_commits_only_a_harness_notice() {
    for refusal in [
        "unread mail",
        "live subagent",
        "work changed",
        "generation changed",
    ] {
        let f = stop_fixture_memory().await;
        if refusal != "unread mail" {
            f.store
                .append_event(f.root, &ev_consumed(f.root, &f.handle, 1))
                .await
                .unwrap();
        }
        if refusal == "live subagent" {
            f.store
                .append_event(
                    f.root,
                    &ev_registered(
                        f.root,
                        SessionId::new(),
                        "nested",
                        Some(&f.handle),
                        "resident",
                        SubagentMode::Resident,
                    ),
                )
                .await
                .unwrap();
        }
        let before_root = f.store.replay(f.root).await.unwrap();
        let archive = || ResidentArchive {
            actor: f.actor,
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

        exec_sql(&f.store, r#"CREATE TRIGGER fail_refusal_notice BEFORE INSERT ON event_log WHEN instr(NEW.payload, '"type":"mail_sent"') > 0 BEGIN SELECT RAISE(ABORT, 'notice insert failed'); END"#).await;
        assert!(matches!(
            f.store
                .finalize_resident_archive(
                    ResidentArchiveOwner::Claim(&f.claim),
                    f.root,
                    &f.handle,
                    archive()
                )
                .await,
            Err(StoreError::Sqlite(_))
        ));
        assert_eq!(f.store.replay(f.root).await.unwrap(), before_root);
        assert_eq!(f.store.replay(f.actor).await.unwrap(), f.before_actor);
        f.store.validate_actor_claim(&f.claim).await.unwrap();

        exec_sql(&f.store, "DROP TRIGGER fail_refusal_notice").await;
        let (outcome, events) = f
            .store
            .finalize_resident_archive(
                ResidentArchiveOwner::Claim(&f.claim),
                f.root,
                &f.handle,
                archive(),
            )
            .await
            .unwrap();
        assert_eq!(outcome, ResidentArchiveOutcome::Refused);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].event,
            Event::MailSent { session, from, to: MailEndpoint::Handle(h), body, .. }
                if *session == f.root && from == hya_proto::scope::HARNESS_HANDLE && h == &f.handle
                    && body.contains("Your report was not accepted")
                    && body.contains(if refusal.ends_with("changed") { "episode changed" } else { refusal })
        ));
        let mut expected = before_root;
        expected.extend(events);
        assert_eq!(f.store.replay(f.root).await.unwrap(), expected);
        assert_eq!(f.store.replay(f.actor).await.unwrap(), f.before_actor);
        f.store.validate_actor_claim(&f.claim).await.unwrap();
        let proj = f.store.read_projection(f.root).await.unwrap();
        assert!(
            proj.team.roster.contains_key(&f.handle) && !proj.team.archived.contains_key(&f.handle)
        );
        assert!(
            proj.team.roster[&f.handle].resident_cursor < proj.team.inboxes[&f.handle].len() as u64
        );
    }
}

#[tokio::test]
async fn registration_atomicity_writer_reservation_and_concurrent_capacity() {
    // 1. Roll back claim and all sessions when directive mail fails.
    let mem = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    mem.append_event(root, &ev_created(root, None, "main"))
        .await
        .unwrap();
    let before = mem.replay(root).await.unwrap();
    exec_sql(&mem, "CREATE TRIGGER reject_directive_mail BEFORE INSERT ON event_log WHEN instr(NEW.payload, 'reject_directive_mail') > 0 BEGIN SELECT RAISE(ABORT, 'prompt failure'); END").await;
    let regs = ["first", "reject_directive_mail"]
        .into_iter()
        .map(|directive| {
            let child = SessionId::new();
            let dm = hya_proto::mint_channel_id(ChannelKind::Dm);
            hya_store::EpisodeRegistration {
                actor: child,
                events: vec![
                    ev_created(child, Some(root), "worker"),
                    ev_registered(
                        root,
                        child,
                        &child.to_string(),
                        Some("main"),
                        "worker",
                        SubagentMode::Resident,
                    ),
                    ev_dm(root, &dm, "main", &format!("main/{child}")),
                    ev_mail(root, "main", &dm, directive),
                ],
            }
        })
        .collect();
    assert!(
        mem.register_episodes(root, OwnerRunId::new(), None, 2, regs)
            .await
            .unwrap_err()
            .to_string()
            .contains("prompt failure")
    );
    assert_eq!(mem.replay(root).await.unwrap(), before);
    assert_eq!(mem.list_sessions().await.unwrap().len(), 1);
    assert_eq!(mem.live_member_count(root).await.unwrap(), 0);
    assert!(mem.active_actor_ids().await.unwrap().is_empty());

    // 2. BEGIN IMMEDIATE reserves SQLite before reading; concurrent batches share capacity.
    let db = TempDb::new("registration-lock-and-capacity");
    let store = SessionStore::connect(db.path()).await.unwrap();
    let mut writer = SqliteConnection::connect(&format!("sqlite://{}", db.path()))
        .await
        .unwrap();
    let held = writer.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(40),
            store.begin_transaction()
        )
        .await
        .is_err()
    );
    held.rollback().await.unwrap();
    store
        .begin_transaction()
        .await
        .unwrap()
        .rollback()
        .await
        .unwrap();

    let cap_root = SessionId::new();
    store
        .append_event(cap_root, &ev_created(cap_root, None, "main"))
        .await
        .unwrap();
    let batch = || {
        (0..2)
            .map(|_| {
                let actor = SessionId::new();
                hya_store::EpisodeRegistration {
                    actor,
                    events: vec![
                        ev_created(actor, Some(cap_root), "worker"),
                        ev_registered(
                            cap_root,
                            actor,
                            &actor.to_string(),
                            Some("main"),
                            "worker",
                            SubagentMode::Resident,
                        ),
                    ],
                }
            })
            .collect()
    };
    let (left, right) = tokio::join!(
        store.register_episodes(cap_root, OwnerRunId::new(), None, 2, batch()),
        store.register_episodes(cap_root, OwnerRunId::new(), None, 2, batch()),
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    assert!(matches!(
        left.err().or_else(|| right.err()).unwrap(),
        StoreError::CapacityExceeded {
            live: 2,
            requested: 2,
            limit: 2,
            ..
        }
    ));
    assert_eq!(store.live_member_count(cap_root).await.unwrap(), 2);
    assert_eq!(store.list_sessions().await.unwrap().len(), 3);
    assert_eq!(
        store
            .read_projection(cap_root)
            .await
            .unwrap()
            .team
            .roster
            .len(),
        3
    );
    assert_eq!(
        store.replay(cap_root).await.unwrap().iter().filter(|e| matches!(e.event, Event::AgentRegistered { agent_session, .. } if agent_session == cap_root)).count(),
        1
    );
}

#[tokio::test]
async fn begin_work_commits_resumes_rejects_stale_and_rolls_back_lead() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, claim) = work_fixture(&store).await;
    let (message, envelopes) = store
        .begin_work(
            Some(&claim),
            root,
            "main/worker",
            2,
            ResidentWorkPrompt::Text("[mail from main] first\n\n[mail from main] second".into()),
            "mail from main".into(),
        )
        .await
        .unwrap();
    assert_eq!(envelopes.len(), 7);
    let child = store.read_projection(claim.actor_id).await.unwrap();
    assert_eq!(child.session.messages.len(), 1);
    assert_eq!(
        (child.session.messages[0].id, child.session.messages[0].role),
        (message, Role::User)
    );
    let entry = &store.read_projection(root).await.unwrap().team.roster["main/worker"];
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

    // Stale claim rejected with no writes.
    let recovery_owner = OwnerRunId::new();
    store.claim_runtime_owner(recovery_owner).unwrap();
    let (root2, stale) = work_fixture(&store).await;
    store
        .recover_claim(stale.actor_id, recovery_owner)
        .await
        .unwrap();
    let (br, bc) = (
        store.replay(root2).await.unwrap(),
        store.replay(stale.actor_id).await.unwrap(),
    );
    assert!(matches!(
        store.begin_work(Some(&stale), root2, "main/worker", 2, ResidentWorkPrompt::Text("stale".into()), "mail".into()).await,
        Err(StoreError::StaleActorClaim { actor_id }) if actor_id == stale.actor_id
    ));
    assert_eq!(store.replay(root2).await.unwrap(), br);
    assert_eq!(store.replay(stale.actor_id).await.unwrap(), bc);

    // Lead consumption rolls back atomically with prompt, then succeeds.
    let lead_store = SessionStore::connect_memory().await.unwrap();
    let (lroot, _) = work_fixture(&lead_store).await;
    let before = lead_store.replay(lroot).await.unwrap();
    exec_sql(&lead_store, "CREATE TRIGGER reject_lead BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = 'mail_consumed' BEGIN SELECT RAISE(ABORT, 'lead work failure'); END").await;
    assert!(
        lead_store
            .begin_work(
                None,
                lroot,
                "main",
                2,
                ResidentWorkPrompt::Text("lead work".into()),
                "mail".into()
            )
            .await
            .is_err()
    );
    assert_eq!(lead_store.replay(lroot).await.unwrap(), before);
    exec_sql(&lead_store, "DROP TRIGGER reject_lead").await;
    let (msg, envs) = lead_store
        .begin_work(
            None,
            lroot,
            "main",
            2,
            ResidentWorkPrompt::Text("lead work".into()),
            "mail".into(),
        )
        .await
        .unwrap();
    assert!(
        envs.iter()
            .any(|e| matches!(&e.event, Event::MailConsumed { through: 2, .. }))
    );
    assert_eq!(
        lead_store
            .read_projection(lroot)
            .await
            .unwrap()
            .session
            .messages[0]
            .id,
        msg
    );

    // Resume unanswered prompt rolls back on trigger error, then coalesces.
    let res_store = SessionStore::connect_memory().await.unwrap();
    let (rroot, rclaim) = work_fixture(&res_store).await;
    let (session, msg_id, part) = (rclaim.actor_id, MessageId::new(), PartId::new());
    res_store
        .append_events(
            session,
            vec![
                Event::MessageStarted {
                    session,
                    message: msg_id,
                    role: Role::User,
                    agent: None,
                    model: None,
                },
                Event::TextStart {
                    session,
                    message: msg_id,
                    part,
                },
                Event::TextDelta {
                    session,
                    message: msg_id,
                    part,
                    delta: "unfinished wake".into(),
                },
                Event::TextEnd {
                    session,
                    message: msg_id,
                    part,
                },
                Event::MessageFinished {
                    session,
                    message: msg_id,
                    role: Role::User,
                    finish: FinishReason::Stop,
                    tokens: None,
                    cause: None,
                },
            ],
        )
        .await
        .unwrap();
    let (br, bc) = (
        res_store.replay(rroot).await.unwrap(),
        res_store.replay(session).await.unwrap(),
    );
    exec_sql(&res_store, "CREATE TRIGGER reject_res BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = 'resident_work_started' BEGIN SELECT RAISE(ABORT, 'work failure'); END").await;
    assert!(
        res_store
            .begin_work(
                Some(&rclaim),
                rroot,
                "main/worker",
                2,
                ResidentWorkPrompt::Resume {
                    message: msg_id,
                    append: "first\n\nsecond".into()
                },
                "mail".into()
            )
            .await
            .is_err()
    );
    assert_eq!(res_store.replay(rroot).await.unwrap(), br);
    assert_eq!(res_store.replay(session).await.unwrap(), bc);
    exec_sql(&res_store, "DROP TRIGGER reject_res").await;
    let (owned, _) = res_store
        .begin_work(
            Some(&rclaim),
            rroot,
            "main/worker",
            2,
            ResidentWorkPrompt::Resume {
                message: msg_id,
                append: "first\n\nsecond".into(),
            },
            "mail".into(),
        )
        .await
        .unwrap();
    assert_eq!(owned, msg_id);
    let proj = res_store.read_projection(session).await.unwrap();
    assert_eq!(proj.session.messages.len(), 1);
    assert!(
        matches!(&proj.session.messages[0].parts[0], PartProjection::Text { text, .. } if text == "unfinished wake\n\nfirst\n\nsecond")
    );
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
        let store = SessionStore::connect_memory().await.unwrap();
        let (root, claim) = work_fixture(&store).await;
        let (br, bc) = (
            store.replay(root).await.unwrap(),
            store.replay(claim.actor_id).await.unwrap(),
        );
        exec_sql(&store, &format!("CREATE TRIGGER reject_work BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = '{boundary}' BEGIN SELECT RAISE(ABORT, 'work failure'); END")).await;
        assert!(
            store
                .begin_work(
                    Some(&claim),
                    root,
                    "main/worker",
                    2,
                    ResidentWorkPrompt::Text("coalesced work".into()),
                    "mail".into()
                )
                .await
                .is_err(),
            "{boundary}"
        );
        assert_eq!(store.replay(root).await.unwrap(), br, "{boundary}");
        assert_eq!(
            store.replay(claim.actor_id).await.unwrap(),
            bc,
            "{boundary}"
        );
        store.validate_actor_claim(&claim).await.unwrap();
    }
}

#[tokio::test]
async fn revive_episode_preserves_channels_rechecks_policy_and_rejects_archived_senders() {
    // 1. Revival preserves channels, skips prior mail, injects system boundary, deduplicates restart.
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, old) = revival_fixture(&store).await;
    let channels = store.read_projection(root).await.unwrap().team.channels;
    let (claim, envelopes, recipients) =
        revive_mail(&store, root, old.actor_id, "DM-12345678", "new work")
            .await
            .unwrap();
    assert_eq!(claim.unwrap().0.epoch.get(), old.epoch.get() + 1);
    assert_eq!((envelopes.len(), recipients), (8, 1));
    let proj = store.read_projection(root).await.unwrap();
    assert_eq!(
        proj.team.channels.keys().collect::<Vec<_>>(),
        channels.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        proj.team.channels["DM-12345678"].members,
        channels["DM-12345678"].members
    );
    assert!(
        !channels["announce-12345678"]
            .members
            .contains("main/worker")
    );
    assert!(
        proj.team.channels["announce-12345678"]
            .members
            .contains("main/worker")
    );
    assert!(!proj.team.archived.contains_key("main/worker"));
    assert_eq!(proj.team.roster["main/worker"].resident_cursor, 2);
    let inbox = &proj.team.inboxes["main/worker"];
    assert_eq!(inbox.len(), 3);
    assert_eq!(proj.team.mail[inbox[2]].body, "new work");
    assert_eq!(
        store
            .read_projection(old.actor_id)
            .await
            .unwrap()
            .session
            .messages
            .last()
            .unwrap()
            .role,
        Role::System
    );

    let (second_claim, _, recipients) =
        revive_mail(&store, root, old.actor_id, "DM-12345678", "concurrent work")
            .await
            .unwrap();
    assert!(second_claim.is_none());
    assert_eq!(recipients, 2);
    assert_eq!(
        store
            .replay(root)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(e.event, Event::AgentRestarted { .. }))
            .count(),
        1
    );

    // 2. Policy denial and stale parent claim write nothing.
    let s2 = SessionStore::connect_memory().await.unwrap();
    let (r2, old2) = revival_fixture(&s2).await;
    let before = s2.replay(r2).await.unwrap();
    assert!(matches!(
        s2.revive_episode(
            r2,
            OwnerRunId::new(),
            u64::MAX,
            None,
            old2.actor_id,
            "main".into(),
            "DM-12345678".into(),
            MailKind::Message,
            "denied".into(),
            "HYA_COMPACTED_CONTEXT",
            Some(hya_proto::mail::ChannelPolicySnapshot {
                dm_parent: 0,
                ..Default::default()
            })
        )
        .await,
        Err(StoreError::MailAuthorizationRejected { .. })
    ));
    assert!(matches!(
        s2.revive_episode(
            r2,
            OwnerRunId::new(),
            u64::MAX,
            Some(&old2),
            old2.actor_id,
            "main".into(),
            "DM-12345678".into(),
            MailKind::Message,
            "stale".into(),
            "HYA_COMPACTED_CONTEXT",
            None
        )
        .await,
        Err(StoreError::StaleActorClaim { .. })
    ));
    assert_eq!(s2.replay(r2).await.unwrap(), before);
    assert!(s2.active_actor_ids().await.unwrap().is_empty());

    // 3. Archived sender cannot append to retained DM or revive a child.
    let s3 = SessionStore::connect_memory().await.unwrap();
    let (r3, parent) = work_fixture(&s3).await;
    let child = SessionId::new();
    s3.append_event(child, &ev_created(child, Some(parent.actor_id), "resident"))
        .await
        .unwrap();
    s3.append_events(
        r3,
        vec![
            ev_registered(
                r3,
                child,
                "child",
                Some("main/worker"),
                "resident",
                SubagentMode::Resident,
            ),
            ev_dm(r3, "DM-87654321", "main/worker", "main/worker/child"),
        ],
    )
    .await
    .unwrap();
    let child_claim = s3
        .try_claim_new(r3, child, OwnerRunId::new())
        .await
        .unwrap();
    finalize_resident(
        &s3,
        &child_claim,
        r3,
        "main/worker/child",
        MemberRunStatus::Cancelled,
        "archived",
    )
    .await
    .unwrap();
    let parent_dm = s3
        .read_projection(r3)
        .await
        .unwrap()
        .team
        .resolve_dm("main/worker", "main")
        .unwrap();
    let child_dm = s3
        .read_projection(r3)
        .await
        .unwrap()
        .team
        .resolve_dm("main/worker", "child")
        .unwrap();
    finalize_resident(
        &s3,
        &parent,
        r3,
        "main/worker",
        MemberRunStatus::Cancelled,
        "archived",
    )
    .await
    .unwrap();
    let (br, bc) = (
        s3.replay(r3).await.unwrap(),
        s3.replay(child).await.unwrap(),
    );
    assert!(matches!(
        s3.append_channel_mail(
            r3,
            "main/worker".into(),
            parent_dm,
            MailKind::Message,
            "stale".into(),
            None,
            None
        )
        .await,
        Err(StoreError::MailAuthorizationRejected { .. })
    ));
    assert!(matches!(
        s3.revive_episode(
            r3,
            OwnerRunId::new(),
            u64::MAX,
            None,
            child,
            "main/worker".into(),
            child_dm,
            MailKind::Message,
            "stale".into(),
            "HYA_COMPACTED_CONTEXT",
            None
        )
        .await,
        Err(StoreError::MailAuthorizationRejected { .. })
    ));
    assert_eq!(s3.replay(r3).await.unwrap(), br);
    assert_eq!(s3.replay(child).await.unwrap(), bc);
    assert!(s3.active_actor_ids().await.unwrap().is_empty());
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
        let store = SessionStore::connect_memory().await.unwrap();
        let (root, old) = revival_fixture(&store).await;
        let (br, ba) = (
            store.replay(root).await.unwrap(),
            store.replay(old.actor_id).await.unwrap(),
        );
        let sql = if tag == "claim" {
            "CREATE TRIGGER reject_revive BEFORE UPDATE ON resident_actor_claim BEGIN SELECT RAISE(ABORT, 'injected revive'); END".into()
        } else {
            format!(
                "CREATE TRIGGER reject_revive BEFORE INSERT ON event_log WHEN NEW.payload LIKE '%\"{tag}\"%' BEGIN SELECT RAISE(ABORT, 'injected revive'); END"
            )
        };
        exec_sql(&store, &sql).await;
        assert!(
            revive_mail(&store, root, old.actor_id, "DM-12345678", "must not send")
                .await
                .is_err(),
            "{tag}"
        );
        assert_eq!(store.replay(root).await.unwrap(), br, "{tag}");
        assert_eq!(store.replay(old.actor_id).await.unwrap(), ba, "{tag}");
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
        exec_sql(&store, "DROP TRIGGER reject_revive").await;
        let (claim, _, _) = revive_mail(&store, root, old.actor_id, "DM-12345678", "retry")
            .await
            .unwrap();
        assert_eq!(claim.unwrap().0.epoch.get(), old.epoch.get() + 1);
    }
}

#[tokio::test]
async fn resident_recovery_handles_cancelled_aborted_reasoning_only_and_outgoing_dm() {
    // 1. User-cancelled work after mail acknowledgement still recovers as Queued.
    {
        let store = SessionStore::connect_memory().await.unwrap();
        let owner = OwnerRunId::new();
        store.claim_runtime_owner(owner).unwrap();
        let (root, claim) = work_fixture(&store).await;
        let (prompt, _) = store
            .begin_work(
                Some(&claim),
                root,
                "main/worker",
                2,
                ResidentWorkPrompt::Text("already admitted".into()),
                "working".into(),
            )
            .await
            .unwrap();
        store
            .append_events(
                root,
                vec![
                    ev_consumed(root, "main/worker", 2),
                    Event::AgentActivityChanged {
                        session: root,
                        handle: "main/worker".into(),
                        status: RosterStatus::Idle,
                        current_task: None,
                    },
                ],
            )
            .await
            .unwrap();
        let recovered = store.recover_claim(claim.actor_id, owner).await.unwrap();
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

    // 2. Aborted first wake (none / empty / reasoning-only / partial text) closes unanswered prompt.
    #[derive(Clone, Copy)]
    enum State {
        None,
        EmptyText,
        ReasoningOnly,
        PartialText,
    }
    for state in [
        State::None,
        State::EmptyText,
        State::ReasoningOnly,
        State::PartialText,
    ] {
        let store = SessionStore::connect_memory().await.unwrap();
        let owner = OwnerRunId::new();
        store.claim_runtime_owner(owner).unwrap();
        let (root, claim) = work_fixture(&store).await;
        let (prompt, _) = store
            .begin_work(
                Some(&claim),
                root,
                "main/worker",
                1,
                ResidentWorkPrompt::Text("aborted".into()),
                "working".into(),
            )
            .await
            .unwrap();
        let (msg, part) = (MessageId::new(), PartId::new());
        let a = claim.actor_id;
        match state {
            State::None => {}
            State::EmptyText | State::PartialText => {
                let mut evs = vec![
                    Event::MessageStarted {
                        session: a,
                        message: msg,
                        role: Role::Assistant,
                        agent: None,
                        model: None,
                    },
                    Event::TextStart {
                        session: a,
                        message: msg,
                        part,
                    },
                ];
                if matches!(state, State::PartialText) {
                    evs.push(Event::TextDelta {
                        session: a,
                        message: msg,
                        part,
                        delta: "partial answer".into(),
                    });
                }
                store.append_events(a, evs).await.unwrap();
            }
            State::ReasoningOnly => {
                store
                    .append_events(
                        a,
                        vec![
                            Event::MessageStarted {
                                session: a,
                                message: msg,
                                role: Role::Assistant,
                                agent: None,
                                model: None,
                            },
                            Event::ReasoningStart {
                                session: a,
                                message: msg,
                                part,
                                reason: None,
                            },
                            Event::ReasoningDelta {
                                session: a,
                                message: msg,
                                part,
                                delta: "thought".into(),
                            },
                            Event::ReasoningEnd {
                                session: a,
                                message: msg,
                                part,
                                provider_data: None,
                            },
                        ],
                    )
                    .await
                    .unwrap();
            }
        }
        let recovered = store.recover_claim(a, owner).await.unwrap();
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
                ResidentWorkPrompt::Text("next".into()),
                "working".into(),
            )
            .await
            .unwrap();
        let msgs = &store.read_projection(a).await.unwrap().session.messages;
        assert_eq!(msgs[0].id, prompt);
        match state {
            State::PartialText => assert_eq!(msgs.len(), 3),
            State::ReasoningOnly => {
                let roles: Vec<_> =
                    msgs.iter()
                        .filter(|m| {
                            m.role != Role::Assistant || m.parts.iter().any(|p| {
                                matches!(p, PartProjection::Text { text, .. } if !text.is_empty())
                                    || matches!(p, PartProjection::Tool { .. })
                            })
                        })
                        .map(|m| m.role)
                        .collect();
                assert_eq!(roles, [Role::User, Role::Assistant, Role::User]);
            }
            State::None | State::EmptyText => {
                let closing = msgs[1..]
                    .iter()
                    .find(|m| {
                        m.parts.iter().any(
                            |p| matches!(p, PartProjection::Text { text, .. } if !text.is_empty()),
                        )
                    })
                    .unwrap();
                assert_eq!(
                    (closing.role, closing.finish, closing.cause),
                    (
                        Role::Assistant,
                        Some(FinishReason::Cancelled),
                        Some(hya_proto::FinishCause::Interrupted)
                    )
                );
                assert_eq!(msgs.last().unwrap().role, Role::User);
            }
        }
    }

    // 3. Outgoing DM copy does not queue a wake when idle or after interrupted work.
    for interrupted in [false, true] {
        let store = SessionStore::connect_memory().await.unwrap();
        let owner = OwnerRunId::new();
        store.claim_runtime_owner(owner).unwrap();
        let (root, claim) = work_fixture(&store).await;
        if interrupted {
            store
                .begin_work(
                    Some(&claim),
                    root,
                    "main/worker",
                    2,
                    ResidentWorkPrompt::Text("in".into()),
                    "working".into(),
                )
                .await
                .unwrap();
        } else {
            store
                .append_event(root, &ev_consumed(root, "main/worker", 2))
                .await
                .unwrap();
        }
        store
            .append_channel_mail(
                root,
                "main/worker".into(),
                "DM-12345678".into(),
                MailKind::Message,
                "out".into(),
                Some(&claim),
                None,
            )
            .await
            .unwrap();
        let recovered = store.recover_claim(claim.actor_id, owner).await.unwrap();
        let result = store
            .recover_resident_actor(&recovered, root, "main/worker")
            .await
            .unwrap();
        assert_eq!(
            result.work,
            if interrupted {
                hya_store::RecoveredResidentWork::AbortedRunning {
                    inbox_cursor: 2,
                    queued_after: false,
                }
            } else {
                hya_store::RecoveredResidentWork::Idle
            }
        );
    }
}
