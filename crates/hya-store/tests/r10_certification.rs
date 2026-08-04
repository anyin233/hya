//! R10 / 0.34.12 capacity certification gate (CI-bounded residual expansion).
//!
//! Proves the durable admission envelope vector required for truthful R10:
//! active <= 100, non-active <= 156, total <= 256, item 257 typed overload with
//! zero partial commit, bounded promotion after Started release, serial
//! promotion fairness, and startup reconstruction of nonterminal rows.
//!
//! P9.2 30-minute / 10k soak is intentionally `#[ignore]` for default CI.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{OperationId, SessionId, ToolCallId};
use hya_store::{
    AdmissionBatchClaimOutcome, AdmissionClaim, AdmissionIntent, AdmissionStartOutcome,
    AdmissionState, AdmissionTerminal, SessionStore, StoreError,
};

struct TempDb {
    path: String,
}

impl TempDb {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .join(format!("hya-r10-{}.db", SessionId::new()))
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

fn intent(byte: u8) -> AdmissionIntent {
    AdmissionIntent {
        runtime_fingerprint_version: 1,
        runtime_fingerprint: [byte; 32],
        admission_binding_fingerprint_version: 1,
        admission_binding_fingerprint: [byte.wrapping_add(1); 32],
        spawn_intent: vec![byte, 0x01],
    }
}

fn claim(units: u32, fingerprint: u8) -> AdmissionClaim {
    let source = ToolCallId::new();
    AdmissionClaim {
        operation_id: OperationId::from_tool_call(source),
        source_tool_call_id: source,
        root_session: SessionId::new(),
        request_fingerprint: [fingerprint; 32],
        admission_units: units,
        actor_claim: None,
    }
}

#[tokio::test]
async fn r10_capacity_vector_100_156_and_item_257_zero_allocation() {
    let store = SessionStore::connect_memory().await.unwrap();

    let full = claim(256, 0x10);
    let intents = (0..256).map(|i| intent(i as u8)).collect();
    match store.claim_admission_batch(&full, intents).await.unwrap() {
        AdmissionBatchClaimOutcome::Claimed(launches) => {
            assert_eq!(launches.len(), 100);
        }
        other => panic!("expected claimed full envelope batch, got {other:?}"),
    }
    let members = store.admissions(full.operation_id).await.unwrap();
    assert_eq!(members.len(), 256);
    assert!(
        members
            .iter()
            .take(100)
            .all(|r| r.state == AdmissionState::Accepted)
    );
    assert!(
        members
            .iter()
            .skip(100)
            .all(|r| r.state == AdmissionState::Queued)
    );

    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 156);
    assert_eq!(counts.total, 256);

    let overflow = claim(1, 0x30);
    let overflow_err = store
        .claim_admission_batch(&overflow, vec![intent(0xFF)])
        .await
        .expect_err("item beyond 256 must reject");
    assert!(
        matches!(overflow_err, StoreError::AdmissionCapacityExceeded { .. }),
        "typed overload required, got {overflow_err:?}"
    );
    assert!(
        store
            .admissions(overflow.operation_id)
            .await
            .unwrap()
            .is_empty()
    );
    let counts_after = store.admission_counts().await.unwrap();
    assert_eq!(counts_after, counts, "overflow must not allocate envelopes");
}

#[tokio::test]
async fn r10_started_release_promotes_exactly_one_queued() {
    let store = SessionStore::connect_memory().await.unwrap();
    let active = claim(100, 0x40);
    let intents = (0..100).map(|i| intent(i as u8)).collect();
    let launches = match store.claim_admission_batch(&active, intents).await.unwrap() {
        AdmissionBatchClaimOutcome::Claimed(launches) => launches,
        other => panic!("expected claimed, {other:?}"),
    };
    let first = &launches[0];
    match store
        .start_admission_member(first.record.operation_id, first.record.member_ordinal, None)
        .await
        .unwrap()
    {
        AdmissionStartOutcome::Started(record) => {
            assert_eq!(record.state, AdmissionState::Started);
        }
        other => panic!("expected Started, {other:?}"),
    }

    let queued_op = claim(1, 0x50);
    match store
        .claim_admission_batch(&queued_op, vec![intent(0xAB)])
        .await
        .unwrap()
    {
        AdmissionBatchClaimOutcome::Claimed(launches) => {
            assert!(launches.is_empty());
        }
        other => panic!("expected queue, {other:?}"),
    }
    assert_eq!(store.admission_counts().await.unwrap().non_active, 1);

    let release = store
        .finalize_admission_members(
            &[(first.record.operation_id, first.record.member_ordinal)],
            AdmissionTerminal::Completed,
            "r10 release one",
            None,
        )
        .await
        .unwrap();
    assert_eq!(release.promoted.len(), 1);
    assert_eq!(release.promoted[0].record.state, AdmissionState::Accepted);
    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 100);
    assert_eq!(counts.non_active, 0);
}

