//! File-backed recovery, atomic admission, and runtime ownership of durable Workflow runs.

#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{
    AgentName, Event, OwnerRunId, SessionId, WorkflowIdentity, WorkflowRevision, WorkflowRunId,
    WorkflowRunStatus, WorkflowSourceId, WorkflowStagePlan,
};
use hya_store::{SessionStore, StoreError, WorkflowAdmissionOutcome, WorkflowSelectionOutcome};

struct TempDb(PathBuf);

impl TempDb {
    fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let serial = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "hya-workflow-recovery-{}-{nonce}-{serial}.db",
            std::process::id()
        )))
    }

    fn path_str(&self) -> &str {
        self.0.to_str().expect("UTF-8 path")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        for suffix in ["-wal", "-shm", ".runtime-owner.lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

fn workflow_identity(name: &str) -> WorkflowIdentity {
    WorkflowIdentity {
        source: WorkflowSourceId::new(format!("project:{name}")),
        name: name.to_string(),
        revision: WorkflowRevision::from_bytes([1; 32]),
    }
}

fn workflow_stage() -> WorkflowStagePlan {
    WorkflowStagePlan {
        id: "stage".to_string(),
        title: None,
        agent: AgentName::new("general"),
        mode: "once".to_string(),
        level: 0,
        worker_model: None,
        selected_worker_model: None,
        verifier_model: None,
        selected_verifier_model: None,
    }
}

/// Reopen + startup recovery finds running runs among idle sessions, interrupts stale-owner
/// runs exactly once without replaying stage side effects, and preserves workflow selections.
#[tokio::test]
async fn reopen_and_recovery_interrupts_stale_runs_once_among_idle_sessions() {
    let db = TempDb::new();
    let running = SessionId::new();
    let selected_session = SessionId::new();
    let run = WorkflowRunId::new();
    let old_owner = OwnerRunId::new();
    let current_owner = OwnerRunId::new();

    let selection_before = {
        let store = SessionStore::connect(db.path_str()).await.expect("create");
        for _ in 0..12 {
            let idle = SessionId::new();
            store
                .append_event(
                    idle,
                    &Event::SessionCreated {
                        session: idle,
                        parent: None,
                        agent: "general".into(),
                        model: "fake".into(),
                        workdir: "/tmp".into(),
                        project: None,
                        kind: hya_proto::SessionKind::Project,
                    },
                )
                .await
                .expect("idle session");
        }
        store
            .append_event(
                running,
                &Event::WorkflowRunStarted {
                    session: running,
                    run,
                    workflow: workflow_identity("deliver"),
                    request_hash: "inputs".to_string(),
                    owner: old_owner,
                    stages: vec![workflow_stage()],
                },
            )
            .await
            .expect("append run start");
        store
            .append_event(
                selected_session,
                &Event::WorkflowSelected {
                    session: selected_session,
                    workflow: workflow_identity("persisted"),
                },
            )
            .await
            .expect("append selection");
        store
            .read_projection(selected_session)
            .await
            .expect("proj")
            .session
            .workflow
    };

    let store = SessionStore::connect(db.path_str()).await.expect("reopen");
    assert_eq!(
        store
            .read_projection(selected_session)
            .await
            .expect("proj")
            .session
            .workflow,
        selection_before
    );
    assert_eq!(
        store
            .read_projection(running)
            .await
            .expect("proj")
            .session
            .workflow
            .expect("wf")
            .run
            .expect("run")
            .status,
        WorkflowRunStatus::Running
    );

    store.claim_runtime_owner(current_owner).expect("claim");
    let recovered = store
        .recover_nonterminal_workflows(current_owner, "backend owner exited")
        .await
        .expect("recover");
    let repeated = store
        .recover_nonterminal_workflows(current_owner, "backend owner exited")
        .await
        .expect("repeat");

    assert_eq!(recovered.len(), 1);
    assert!(repeated.is_empty());
    assert!(matches!(
        recovered[0].event,
        Event::WorkflowRunFinished { run: r, status: WorkflowRunStatus::Interrupted, .. } if r == run
    ));
    let events = store.replay(running).await.expect("replay");
    assert_eq!(events.len(), 2);
    assert!(!events.iter().any(|e| matches!(
        e.event,
        Event::WorkflowStageStarted { .. } | Event::WorkflowStageMemberLinked { .. }
    )));
    let after = store
        .read_projection(running)
        .await
        .expect("proj")
        .session
        .workflow
        .expect("wf")
        .run
        .expect("run");
    assert_eq!(after.status, WorkflowRunStatus::Interrupted);
    assert_eq!(after.error.as_deref(), Some("backend owner exited"));
}

/// Recovery requires the runtime owner claim, skips terminal runs, and skips current-owner runs.
#[tokio::test]
async fn recovery_guards_owner_claim_terminal_runs_and_live_owner_runs() {
    let store = SessionStore::connect_memory().await.expect("memory");
    let error = store
        .recover_nonterminal_workflows(OwnerRunId::new(), "backend owner exited")
        .await
        .expect_err("claim-less recovery must fail closed");
    assert!(matches!(error, StoreError::RuntimeOwnerClaimRequired));

    let owner = OwnerRunId::new();
    let done_session = SessionId::new();
    let done_run = WorkflowRunId::new();
    let live_session = SessionId::new();
    let live_run = WorkflowRunId::new();

    store
        .append_event(
            done_session,
            &Event::WorkflowRunStarted {
                session: done_session,
                run: done_run,
                workflow: workflow_identity("done"),
                request_hash: "inputs".to_string(),
                owner,
                stages: vec![workflow_stage()],
            },
        )
        .await
        .expect("start done");
    store
        .append_event(
            done_session,
            &Event::WorkflowRunFinished {
                session: done_session,
                run: done_run,
                status: WorkflowRunStatus::Completed,
                error: None,
            },
        )
        .await
        .expect("finish done");
    store
        .append_event(
            live_session,
            &Event::WorkflowRunStarted {
                session: live_session,
                run: live_run,
                workflow: workflow_identity("live"),
                request_hash: "inputs".to_string(),
                owner,
                stages: vec![workflow_stage()],
            },
        )
        .await
        .expect("start live");

    store.claim_runtime_owner(owner).expect("claim");
    assert!(
        store
            .recover_nonterminal_workflows(owner, "backend owner exited")
            .await
            .expect("recover")
            .is_empty()
    );
    assert_eq!(store.replay(done_session).await.expect("replay").len(), 2);
    assert_eq!(
        store
            .read_projection(live_session)
            .await
            .expect("proj")
            .session
            .workflow
            .expect("wf")
            .run
            .expect("run")
            .status,
        WorkflowRunStatus::Running
    );
}

#[tokio::test]
async fn workflow_admission_is_atomic_and_rejects_conflicts_and_stale_claims() {
    let db = TempDb::new();
    let store = SessionStore::connect(db.path_str()).await.expect("left");
    let other = SessionStore::connect(db.path_str()).await.expect("right");
    let session = SessionId::new();
    let run = WorkflowRunId::new();
    let owner = OwnerRunId::new();
    let start = Event::WorkflowRunStarted {
        session,
        run,
        workflow: workflow_identity("atomic"),
        request_hash: "request-a".to_string(),
        owner,
        stages: vec![workflow_stage()],
    };
    let (left, right) = tokio::join!(
        store.admit_workflow_run(None, session, start.clone()),
        other.admit_workflow_run(None, session, start.clone()),
    );
    let outcomes = [left.expect("left"), right.expect("right")];
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, WorkflowAdmissionOutcome::Admitted(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, WorkflowAdmissionOutcome::Existing))
            .count(),
        1
    );

    assert_eq!(
        store
            .admit_workflow_run(
                None,
                session,
                Event::WorkflowRunStarted {
                    session,
                    run,
                    workflow: workflow_identity("atomic"),
                    request_hash: "request-b".to_string(),
                    owner,
                    stages: vec![workflow_stage()],
                },
            )
            .await
            .expect("conflict"),
        WorkflowAdmissionOutcome::Conflict
    );
    assert_eq!(
        store
            .select_workflow(
                None,
                session,
                Event::WorkflowSelected {
                    session,
                    workflow: workflow_identity("other"),
                },
            )
            .await
            .expect("busy"),
        WorkflowSelectionOutcome::Busy { run }
    );
    assert_eq!(store.replay(session).await.expect("replay").len(), 1);

    // Stale actor claim is rejected atomically without appending an event.
    let mem = SessionStore::connect_memory().await.expect("mem");
    let recovery_owner = OwnerRunId::new();
    mem.claim_runtime_owner(recovery_owner).expect("claim");
    let actor = SessionId::new();
    let fenced_session = SessionId::new();
    let stale = mem
        .try_claim_new(fenced_session, actor, OwnerRunId::new())
        .await
        .expect("claim");
    let _ = mem
        .recover_claim(actor, recovery_owner)
        .await
        .expect("fence");
    let err = mem
        .admit_workflow_run(
            Some(&stale),
            fenced_session,
            Event::WorkflowRunStarted {
                session: fenced_session,
                run: WorkflowRunId::new(),
                workflow: workflow_identity("fenced"),
                request_hash: "request".to_string(),
                owner: stale.owner_run_id,
                stages: vec![workflow_stage()],
            },
        )
        .await;
    assert!(matches!(err, Err(StoreError::StaleActorClaim { .. })));
    assert!(mem.replay(fenced_session).await.expect("replay").is_empty());
}

