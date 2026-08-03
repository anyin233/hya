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
async fn startup_recovery_requeues_accepted_aborts_started_and_is_idempotent() {
    let store = SessionStore::connect_memory().await.unwrap();
    let accepted = claim(ToolCallId::new(), 12);
    let started = claim(ToolCallId::new(), 13);
    store.claim_admission(&accepted).await.unwrap();
    store.claim_admission(&started).await.unwrap();
    store
        .start_admission(started.operation_id, None)
        .await
        .unwrap();

    let recovered = store
        .recover_nonterminal_admissions("startup recovery")
        .await
        .unwrap();
    let repeated = store
        .recover_nonterminal_admissions("startup recovery")
        .await
        .unwrap();

    assert_eq!(recovered.len(), 2);
    assert!(repeated.is_empty());
    let accepted_record = recovered
        .iter()
        .find(|record| record.operation_id == accepted.operation_id)
        .unwrap();
    let started_record = recovered
        .iter()
        .find(|record| record.operation_id == started.operation_id)
        .unwrap();
    assert_eq!(accepted_record.state, AdmissionState::Queued);
    assert!(!accepted_record.logical_released);
    assert_eq!(accepted_record.terminal_reason, None);
    assert_eq!(started_record.state, AdmissionState::Aborted);
    assert!(started_record.logical_released);
    assert_eq!(
        started_record.terminal_reason.as_deref(),
        Some("startup recovery")
    );
    assert!(
        store
            .replay(accepted.root_session)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(store.replay(started.root_session).await.unwrap().is_empty());
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
async fn admission_counts_are_derived_from_nonterminal_row_states() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let claims = [
        claim(ToolCallId::new(), 18),
        claim(ToolCallId::new(), 19),
        claim(ToolCallId::new(), 20),
        claim(ToolCallId::new(), 21),
        claim(ToolCallId::new(), 22),
        claim(ToolCallId::new(), 23),
        claim(ToolCallId::new(), 24),
    ];
    for admission in &claims {
        store.claim_admission(admission).await.unwrap();
    }

    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", temp_db.path()))
        .await
        .unwrap();
    let mut transaction = connection.begin().await.unwrap();
    for (admission, state) in [
        (&claims[0], "queued"),
        (&claims[1], "accepted"),
        (&claims[2], "started"),
        (&claims[3], "waiting"),
        (&claims[4], "completed"),
        (&claims[5], "cancelled"),
        (&claims[6], "aborted"),
    ] {
        sqlx::query("UPDATE admission_journal SET state = ? WHERE operation_id = ?")
            .bind(state)
            .bind(admission.operation_id.as_uuid().as_bytes().as_slice())
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 2);
    assert_eq!(counts.non_active, 2);
    assert_eq!(counts.total, 4);
}

#[tokio::test]
async fn batch_members_are_one_durable_envelope_each() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let admission = claim(ToolCallId::new(), 25);
    store.claim_admission(&admission).await.unwrap();

    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", temp_db.path()))
        .await
        .unwrap();
    let mut transaction = connection.begin().await.unwrap();
    sqlx::query(
        "UPDATE admission_journal \
         SET member_ordinal = 0, batch_size = 3, admission_units = 1 \
         WHERE operation_id = ?",
    )
    .bind(admission.operation_id.as_uuid().as_bytes().as_slice())
    .execute(&mut *transaction)
    .await
    .unwrap();
    for member_ordinal in [1, 2] {
        sqlx::query(
            "INSERT INTO admission_journal (\
                operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                state, admission_units, logical_released, terminal_reason, created_at, \
                updated_at, actor_id, actor_epoch, member_ordinal, batch_size\
             ) \
             SELECT operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                    state, 1, logical_released, terminal_reason, created_at, updated_at, \
                    actor_id, actor_epoch, ?, batch_size \
             FROM admission_journal WHERE operation_id = ? AND member_ordinal = 0",
        )
        .bind(member_ordinal)
        .bind(admission.operation_id.as_uuid().as_bytes().as_slice())
        .execute(&mut *transaction)
        .await
        .unwrap();
    }
    transaction.commit().await.unwrap();

    let records = store.admissions(admission.operation_id).await.unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(
        records
            .iter()
            .map(|record| record.member_ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(records.iter().all(|record| {
        record.batch_size == 3
            && record.admission_units == 1
            && record.state == AdmissionState::Accepted
    }));

    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 3);
    assert_eq!(counts.non_active, 0);
    assert_eq!(counts.total, 3);
}

#[tokio::test]
async fn atomic_capacity_commits_100_active_156_non_active_and_rejects_item_257() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let mut batch = claim(ToolCallId::new(), 26);
    batch.admission_units = 256;

    let members = store.claim_admission_batch(&batch).await.unwrap();
    assert_eq!(members.len(), 256);
    assert_eq!(
        members
            .iter()
            .map(|record| record.member_ordinal)
            .collect::<Vec<_>>(),
        (0_u32..256).collect::<Vec<_>>()
    );
    assert!(
        members
            .iter()
            .all(|record| { record.batch_size == 256 && record.admission_units == 1 })
    );
    assert!(
        members
            .iter()
            .take(100)
            .all(|record| record.state == AdmissionState::Accepted)
    );
    assert!(
        members
            .iter()
            .skip(100)
            .all(|record| record.state == AdmissionState::Queued)
    );

    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 156);
    assert_eq!(counts.total, 256);

    let mut rejected = claim(ToolCallId::new(), 27);
    rejected.admission_units = 1;
    assert!(matches!(
        store.claim_admission_batch(&rejected).await,
        Err(StoreError::AdmissionCapacityExceeded { .. })
    ));
    assert!(
        store
            .admissions(rejected.operation_id)
            .await
            .unwrap()
            .is_empty()
    );
    let counts_after_rejection = store.admission_counts().await.unwrap();
    assert_eq!(counts_after_rejection.active, 100);
    assert_eq!(counts_after_rejection.non_active, 156);
    assert_eq!(counts_after_rejection.total, 256);
}

