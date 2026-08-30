//! File-backed recovery of durable Workflow runs.

#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{
    AgentName, Event, OwnerRunId, SessionId, WorkflowIdentity, WorkflowRevision, WorkflowRunId,
    WorkflowRunStatus, WorkflowSourceId, WorkflowStagePlan,
};
use hya_store::{SessionStore, StoreError, WorkflowAdmissionOutcome, WorkflowSelectionOutcome};

/// Return one process-unique SQLite path.
fn database_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "hya-workflow-recovery-{}-{nonce}.db",
        std::process::id()
    ))
}

/// Delete the database and SQLite sidecar files after a test.
fn remove_database(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    let _ = std::fs::remove_file(format!("{}.runtime-owner.lock", path.display()));
}

/// Build one stable Workflow identity for a recovery fixture.
fn workflow_identity(name: &str) -> WorkflowIdentity {
    WorkflowIdentity {
        source: WorkflowSourceId::new(format!("project:{name}")),
        name: name.to_string(),
        revision: WorkflowRevision::from_bytes([1; 32]),
    }
}

/// Build one declaration-ordered Stage plan for a recovery fixture.
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

/// Startup recovery terminalizes a persisted running run exactly once and never
/// replays Stage side effects.
#[tokio::test]
async fn reopen_then_recovery_marks_running_workflow_interrupted_once() {
    let path = database_path();
    let session = SessionId::new();
    let run = WorkflowRunId::new();
    let old_owner = OwnerRunId::new();
    let current_owner = OwnerRunId::new();
    assert_ne!(old_owner, current_owner);

    {
        let store = SessionStore::connect(path.to_str().expect("UTF-8 path"))
            .await
            .expect("create store");
        store
            .append_event(
                session,
                &Event::WorkflowRunStarted {
                    session,
                    run,
                    workflow: workflow_identity("deliver"),
                    request_hash: "inputs".to_string(),
                    owner: old_owner,
                    stages: vec![workflow_stage()],
                },
            )
            .await
            .expect("append run start");
    }

    let store = SessionStore::connect(path.to_str().expect("UTF-8 path"))
        .await
        .expect("reopen store");
    assert_eq!(
        store
            .read_projection(session)
            .await
            .expect("projection before recovery")
            .session
            .workflow
            .expect("Workflow projection")
            .run
            .expect("running run")
            .status,
        WorkflowRunStatus::Running
    );

    store
        .claim_runtime_owner(current_owner)
        .expect("claim runtime owner before recovery");
    let recovered = store
        .recover_nonterminal_workflows(current_owner, "backend owner exited")
        .await
        .expect("recover running Workflows");
    let repeated = store
        .recover_nonterminal_workflows(current_owner, "backend owner exited")
        .await
        .expect("repeat recovery");

    assert_eq!(recovered.len(), 1);
    assert!(repeated.is_empty());
    assert!(matches!(
        recovered[0].event,
        Event::WorkflowRunFinished {
            run: event_run,
            status: WorkflowRunStatus::Interrupted,
            ..
        } if event_run == run
    ));
    let events = store.replay(session).await.expect("replay recovered log");
    assert_eq!(events.len(), 2, "recovery adds only one terminal event");
    assert!(!events.iter().any(|envelope| matches!(
        envelope.event,
        Event::WorkflowStageStarted { .. } | Event::WorkflowStageMemberLinked { .. }
    )));
    let current = store
        .read_projection(session)
        .await
        .expect("projection after recovery")
        .session
        .workflow
        .expect("Workflow projection")
        .run
        .expect("recovered run");
    assert_eq!(current.status, WorkflowRunStatus::Interrupted);
    assert_eq!(current.error.as_deref(), Some("backend owner exited"));

    drop(store);
    remove_database(&path);
}

/// Recovery leaves already-terminal Workflow runs unchanged.
#[tokio::test]
async fn recovery_does_not_reterminalize_completed_workflow() {
    let store = SessionStore::connect_memory().await.expect("memory store");
    let session = SessionId::new();
    let owner = OwnerRunId::new();
    let run = WorkflowRunId::new();
    store
        .append_event(
            session,
            &Event::WorkflowRunStarted {
                session,
                run,
                workflow: workflow_identity("done"),
                request_hash: "inputs".to_string(),
                owner,
                stages: vec![workflow_stage()],
            },
        )
        .await
        .expect("append start");
    store
        .append_event(
            session,
            &Event::WorkflowRunFinished {
                session,
                run,
                status: WorkflowRunStatus::Completed,
                error: None,
            },
        )
        .await
        .expect("append finish");

    store
        .claim_runtime_owner(owner)
        .expect("claim runtime owner before terminal recovery");
    let recovered = store
        .recover_nonterminal_workflows(owner, "backend owner exited")
        .await
        .expect("recover terminal Workflows");

    assert!(recovered.is_empty());
    assert_eq!(store.replay(session).await.expect("replay").len(), 2);
}