#[tokio::test]
async fn r10_serial_started_releases_promote_queued_one_at_a_time() {
    let store = SessionStore::connect_memory().await.unwrap();
    let active = claim(100, 0x60);
    let intents = (0..100).map(|i| intent(i as u8)).collect();
    let launches = match store.claim_admission_batch(&active, intents).await.unwrap() {
        AdmissionBatchClaimOutcome::Claimed(launches) => launches,
        other => panic!("expected claimed, {other:?}"),
    };
    // Start two Accepted members so two releases can each free one slot.
    for launch in launches.iter().take(2) {
        match store
            .start_admission_member(
                launch.record.operation_id,
                launch.record.member_ordinal,
                None,
            )
            .await
            .unwrap()
        {
            AdmissionStartOutcome::Started(_) => {}
            other => panic!("expected Started, {other:?}"),
        }
    }

    let q1 = claim(1, 0x61);
    let q2 = claim(1, 0x62);
    for (op, byte) in [(&q1, 0xB1u8), (&q2, 0xB2u8)] {
        match store
            .claim_admission_batch(op, vec![intent(byte)])
            .await
            .unwrap()
        {
            AdmissionBatchClaimOutcome::Claimed(launches) => assert!(launches.is_empty()),
            other => panic!("expected queue, {other:?}"),
        }
    }
    assert_eq!(store.admission_counts().await.unwrap().non_active, 2);

    let first_release = store
        .finalize_admission_members(
            &[(
                launches[0].record.operation_id,
                launches[0].record.member_ordinal,
            )],
            AdmissionTerminal::Completed,
            "r10 serial first",
            None,
        )
        .await
        .unwrap();
    assert_eq!(first_release.promoted.len(), 1);
    assert_eq!(
        first_release.promoted[0].record.state,
        AdmissionState::Accepted
    );
    assert_eq!(store.admission_counts().await.unwrap().non_active, 1);

    let second_release = store
        .finalize_admission_members(
            &[(
                launches[1].record.operation_id,
                launches[1].record.member_ordinal,
            )],
            AdmissionTerminal::Completed,
            "r10 serial second",
            None,
        )
        .await
        .unwrap();
    assert_eq!(second_release.promoted.len(), 1);
    assert_eq!(
        second_release.promoted[0].record.state,
        AdmissionState::Accepted
    );
    assert_eq!(store.admission_counts().await.unwrap().non_active, 0);
    assert_ne!(
        first_release.promoted[0].record.operation_id,
        second_release.promoted[0].record.operation_id,
        "serial releases must promote distinct queued operations"
    );
}

#[tokio::test]
async fn r10_restart_reconstructs_started_to_aborted_and_accepted_to_queued() {
    let temp_db = TempDb::new();
    let store = SessionStore::connect(temp_db.path()).await.unwrap();

    let active = claim(2, 0x70);
    let intents = vec![intent(1), intent(2)];
    let launches = match store.claim_admission_batch(&active, intents).await.unwrap() {
        AdmissionBatchClaimOutcome::Claimed(launches) => launches,
        other => panic!("expected claimed, {other:?}"),
    };
    assert_eq!(launches.len(), 2);
    match store
        .start_admission_member(
            launches[0].record.operation_id,
            launches[0].record.member_ordinal,
            None,
        )
        .await
        .unwrap()
    {
        AdmissionStartOutcome::Started(_) => {}
        other => panic!("expected Started, {other:?}"),
    }
    // Leave second member Accepted (not Started).
    let counts_before = store.admission_counts().await.unwrap();
    assert_eq!(counts_before.active, 2);
    drop(store);

    let store = SessionStore::connect(temp_db.path()).await.unwrap();
    let recovered = store
        .recover_nonterminal_admissions("r10 residual restart")
        .await
        .unwrap();
    assert!(!recovered.is_empty());
    let started_row = recovered
        .iter()
        .find(|r| r.member_ordinal == launches[0].record.member_ordinal)
        .expect("started ordinal recovered");
    assert_eq!(started_row.state, AdmissionState::Aborted);
    let accepted_row = recovered
        .iter()
        .find(|r| r.member_ordinal == launches[1].record.member_ordinal)
        .expect("accepted ordinal recovered");
    assert_eq!(accepted_row.state, AdmissionState::Queued);
    let counts = store.admission_counts().await.unwrap();
    assert_eq!(counts.active, 0);
    assert!(counts.non_active >= 1);
}

#[tokio::test]
async fn r10_second_overflow_batch_still_zero_allocates() {
    let store = SessionStore::connect_memory().await.unwrap();
    let full = claim(256, 0x80);
    let intents = (0..256).map(|i| intent(i as u8)).collect();
    assert!(matches!(
        store.claim_admission_batch(&full, intents).await.unwrap(),
        AdmissionBatchClaimOutcome::Claimed(_)
    ));
    let baseline = store.admission_counts().await.unwrap();
    assert_eq!(baseline.total, 256);

    for fingerprint in [0x81u8, 0x82u8, 0x83u8] {
        let overflow = claim(1, fingerprint);
        let err = store
            .claim_admission_batch(&overflow, vec![intent(fingerprint)])
            .await
            .expect_err("overflow must stay typed fail-closed");
        assert!(matches!(err, StoreError::AdmissionCapacityExceeded { .. }));
        assert!(
            store
                .admissions(overflow.operation_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.admission_counts().await.unwrap(), baseline);
    }
}

/// Manual P9.2 soak placeholder — not run in default CI.
#[tokio::test]
#[ignore = "manual soak: hold 100 active for 30m / 10k completions outside CI"]
async fn r10_manual_soak_placeholder() {
    // Operators run capacity soaks outside the default PR gate. The durable
    // vector and promotion tests above are the CI-bounded certification bar.
}