#[tokio::test]
async fn full_capacity_batch_retry_is_idempotent() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let mut batch = claim(ToolCallId::new(), 28);
    batch.admission_units = 256;

    let first = store.claim_admission_batch(&batch).await.unwrap();
    let retry = store.claim_admission_batch(&batch).await.unwrap();
    assert_eq!(retry, first);
    assert_eq!(store.admissions(batch.operation_id).await.unwrap(), first);

    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 156);
    assert_eq!(counts.total, 256);
}

#[tokio::test]
async fn batch_crossing_non_active_capacity_rolls_back_every_member() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let mut first = claim(ToolCallId::new(), 29);
    first.admission_units = 255;
    store.claim_admission_batch(&first).await.unwrap();

    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 155);
    assert_eq!(counts.total, 255);

    let mut second = claim(ToolCallId::new(), 30);
    second.admission_units = 2;
    assert!(matches!(
        store.claim_admission_batch(&second).await,
        Err(StoreError::AdmissionCapacityExceeded { .. })
    ));
    assert!(
        store
            .admissions(second.operation_id)
            .await
            .unwrap()
            .is_empty()
    );

    let counts_after_rejection = store.admission_counts().await.unwrap();
    assert_eq!(counts_after_rejection.active, 100);
    assert_eq!(counts_after_rejection.non_active, 155);
    assert_eq!(counts_after_rejection.total, 255);
}

#[tokio::test]
async fn batch_source_tool_call_cannot_be_rebound_to_another_operation() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let source_tool_call_id = ToolCallId::new();
    let mut first = claim(source_tool_call_id, 31);
    first.admission_units = 2;
    let first_records = store.claim_admission_batch(&first).await.unwrap();
    let first_counts = store.admission_counts().await.unwrap();

    let mut second = first.clone();
    second.operation_id = OperationId::from_storage_uuid(uuid::Uuid::now_v7());
    second.request_fingerprint = [32; 32];
    assert!(matches!(
        store.claim_admission_batch(&second).await,
        Err(StoreError::OperationIdConflict { operation_id })
            if operation_id == second.operation_id
    ));
    assert!(
        store
            .admissions(second.operation_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.admissions(first.operation_id).await.unwrap(),
        first_records
    );
    assert_eq!(store.admission_counts().await.unwrap(), first_counts);
    assert_eq!(first_counts.active, 2);
    assert_eq!(first_counts.non_active, 0);
    assert_eq!(first_counts.total, 2);
}