#[tokio::test]
async fn runtime_owner_lock_metadata_exclusivity_and_release_on_drop() {
    let db = TempDb::new();
    let session = SessionId::new();
    let run = WorkflowRunId::new();
    let previous_owner = OwnerRunId::new();
    let owner = OwnerRunId::new();

    let first = SessionStore::connect_runtime(db.path_str())
        .await
        .expect("first");
    let metadata = PathBuf::from(format!("{}.runtime-owner.lock", db.0.display()));
    let pid = format!("{}\n", std::process::id());
    assert_eq!(std::fs::read_to_string(&metadata).expect("meta"), pid);

    first
        .append_event(
            session,
            &Event::WorkflowRunStarted {
                session,
                run,
                workflow: workflow_identity("owner-claim"),
                request_hash: "inputs".to_string(),
                owner: previous_owner,
                stages: vec![workflow_stage()],
            },
        )
        .await
        .expect("start");
    let first_clone = first.clone();
    let second = SessionStore::connect(db.path_str()).await.expect("second");

    assert!(matches!(
        second.claim_runtime_owner(OwnerRunId::new()),
        Err(StoreError::RuntimeOwnerBusy)
    ));
    first.claim_runtime_owner(owner).expect("claim");
    first_clone
        .claim_runtime_owner(owner)
        .expect("clone idempotent");
    assert!(matches!(
        second.claim_runtime_owner(OwnerRunId::new()),
        Err(StoreError::RuntimeOwnerBusy)
    ));
    assert_eq!(std::fs::read_to_string(&metadata).expect("meta"), pid);

    drop(first_clone);
    drop(first);
    assert_eq!(std::fs::read_to_string(&metadata).expect("released"), "");

    second.claim_runtime_owner(owner).expect("takeover");
    let recovered = second
        .recover_nonterminal_workflows(owner, "backend owner exited")
        .await
        .expect("recover");
    assert_eq!(recovered.len(), 1);
    assert!(
        second
            .recover_nonterminal_workflows(owner, "backend owner exited")
            .await
            .expect("repeat")
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_owner_claim_rejects_symlink_lock_path() {
    use std::os::unix::fs::symlink;

    let db = TempDb::new();
    let store = SessionStore::connect(db.path_str()).await.expect("store");
    let victim = db.0.with_extension("victim");
    std::fs::write(&victim, "unchanged").expect("victim");
    let lock_path = PathBuf::from(format!("{}.runtime-owner.lock", db.0.display()));
    symlink(&victim, &lock_path).expect("symlink");

    let error = store
        .claim_runtime_owner(OwnerRunId::new())
        .expect_err("symlink lock must fail closed");
    assert!(matches!(error, StoreError::RuntimeOwnerLock { .. }));
    assert_eq!(
        std::fs::read_to_string(&victim).expect("victim"),
        "unchanged"
    );

    drop(store);
    let _ = std::fs::remove_file(&lock_path);
    let _ = std::fs::remove_file(&victim);
}