/// Startup recovery never interrupts a run owned by the current runtime.
#[tokio::test]
async fn recovery_leaves_current_owner_running() {
    let store = SessionStore::connect_memory().await.expect("memory store");
    let session = SessionId::new();
    let run = WorkflowRunId::new();
    let owner = OwnerRunId::new();
    store
        .append_event(
            session,
            &Event::WorkflowRunStarted {
                session,
                run,
                workflow: workflow_identity("live"),
                request_hash: "inputs".to_string(),
                owner,
                stages: vec![workflow_stage()],
            },
        )
        .await
        .expect("append start");

    store
        .claim_runtime_owner(owner)
        .expect("claim runtime owner before current-owner recovery");
    let recovered = store
        .recover_nonterminal_workflows(owner, "backend owner exited")
        .await
        .expect("recover current-owner Workflows");

    assert!(recovered.is_empty());
    let current = store
        .read_projection(session)
        .await
        .expect("projection")
        .session
        .workflow
        .expect("Workflow projection")
        .run
        .expect("run");
    assert_eq!(current.status, WorkflowRunStatus::Running);
}

/// Closing and reopening the store preserves replay-derived Workflow selection.
#[tokio::test]
async fn reopen_preserves_workflow_projection() {
    let path = database_path();
    let session = SessionId::new();
    let selected = workflow_identity("persisted");

    let before = {
        let store = SessionStore::connect(path.to_str().expect("UTF-8 path"))
            .await
            .expect("create store");
        store
            .append_event(
                session,
                &Event::WorkflowSelected {
                    session,
                    workflow: selected,
                },
            )
            .await
            .expect("append selection");
        store
            .read_projection(session)
            .await
            .expect("projection before reopen")
            .session
            .workflow
    };
    assert_eq!(
        before
            .as_ref()
            .and_then(|workflow| workflow.selection.as_ref())
            .map(|workflow| workflow.name.as_str()),
        Some("persisted")
    );

    let store = SessionStore::connect(path.to_str().expect("UTF-8 path"))
        .await
        .expect("reopen store");
    let after = store
        .read_projection(session)
        .await
        .expect("projection after reopen")
        .session
        .workflow;

    assert_eq!(after, before);
    drop(store);
    remove_database(&path);
}

/// Concurrent stores cannot duplicate a run start, change its immutable
/// request, or switch selection while that run remains active.
#[tokio::test]
async fn workflow_admission_is_atomic_across_store_handles() {
    let path = database_path();
    let store = SessionStore::connect(path.to_str().expect("UTF-8 path"))
        .await
        .expect("left store");
    let other = SessionStore::connect(path.to_str().expect("UTF-8 path"))
        .await
        .expect("right store");
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
    let left_store = store.clone();
    let right_store = other.clone();
    let (left, right) = tokio::join!(
        left_store.admit_workflow_run(None, session, start.clone()),
        right_store.admit_workflow_run(None, session, start.clone()),
    );
    let outcomes = [
        left.expect("left admission"),
        right.expect("right admission"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, WorkflowAdmissionOutcome::Admitted(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, WorkflowAdmissionOutcome::Existing))
            .count(),
        1
    );

    let conflict = store
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
        .expect("conflicting admission outcome");
    assert_eq!(conflict, WorkflowAdmissionOutcome::Conflict);
    let selection = store
        .select_workflow(
            None,
            session,
            Event::WorkflowSelected {
                session,
                workflow: workflow_identity("other"),
            },
        )
        .await
        .expect("busy selection outcome");
    assert_eq!(selection, WorkflowSelectionOutcome::Busy { run });
    assert_eq!(store.replay(session).await.expect("replay").len(), 1);
    drop(left_store);
    drop(right_store);
    drop(other);
    drop(store);
    remove_database(&path);
}

