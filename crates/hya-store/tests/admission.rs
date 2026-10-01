//! Spawn-admission claim, refund, and terminal-state transitions.

#![allow(clippy::unwrap_used)]

use hya_proto::{OperationId, OwnerRunId, SessionId, ToolCallId};
use hya_store::{
    AdmissionClaim, AdmissionClaimOutcome, AdmissionStartOutcome, AdmissionState,
    AdmissionTerminal, SessionStore, StoreError,
};
use sqlx::{Connection, SqliteConnection};

struct AdmissionTempDb {
    path: String,
}

impl AdmissionTempDb {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .join(format!("hya-admission-{}.db", SessionId::new()))
            .to_string_lossy()
            .into_owned();
        Self { path }
    }

    fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for AdmissionTempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path));
        }
    }
}

fn claim(source_tool_call_id: ToolCallId, fingerprint: u8) -> AdmissionClaim {
    AdmissionClaim {
        operation_id: OperationId::from_tool_call(source_tool_call_id),
        source_tool_call_id,
        root_session: SessionId::new(),
        request_fingerprint: [fingerprint; 32],
        admission_units: 2,
        actor_claim: None,
    }
}

#[tokio::test]
async fn concurrent_start_has_exactly_one_dispatch_winner() {
    let store = SessionStore::connect_memory().await.unwrap();
    let admission = claim(ToolCallId::new(), 9);
    store.claim_admission(&admission).await.unwrap();

    let (left, right) = tokio::join!(
        store.start_admission(admission.operation_id, None),
        store.start_admission(admission.operation_id, None)
    );
    let outcomes = [left.unwrap(), right.unwrap()];

    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, AdmissionStartOutcome::Started(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                AdmissionStartOutcome::Existing(record)
                    if record.state == AdmissionState::Started
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn terminal_transition_is_immutable_idempotent_and_releases_only_started() {
    let store = SessionStore::connect_memory().await.unwrap();

    let accepted = claim(ToolCallId::new(), 10);
    store.claim_admission(&accepted).await.unwrap();
    let aborted = store
        .finalize_admission(
            accepted.operation_id,
            AdmissionTerminal::Aborted,
            "overloaded",
            None,
        )
        .await
        .unwrap();
    let aborted_again = store
        .finalize_admission(
            accepted.operation_id,
            AdmissionTerminal::Aborted,
            "overloaded",
            None,
        )
        .await
        .unwrap();
    let conflict = store
        .finalize_admission(
            accepted.operation_id,
            AdmissionTerminal::Cancelled,
            "different terminal",
            None,
        )
        .await
        .unwrap_err();

    assert_eq!(aborted.record.state, AdmissionState::Aborted);
    assert!(!aborted.release_required);
    assert!(!aborted_again.release_required);
    assert!(matches!(
        conflict,
        StoreError::AdmissionTransitionConflict { operation_id, .. }
            if operation_id == accepted.operation_id
    ));

    let started = claim(ToolCallId::new(), 11);
    store.claim_admission(&started).await.unwrap();
    assert!(matches!(
        store
            .start_admission(started.operation_id, None)
            .await
            .unwrap(),
        AdmissionStartOutcome::Started(_)
    ));
    let completed = store
        .finalize_admission(
            started.operation_id,
            AdmissionTerminal::Completed,
            "completed",
            None,
        )
        .await
        .unwrap();
    let completed_again = store
        .finalize_admission(
            started.operation_id,
            AdmissionTerminal::Completed,
            "completed",
            None,
        )
        .await
        .unwrap();

    assert_eq!(completed.record.state, AdmissionState::Completed);
    assert!(completed.release_required);
    assert!(!completed_again.release_required);
    assert!(completed_again.record.logical_released);
}

#[tokio::test]
async fn startup_recovery_aborts_unbound_accepted_without_rebinding() {
    let store = SessionStore::connect_memory().await.unwrap();
    let accepted = claim(ToolCallId::new(), 34);
    store.claim_admission(&accepted).await.unwrap();

    let recovered = store
        .recover_nonterminal_admissions("startup recovery")
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    let recovered_record = recovered
        .into_iter()
        .find(|record| record.operation_id == accepted.operation_id)
        .unwrap();
    assert_eq!(recovered_record.state, AdmissionState::Aborted);
    assert!(!recovered_record.logical_released);
    assert_eq!(
        recovered_record.terminal_reason.as_deref(),
        Some("startup recovery")
    );

    let persisted = store
        .admission(accepted.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.admission_counts().await.unwrap(),
        hya_store::AdmissionCounts {
            active: 0,
            non_active: 0,
            total: 0,
        }
    );

    let repeated = store
        .recover_nonterminal_admissions("startup recovery")
        .await
        .unwrap();
    assert!(repeated.is_empty());
    let repeated_record = store
        .admission(accepted.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(repeated_record, persisted);
    assert_eq!(
        repeated_record.terminal_reason.as_deref(),
        Some("startup recovery")
    );
}

#[tokio::test]
async fn startup_recovery_leaves_actor_bound_operation_for_fenced_takeover() {
    let store = SessionStore::connect_memory().await.unwrap();
    let actor_id = SessionId::new();
    let old_claim = store
        .try_claim_new(actor_id, OwnerRunId::new())
        .await
        .unwrap();
    let mut admission = claim(ToolCallId::new(), 14);
    admission.actor_claim = Some(old_claim);
    store.claim_admission(&admission).await.unwrap();
    store
        .start_admission(admission.operation_id, Some(&old_claim))
        .await
        .unwrap();
    let recovered = store
        .recover_claim(actor_id, OwnerRunId::new())
        .await
        .unwrap();

    let global = store
        .recover_nonterminal_admissions("startup recovery")
        .await
        .unwrap();

    assert!(global.is_empty());
    assert_eq!(
        store
            .admission(admission.operation_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        AdmissionState::Started
    );
    let actor = store
        .abort_recovered_actor_admissions(&recovered, "resident actor takeover")
        .await
        .unwrap();
    assert_eq!(actor.len(), 1);
    assert_eq!(actor[0].state, AdmissionState::Aborted);
    assert!(actor[0].logical_released);
}

#[tokio::test]
async fn release_claim_aborts_bound_operation_before_releasing_actor() {
    let store = SessionStore::connect_memory().await.unwrap();
    let actor_id = SessionId::new();
    let actor_claim = store
        .try_claim_new(actor_id, OwnerRunId::new())
        .await
        .unwrap();
    let mut admission = claim(ToolCallId::new(), 15);
    admission.actor_claim = Some(actor_claim);
    store.claim_admission(&admission).await.unwrap();
    store
        .start_admission(admission.operation_id, Some(&actor_claim))
        .await
        .unwrap();

    store.release_claim(&actor_claim).await.unwrap();

    let record = store
        .admission(admission.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, AdmissionState::Aborted);
    assert!(record.logical_released);
    assert!(matches!(
        store.validate_actor_claim(&actor_claim).await,
        Err(StoreError::StaleActorClaim { actor_id: stale }) if stale == actor_id
    ));
}

#[tokio::test]
async fn claim_is_idempotent_and_conflicting_fingerprint_fails_closed() {
    let store = SessionStore::connect_memory().await.unwrap();
    let source = ToolCallId::new();
    let first = claim(source, 7);

    let inserted = store.claim_admission(&first).await.unwrap();
    let replayed = store.claim_admission(&first).await.unwrap();
    let conflict = store.claim_admission(&claim(source, 8)).await.unwrap_err();

    assert!(matches!(
        inserted,
        AdmissionClaimOutcome::Claimed(ref record)
            if record.state == AdmissionState::Accepted
    ));
    assert!(matches!(
        replayed,
        AdmissionClaimOutcome::Existing(ref record)
            if record.state == AdmissionState::Accepted
    ));
    assert!(matches!(
        conflict,
        StoreError::OperationIdConflict { operation_id }
            if operation_id == OperationId::from_tool_call(source)
    ));
    assert!(store.replay(first.root_session).await.unwrap().is_empty());
}

#[tokio::test]
async fn queued_and_waiting_states_round_trip_through_admission_journal() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let queued = claim(ToolCallId::new(), 16);
    let waiting = claim(ToolCallId::new(), 17);
    store.claim_admission(&queued).await.unwrap();
    store.claim_admission(&waiting).await.unwrap();

    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", temp_db.path()))
        .await
        .unwrap();
    let mut transaction = connection.begin().await.unwrap();
    for (admission, state) in [(&queued, "queued"), (&waiting, "waiting")] {
        sqlx::query("UPDATE admission_journal SET state = ? WHERE operation_id = ?")
            .bind(state)
            .bind(admission.operation_id.as_uuid().as_bytes().as_slice())
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    let queued_record = store.admission(queued.operation_id).await.unwrap().unwrap();
    let waiting_record = store
        .admission(waiting.operation_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(queued_record.state, AdmissionState::Queued);
    assert_eq!(waiting_record.state, AdmissionState::Waiting);
    assert!(!queued_record.state.is_terminal());
    assert!(!waiting_record.state.is_terminal());
}

#[tokio::test]
async fn startup_recovery_preserves_legacy_bound_rows_and_releases_started_once() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let accepted = claim(ToolCallId::new(), 12);
    let started = claim(ToolCallId::new(), 13);
    let waiting = claim(ToolCallId::new(), 14);
    for admission in [&accepted, &started, &waiting] {
        store.claim_admission(admission).await.unwrap();
    }
    store
        .start_admission(started.operation_id, None)
        .await
        .unwrap();
    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", temp_db.path()))
        .await
        .unwrap();
    sqlx::query("UPDATE admission_journal SET runtime_fingerprint_version = 1, runtime_fingerprint = ?, admission_binding_fingerprint_version = 1, admission_binding_fingerprint = ?, spawn_intent = ? WHERE operation_id = ?")
        .bind(vec![12_u8; 32]).bind(vec![12_u8; 32]).bind(vec![12_u8])
        .bind(accepted.operation_id.as_uuid().as_bytes().as_slice())
        .execute(&mut connection).await.unwrap();
    sqlx::query("UPDATE admission_journal SET state = 'waiting' WHERE operation_id = ?")
        .bind(waiting.operation_id.as_uuid().as_bytes().as_slice())
        .execute(&mut connection)
        .await
        .unwrap();
    store
        .recover_nonterminal_admissions("restart")
        .await
        .unwrap();
    let requeued = store
        .admission(accepted.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requeued.state, AdmissionState::Queued);
    assert_eq!(requeued.terminal_reason, None);
    assert!(!requeued.logical_released);
    let aborted = store
        .admission(started.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(aborted.state, AdmissionState::Aborted);
    assert!(aborted.logical_released);
    let parent = store
        .admission(waiting.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parent.state, AdmissionState::Aborted);
    assert!(!parent.logical_released);
    assert_eq!(
        store
            .terminal_released_operations_for_root(started.root_session)
            .await
            .unwrap(),
        vec![started.operation_id]
    );
    assert!(
        store
            .recover_nonterminal_admissions("repeat")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .admission(started.operation_id)
            .await
            .unwrap()
            .unwrap(),
        aborted
    );
}
