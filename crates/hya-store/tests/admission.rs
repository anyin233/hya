#![allow(clippy::unwrap_used)]

use hya_proto::{OperationId, OwnerRunId, SessionId, ToolCallId};
use hya_store::{
    AdmissionActorBinding, AdmissionBatchClaimOutcome, AdmissionClaim, AdmissionClaimOutcome,
    AdmissionIntent, AdmissionStartOutcome, AdmissionState, AdmissionTerminal, SessionStore,
    StoreError,
};
use sqlx::{Connection, Row, SqliteConnection};

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
    let mut accepted = claim(ToolCallId::new(), 12);
    accepted.admission_units = 1;
    let started = claim(ToolCallId::new(), 13);
    let accepted_intent = AdmissionIntent {
        runtime_fingerprint_version: 1,
        runtime_fingerprint: [12; 32],
        admission_binding_fingerprint_version: 1,
        admission_binding_fingerprint: [13; 32],
        spawn_intent: vec![0x12],
    };
    assert!(matches!(
        store
            .claim_admission_batch(&accepted, vec![accepted_intent])
            .await
            .unwrap(),
        AdmissionBatchClaimOutcome::Claimed(_)
    ));
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
    assert_eq!(persisted, recovered_record);
    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 0);
    assert_eq!(counts.non_active, 0);
    assert_eq!(counts.total, 0);
    assert!(
        store
            .replay(accepted.root_session)
            .await
            .unwrap()
            .is_empty()
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

    let intents = vec![
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [1; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [2; 32],
            spawn_intent: vec![1],
        };
        256
    ];
    let outcome = store.claim_admission_batch(&batch, intents).await.unwrap();
    assert!(matches!(outcome, AdmissionBatchClaimOutcome::Claimed(_)));
    let members = store.admissions(batch.operation_id).await.unwrap();
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
    let rejected_intents = vec![AdmissionIntent {
        runtime_fingerprint_version: 1,
        runtime_fingerprint: [11; 32],
        admission_binding_fingerprint_version: 1,
        admission_binding_fingerprint: [12; 32],
        spawn_intent: vec![6],
    }];
    assert!(matches!(
        store
            .claim_admission_batch(&rejected, rejected_intents)
            .await,
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

    let intents = vec![
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [3; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [4; 32],
            spawn_intent: vec![2],
        };
        256
    ];
    let first = store
        .claim_admission_batch(&batch, intents.clone())
        .await
        .unwrap();
    assert!(matches!(first, AdmissionBatchClaimOutcome::Claimed(_)));
    let retry = store.claim_admission_batch(&batch, intents).await.unwrap();
    assert!(matches!(retry, AdmissionBatchClaimOutcome::Existing));
    let records = store.admissions(batch.operation_id).await.unwrap();
    assert_eq!(records.len(), 256);
    assert!(records.iter().all(|record| record.batch_size == 256));

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
    let first_intents = vec![
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [5; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [6; 32],
            spawn_intent: vec![3],
        };
        255
    ];
    let first_outcome = store
        .claim_admission_batch(&first, first_intents)
        .await
        .unwrap();
    assert!(matches!(
        first_outcome,
        AdmissionBatchClaimOutcome::Claimed(_)
    ));

    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 155);
    assert_eq!(counts.total, 255);

    let mut second = claim(ToolCallId::new(), 30);
    second.admission_units = 2;
    let second_intents = vec![
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [7; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [8; 32],
            spawn_intent: vec![4],
        };
        2
    ];
    assert!(matches!(
        store.claim_admission_batch(&second, second_intents).await,
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
    let first_intents = vec![
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [9; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [10; 32],
            spawn_intent: vec![5],
        };
        2
    ];
    let first_outcome = store
        .claim_admission_batch(&first, first_intents.clone())
        .await
        .unwrap();
    assert!(matches!(
        first_outcome,
        AdmissionBatchClaimOutcome::Claimed(_)
    ));
    let first_records = store.admissions(first.operation_id).await.unwrap();
    let first_counts = store.admission_counts().await.unwrap();

    let mut second = first.clone();
    second.operation_id = OperationId::from_storage_uuid(uuid::Uuid::now_v7());
    second.request_fingerprint = [32; 32];
    assert!(matches!(
        store
            .claim_admission_batch(&second, first_intents)
            .await,
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

#[tokio::test]
async fn durable_batch_persists_binding_and_returns_only_accepted_launches() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let mut batch = claim(ToolCallId::new(), 32);
    batch.admission_units = 101;
    let intents: Vec<AdmissionIntent> = (0_u32..101)
        .map(|ordinal| AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [ordinal as u8; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [ordinal as u8 ^ 0x5a; 32],
            spawn_intent: vec![0xa5, ordinal as u8],
        })
        .collect();

    let outcome = store
        .claim_admission_batch(&batch, intents.clone())
        .await
        .unwrap();
    let launches = match outcome {
        AdmissionBatchClaimOutcome::Claimed(launches) => launches,
        other => panic!("expected claimed launch instructions, got {other:?}"),
    };
    assert_eq!(launches.len(), 100);
    for (ordinal, (launch, expected_intent)) in launches.iter().zip(&intents).enumerate() {
        assert_eq!(launch.record.member_ordinal, ordinal as u32);
        assert_eq!(launch.record.state, AdmissionState::Accepted);
        assert_eq!(&launch.intent, expected_intent);
    }
    assert!(
        launches
            .iter()
            .all(|launch| launch.record.member_ordinal < 100)
    );

    let records = store.admissions(batch.operation_id).await.unwrap();
    assert_eq!(records.len(), 101);
    assert_eq!(
        records
            .iter()
            .map(|record| record.member_ordinal)
            .collect::<Vec<_>>(),
        (0_u32..101).collect::<Vec<_>>()
    );
    assert!(
        records
            .iter()
            .take(100)
            .all(|record| record.state == AdmissionState::Accepted)
    );
    assert_eq!(records[100].state, AdmissionState::Queued);

    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", temp_db.path()))
        .await
        .unwrap();
    let rows = sqlx::query(
        "SELECT member_ordinal, runtime_fingerprint_version, runtime_fingerprint, \
                admission_binding_fingerprint_version, admission_binding_fingerprint, spawn_intent \
         FROM admission_journal WHERE operation_id = ? ORDER BY member_ordinal",
    )
    .bind(batch.operation_id.as_uuid().as_bytes().as_slice())
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert_eq!(rows.len(), intents.len());
    let stored_rows = rows
        .iter()
        .map(|row| {
            (
                row.try_get::<i64, _>("member_ordinal").unwrap(),
                row.try_get::<i64, _>("runtime_fingerprint_version")
                    .unwrap(),
                row.try_get::<Vec<u8>, _>("runtime_fingerprint").unwrap(),
                row.try_get::<i64, _>("admission_binding_fingerprint_version")
                    .unwrap(),
                row.try_get::<Vec<u8>, _>("admission_binding_fingerprint")
                    .unwrap(),
                row.try_get::<Vec<u8>, _>("spawn_intent").unwrap(),
            )
        })
        .collect::<Vec<_>>();
    for (
        ordinal,
        (
            member_ordinal,
            runtime_fingerprint_version,
            runtime_fingerprint,
            admission_binding_fingerprint_version,
            admission_binding_fingerprint,
            spawn_intent,
        ),
    ) in stored_rows.iter().enumerate()
    {
        let intent = &intents[ordinal];
        assert_eq!(*member_ordinal, ordinal as i64);
        assert_eq!(*runtime_fingerprint_version, 1);
        assert_eq!(
            runtime_fingerprint.as_slice(),
            intent.runtime_fingerprint.as_slice()
        );
        assert_eq!(*admission_binding_fingerprint_version, 1);
        assert_eq!(
            admission_binding_fingerprint.as_slice(),
            intent.admission_binding_fingerprint.as_slice()
        );
        assert_eq!(spawn_intent.as_slice(), intent.spawn_intent.as_slice());
    }

    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 1);
    assert_eq!(counts.total, 101);

    let retry = store
        .claim_admission_batch(&batch, intents.clone())
        .await
        .unwrap();
    assert!(matches!(retry, AdmissionBatchClaimOutcome::Existing));
    assert_eq!(store.admissions(batch.operation_id).await.unwrap(), records);
    assert_eq!(store.admission_counts().await.unwrap(), counts);

    let mut conflicting_intents = intents.clone();
    conflicting_intents[100].spawn_intent[0] ^= 1;
    assert!(matches!(
        store
            .claim_admission_batch(&batch, conflicting_intents)
            .await,
        Err(StoreError::OperationIdConflict { operation_id })
            if operation_id == batch.operation_id
    ));
    assert_eq!(store.admissions(batch.operation_id).await.unwrap(), records);
    assert_eq!(store.admission_counts().await.unwrap(), counts);

    let rows_after_conflict = sqlx::query(
        "SELECT member_ordinal, runtime_fingerprint_version, runtime_fingerprint, \
                admission_binding_fingerprint_version, admission_binding_fingerprint, spawn_intent \
         FROM admission_journal WHERE operation_id = ? ORDER BY member_ordinal",
    )
    .bind(batch.operation_id.as_uuid().as_bytes().as_slice())
    .fetch_all(&mut connection)
    .await
    .unwrap();
    let stored_rows_after_conflict = rows_after_conflict
        .iter()
        .map(|row| {
            (
                row.try_get::<i64, _>("member_ordinal").unwrap(),
                row.try_get::<i64, _>("runtime_fingerprint_version")
                    .unwrap(),
                row.try_get::<Vec<u8>, _>("runtime_fingerprint").unwrap(),
                row.try_get::<i64, _>("admission_binding_fingerprint_version")
                    .unwrap(),
                row.try_get::<Vec<u8>, _>("admission_binding_fingerprint")
                    .unwrap(),
                row.try_get::<Vec<u8>, _>("spawn_intent").unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(stored_rows_after_conflict, stored_rows);
}

#[tokio::test]
async fn batch_member_start_is_exactly_once_and_queued_cannot_cross() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let mut batch = claim(ToolCallId::new(), 33);
    batch.admission_units = 101;
    let intents: Vec<AdmissionIntent> = (0_u32..101)
        .map(|ordinal| AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [ordinal as u8; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [ordinal as u8 ^ 0x5a; 32],
            spawn_intent: vec![0xa5, ordinal as u8],
        })
        .collect();

    let outcome = store.claim_admission_batch(&batch, intents).await.unwrap();
    let launches = match outcome {
        AdmissionBatchClaimOutcome::Claimed(launches) => launches,
        other => panic!("expected claimed launch instructions, got {other:?}"),
    };
    assert_eq!(launches.len(), 100);
    assert!(launches.iter().all(|launch| {
        launch.record.state == AdmissionState::Accepted && launch.record.member_ordinal < 100
    }));

    let left_future = async {
        let outcome: Result<AdmissionStartOutcome, StoreError> = store
            .start_admission_member(batch.operation_id, 42, None)
            .await;
        outcome
    };
    let right_future = async {
        let outcome: Result<AdmissionStartOutcome, StoreError> = store
            .start_admission_member(batch.operation_id, 42, None)
            .await;
        outcome
    };
    let (left, right) = tokio::join!(left_future, right_future);
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
    assert!(outcomes.iter().all(|outcome| match outcome {
        AdmissionStartOutcome::Started(record) | AdmissionStartOutcome::Existing(record) => {
            record.member_ordinal == 42
        }
    }));

    let independent = store
        .start_admission_member(batch.operation_id, 99, None)
        .await
        .unwrap();
    assert!(matches!(
        independent,
        AdmissionStartOutcome::Started(record)
            if record.member_ordinal == 99 && record.state == AdmissionState::Started
    ));

    let queued = store
        .start_admission_member(batch.operation_id, 100, None)
        .await
        .unwrap();
    assert!(matches!(
        queued,
        AdmissionStartOutcome::Existing(record)
            if record.member_ordinal == 100 && record.state == AdmissionState::Queued
    ));

    let records = store.admissions(batch.operation_id).await.unwrap();
    assert_eq!(records.len(), 101);
    assert_eq!(
        records
            .iter()
            .map(|record| record.member_ordinal)
            .collect::<Vec<_>>(),
        (0_u32..101).collect::<Vec<_>>()
    );
    for record in &records {
        match record.member_ordinal {
            42 | 99 => assert_eq!(record.state, AdmissionState::Started),
            0..=98 => assert_eq!(record.state, AdmissionState::Accepted),
            100 => assert_eq!(record.state, AdmissionState::Queued),
            _ => unreachable!(),
        }
    }

    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 1);
    assert_eq!(counts.total, 101);
}

#[tokio::test]
async fn recovered_queued_member_promotes_to_one_exact_launch() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let mut admission = claim(ToolCallId::new(), 41);
    admission.admission_units = 1;
    let intent = AdmissionIntent {
        runtime_fingerprint_version: 1,
        runtime_fingerprint: [0x41; 32],
        admission_binding_fingerprint_version: 1,
        admission_binding_fingerprint: [0xa1; 32],
        spawn_intent: vec![0xde, 0xad, 0xbe, 0xef],
    };

    let launches = match store
        .claim_admission_batch(&admission, vec![intent.clone()])
        .await
        .unwrap()
    {
        AdmissionBatchClaimOutcome::Claimed(launches) => launches,
        other => panic!("expected one accepted launch, got {other:?}"),
    };
    assert_eq!(launches.len(), 1);
    let launch = launches.into_iter().next().unwrap();
    assert_eq!(launch.record.state, AdmissionState::Accepted);
    assert_eq!(launch.intent, intent);

    let recovered = store
        .recover_nonterminal_admissions("startup recovery")
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    let recovered_record = recovered.into_iter().next().unwrap();
    assert_eq!(recovered_record.operation_id, admission.operation_id);
    assert_eq!(recovered_record.state, AdmissionState::Queued);
    let recovered_counts = store.admission_counts().await.unwrap();
    assert_eq!(recovered_counts.active, 0);
    assert_eq!(recovered_counts.non_active, 1);
    assert_eq!(recovered_counts.total, 1);

    let promoted = store.promote_queued_admissions(1).await.unwrap();
    assert_eq!(promoted.len(), 1);
    let promoted_launch = promoted.into_iter().next().unwrap();
    assert_eq!(promoted_launch.record.operation_id, admission.operation_id);
    assert_eq!(
        promoted_launch.record.source_tool_call_id,
        admission.source_tool_call_id
    );
    assert_eq!(promoted_launch.record.root_session, admission.root_session);
    assert_eq!(
        promoted_launch.record.request_fingerprint,
        admission.request_fingerprint
    );
    assert_eq!(promoted_launch.record.member_ordinal, 0);
    assert_eq!(promoted_launch.record.batch_size, 1);
    assert_eq!(promoted_launch.record.admission_units, 1);
    assert_eq!(promoted_launch.record.state, AdmissionState::Accepted);
    assert!(promoted_launch.record.actor.is_none());
    assert_eq!(promoted_launch.intent, intent);

    let persisted = store
        .admission(admission.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted, promoted_launch.record);
    let promoted_counts = store.admission_counts().await.unwrap();
    assert_eq!(promoted_counts.active, 1);
    assert_eq!(promoted_counts.non_active, 0);
    assert_eq!(promoted_counts.total, 1);

    let repeated = store.promote_queued_admissions(1).await.unwrap();
    assert!(repeated.is_empty());
    let repeated_record = store
        .admission(admission.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(repeated_record, promoted_launch.record);
    assert_eq!(repeated_record.state, AdmissionState::Accepted);
}

#[tokio::test]
async fn promotion_round_robin_is_durable_after_terminal_release_and_restart() {
    let temp_db = AdmissionTempDb::new();
    let mut store = SessionStore::connect(temp_db.path()).await.unwrap();
    let root_a = SessionId::new();
    let root_b = SessionId::new();
    let mut claims = [
        claim(ToolCallId::new(), 42),
        claim(ToolCallId::new(), 43),
        claim(ToolCallId::new(), 44),
        claim(ToolCallId::new(), 45),
    ];
    for (admission, root_session) in claims.iter_mut().zip([root_a, root_a, root_b, root_b]) {
        admission.root_session = root_session;
        admission.admission_units = 1;
    }
    let intents = [
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [0x42; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [0xa2; 32],
            spawn_intent: vec![0xd1, 0x01],
        },
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [0x43; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [0xa3; 32],
            spawn_intent: vec![0xd1, 0x02],
        },
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [0x44; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [0xa4; 32],
            spawn_intent: vec![0xd1, 0x03],
        },
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [0x45; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [0xa5; 32],
            spawn_intent: vec![0xd1, 0x04],
        },
    ];
    for (admission, intent) in claims.iter().zip(intents) {
        let outcome = store
            .claim_admission_batch(admission, vec![intent])
            .await
            .unwrap();
        match outcome {
            AdmissionBatchClaimOutcome::Claimed(launches) => {
                assert_eq!(launches.len(), 1);
                assert_eq!(launches[0].record.state, AdmissionState::Accepted);
            }
            other => panic!("expected one accepted launch, got {other:?}"),
        }
    }

    let recovered = store
        .recover_nonterminal_admissions("startup recovery")
        .await
        .unwrap();
    assert_eq!(recovered.len(), 4);
    let recovered_counts = store.admission_counts().await.unwrap();
    assert_eq!(recovered_counts.active, 0);
    assert_eq!(recovered_counts.non_active, 4);
    assert_eq!(recovered_counts.total, 4);

    let first = store.promote_queued_admissions(1).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].record.operation_id, claims[0].operation_id);
    store
        .finalize_admission(
            claims[0].operation_id,
            AdmissionTerminal::Cancelled,
            "terminal release",
            None,
        )
        .await
        .unwrap();
    drop(store);
    store = SessionStore::connect(temp_db.path()).await.unwrap();

    let promoted = store.promote_queued_admissions(3).await.unwrap();
    assert_eq!(promoted.len(), 3);
    assert_eq!(promoted[0].record.operation_id, claims[2].operation_id);
    assert_eq!(promoted[1].record.operation_id, claims[1].operation_id);
    assert_eq!(promoted[2].record.operation_id, claims[3].operation_id);
    assert!(store.promote_queued_admissions(1).await.unwrap().is_empty());
    let final_counts = store.admission_counts().await.unwrap();
    assert_eq!(final_counts.active, 3);
    assert_eq!(final_counts.non_active, 0);
    assert_eq!(final_counts.total, 3);
}

#[tokio::test]
async fn parent_wait_claims_child_atomically_and_requeues_through_fair_scheduler() {
    let temp_db = AdmissionTempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let actor_claim = store
        .try_claim_new(SessionId::new(), OwnerRunId::new())
        .await
        .unwrap();
    let mut parent = claim(ToolCallId::new(), 46);
    parent.admission_units = 1;
    parent.actor_claim = Some(actor_claim);
    let parent_intent = AdmissionIntent {
        runtime_fingerprint_version: 1,
        runtime_fingerprint: [0x46; 32],
        admission_binding_fingerprint_version: 1,
        admission_binding_fingerprint: [0xa6; 32],
        spawn_intent: vec![0xd1, 0x05],
    };
    assert!(matches!(
        store
            .claim_admission_batch(&parent, vec![parent_intent.clone()])
            .await
            .unwrap(),
        AdmissionBatchClaimOutcome::Claimed(launches)
            if launches.len() == 1 && launches[0].intent == parent_intent
    ));
    let parent_started = match store
        .start_admission_member(parent.operation_id, 0, Some(&actor_claim))
        .await
        .unwrap()
    {
        AdmissionStartOutcome::Started(record) => record,
        other => panic!("expected parent member to start, got {other:?}"),
    };

    let mut child = claim(ToolCallId::new(), 47);
    child.root_session = parent.root_session;
    child.admission_units = 2;
    child.actor_claim = Some(actor_claim);
    let child_intents = vec![
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [0x47; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [0xa7; 32],
            spawn_intent: vec![0xd1, 0x06],
        },
        AdmissionIntent {
            runtime_fingerprint_version: 1,
            runtime_fingerprint: [0x48; 32],
            admission_binding_fingerprint_version: 1,
            admission_binding_fingerprint: [0xa8; 32],
            spawn_intent: vec![0xd1, 0x07],
        },
    ];
    let child_launches = match store
        .suspend_parent_and_claim_admission_batch(
            parent.operation_id,
            0,
            &child,
            child_intents.clone(),
        )
        .await
        .unwrap()
    {
        AdmissionBatchClaimOutcome::Claimed(launches) => launches,
        other => panic!("expected two accepted child launches, got {other:?}"),
    };
    assert_eq!(child_launches.len(), 2);
    for (ordinal, (launch, intent)) in child_launches.iter().zip(&child_intents).enumerate() {
        assert_eq!(launch.record.state, AdmissionState::Accepted);
        assert_eq!(launch.record.member_ordinal, ordinal as u32);
        assert_eq!(&launch.intent, intent);
    }
    let parent_waiting = store.admissions(parent.operation_id).await.unwrap();
    assert_eq!(parent_waiting.len(), 1);
    assert_eq!(parent_waiting[0].state, AdmissionState::Waiting);
    assert_eq!(parent_waiting[0].operation_id, parent_started.operation_id);
    assert_eq!(
        parent_waiting[0].source_tool_call_id,
        parent_started.source_tool_call_id
    );
    assert_eq!(parent_waiting[0].root_session, parent_started.root_session);
    assert_eq!(
        parent_waiting[0].request_fingerprint,
        parent_started.request_fingerprint
    );
    assert_eq!(
        parent_waiting[0].member_ordinal,
        parent_started.member_ordinal
    );
    assert_eq!(parent_waiting[0].batch_size, parent_started.batch_size);
    assert_eq!(
        parent_waiting[0].admission_units,
        parent_started.admission_units
    );
    assert_eq!(
        parent_waiting[0].actor,
        Some(AdmissionActorBinding {
            actor_id: actor_claim.actor_id,
            actor_epoch: actor_claim.epoch,
        })
    );
    let child_records = store.admissions(child.operation_id).await.unwrap();
    assert_eq!(child_records.len(), 2);
    assert!(child_records.iter().all(|record| {
        record.state == AdmissionState::Accepted
            && record.root_session == parent.root_session
            && record.batch_size == 2
            && record.admission_units == 1
            && record.actor
                == Some(AdmissionActorBinding {
                    actor_id: actor_claim.actor_id,
                    actor_epoch: actor_claim.epoch,
                })
    }));
    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 2);
    assert_eq!(counts.non_active, 1);
    assert_eq!(counts.total, 3);

    let queued_parent = store
        .queue_waiting_admission_member(parent.operation_id, 0, Some(&actor_claim))
        .await
        .unwrap();
    assert_eq!(queued_parent.operation_id, parent.operation_id);
    assert_eq!(queued_parent.member_ordinal, 0);
    assert_eq!(queued_parent.state, AdmissionState::Queued);
    let queued_parent_records = store.admissions(parent.operation_id).await.unwrap();
    assert_eq!(queued_parent_records[0].state, AdmissionState::Queued);
    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 2);
    assert_eq!(counts.non_active, 1);
    assert_eq!(counts.total, 3);

    let queued_again = store
        .queue_waiting_admission_member(parent.operation_id, 0, Some(&actor_claim))
        .await
        .unwrap();
    assert_eq!(queued_again, queued_parent);
    let wrong_claim = store
        .queue_waiting_admission_member(parent.operation_id, 0, None)
        .await;
    assert!(matches!(wrong_claim, Err(StoreError::AdmissionData(_))));
    let queued_after_wrong_claim = store.admissions(parent.operation_id).await.unwrap();
    assert_eq!(queued_after_wrong_claim.len(), 1);
    assert_eq!(queued_after_wrong_claim[0].state, AdmissionState::Queued);
    assert_eq!(queued_after_wrong_claim[0].actor, queued_parent.actor);

    let promoted_parent = store.promote_queued_admissions(1).await.unwrap();
    assert_eq!(promoted_parent.len(), 1);
    assert_eq!(promoted_parent[0].record.operation_id, parent.operation_id);
    assert_eq!(promoted_parent[0].record.member_ordinal, 0);
    assert_eq!(promoted_parent[0].record.state, AdmissionState::Accepted);
    assert_eq!(promoted_parent[0].intent, parent_intent);
    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 3);
    assert_eq!(counts.non_active, 0);
    assert_eq!(counts.total, 3);

    let rollback_db = AdmissionTempDb::new();
    let rollback_store = SessionStore::connect(rollback_db.path()).await.unwrap();
    let mut rollback_parent = claim(ToolCallId::new(), 49);
    rollback_parent.admission_units = 1;
    let rollback_parent_intent = AdmissionIntent {
        runtime_fingerprint_version: 1,
        runtime_fingerprint: [0x49; 32],
        admission_binding_fingerprint_version: 1,
        admission_binding_fingerprint: [0xa9; 32],
        spawn_intent: vec![0xd1, 0x08],
    };
    assert!(matches!(
        rollback_store
            .claim_admission_batch(&rollback_parent, vec![rollback_parent_intent])
            .await
            .unwrap(),
        AdmissionBatchClaimOutcome::Claimed(_)
    ));
    assert!(matches!(
        rollback_store
            .start_admission_member(rollback_parent.operation_id, 0, None)
            .await
            .unwrap(),
        AdmissionStartOutcome::Started(_)
    ));
    let mut filler = claim(ToolCallId::new(), 50);
    filler.admission_units = 255;
    let filler_intent = AdmissionIntent {
        runtime_fingerprint_version: 1,
        runtime_fingerprint: [0x50; 32],
        admission_binding_fingerprint_version: 1,
        admission_binding_fingerprint: [0xaa; 32],
        spawn_intent: vec![0xd1, 0x09],
    };
    assert!(matches!(
        rollback_store
            .claim_admission_batch(&filler, vec![filler_intent; 255])
            .await
            .unwrap(),
        AdmissionBatchClaimOutcome::Claimed(_)
    ));
    let counts = rollback_store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 156);
    assert_eq!(counts.total, 256);

    let mut rejected_child = claim(ToolCallId::new(), 51);
    rejected_child.root_session = rollback_parent.root_session;
    rejected_child.admission_units = 1;
    let rejected_child_intent = AdmissionIntent {
        runtime_fingerprint_version: 1,
        runtime_fingerprint: [0x51; 32],
        admission_binding_fingerprint_version: 1,
        admission_binding_fingerprint: [0xab; 32],
        spawn_intent: vec![0xd1, 0x0a],
    };
    assert!(matches!(
        rollback_store
            .suspend_parent_and_claim_admission_batch(
                rollback_parent.operation_id,
                0,
                &rejected_child,
                vec![rejected_child_intent],
            )
            .await,
        Err(StoreError::AdmissionCapacityExceeded {
            active: 100,
            non_active: 156,
            requested: 1,
        })
    ));
    let rollback_parent_records = rollback_store
        .admissions(rollback_parent.operation_id)
        .await
        .unwrap();
    assert_eq!(rollback_parent_records.len(), 1);
    assert_eq!(rollback_parent_records[0].state, AdmissionState::Started);
    assert!(
        rollback_store
            .admissions(rejected_child.operation_id)
            .await
            .unwrap()
            .is_empty()
    );
    let counts = rollback_store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 156);
    assert_eq!(counts.total, 256);
}
