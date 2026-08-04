//! R10 / 0.34.12 capacity certification gate.
//!
//! Proves the durable admission envelope vector required for truthful R10:
//! active <= 100, non-active <= 156, total <= 256, item 257 typed overload with
//! zero partial commit, and bounded promotion after a Started release.

use hya_proto::{OperationId, SessionId, ToolCallId};
use hya_store::{
    AdmissionBatchClaimOutcome, AdmissionClaim, AdmissionIntent, AdmissionStartOutcome,
    AdmissionState, AdmissionTerminal, SessionStore, StoreError,
};

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
    assert!(members
        .iter()
        .take(100)
        .all(|r| r.state == AdmissionState::Accepted));
    assert!(members
        .iter()
        .skip(100)
        .all(|r| r.state == AdmissionState::Queued));

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
    assert!(store
        .admissions(overflow.operation_id)
        .await
        .unwrap()
        .is_empty());
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