/// Run admission validates the complete actor capability in the same writer
/// transaction as the start event.
#[tokio::test]
async fn workflow_admission_rejects_stale_actor_claim_without_event() {
    let store = SessionStore::connect_memory().await.expect("memory store");
    let actor = SessionId::new();
    let session = SessionId::new();
    let stale = store
        .try_claim_new(actor, OwnerRunId::new())
        .await
        .expect("initial actor claim");
    let _current = store
        .recover_claim(actor, OwnerRunId::new())
        .await
        .expect("fence initial claim");
    let result = store
        .admit_workflow_run(
            Some(&stale),
            session,
            Event::WorkflowRunStarted {
                session,
                run: WorkflowRunId::new(),
                workflow: workflow_identity("fenced"),
                request_hash: "request".to_string(),
                owner: stale.owner_run_id,
                stages: vec![workflow_stage()],
            },
        )
        .await;
    assert!(matches!(result, Err(StoreError::StaleActorClaim { .. })));
    assert!(store.replay(session).await.expect("replay").is_empty());
}

/// Recovery is rejected unless this store holds the matching runtime owner claim.
#[tokio::test]
async fn recovery_requires_runtime_owner_claim() {
    let store = SessionStore::connect_memory().await.expect("memory store");
    let error = store
        .recover_nonterminal_workflows(OwnerRunId::new(), "backend owner exited")
        .await
        .expect_err("claim-less recovery must fail closed");
    assert!(matches!(error, StoreError::RuntimeOwnerClaimRequired));
}

/// File-backed claims are clone-shared, process-exclusive, and released on drop.
#[tokio::test]
async fn runtime_owner_claim_is_exclusive_and_releases_after_holder_drop() {
    let path = database_path();
    let session = SessionId::new();
    let run = WorkflowRunId::new();
    let previous_owner = OwnerRunId::new();
    let owner = OwnerRunId::new();
    let first = SessionStore::connect(path.to_str().expect("UTF-8 path"))
        .await
        .expect("first store");
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
        .expect("append running workflow");
    let first_clone = first.clone();
    let second = SessionStore::connect(path.to_str().expect("UTF-8 path"))
        .await
        .expect("second store");

    first
        .claim_runtime_owner(owner)
        .expect("first store claims runtime owner");
    first_clone
        .claim_runtime_owner(owner)
        .expect("matching clone claim is idempotent");
    assert!(matches!(
        second.claim_runtime_owner(OwnerRunId::new()),
        Err(StoreError::RuntimeOwnerBusy)
    ));

    drop(first_clone);
    drop(first);
    second
        .claim_runtime_owner(owner)
        .expect("second store claims after holder drop");
    let recovered = second
        .recover_nonterminal_workflows(owner, "backend owner exited")
        .await
        .expect("recover running workflow");
    let repeated = second
        .recover_nonterminal_workflows(owner, "backend owner exited")
        .await
        .expect("repeat recovery");
    assert_eq!(recovered.len(), 1);
    assert!(repeated.is_empty());
    assert!(matches!(
        recovered[0].event,
        Event::WorkflowRunFinished {
            run: recovered_run,
            status: WorkflowRunStatus::Interrupted,
            ..
        } if recovered_run == run
    ));
    drop(second);
    remove_database(&path);
}

/// Runtime ownership never follows an attacker-controlled adjacent symlink.
#[cfg(unix)]
#[tokio::test]
async fn runtime_owner_claim_rejects_symlink_lock_path() {
    use std::os::unix::fs::symlink;

    let path = database_path();
    let store = SessionStore::connect(path.to_str().expect("UTF-8 path"))
        .await
        .expect("store");
    let victim = path.with_extension("victim");
    std::fs::write(&victim, "unchanged").expect("victim fixture");
    let lock_path = PathBuf::from(format!("{}.runtime-owner.lock", path.display()));
    symlink(&victim, &lock_path).expect("lock symlink fixture");

    let error = store
        .claim_runtime_owner(OwnerRunId::new())
        .expect_err("symlink lock must fail closed");
    assert!(matches!(error, StoreError::RuntimeOwnerLock { .. }));
    assert_eq!(
        std::fs::read_to_string(&victim).expect("read victim"),
        "unchanged"
    );

    drop(store);
    let _ = std::fs::remove_file(&lock_path);
    let _ = std::fs::remove_file(&victim);
    remove_database(&path);
}
