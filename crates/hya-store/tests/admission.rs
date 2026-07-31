#![allow(clippy::unwrap_used)]

use hya_proto::{OperationId, SessionId, ToolCallId};
use hya_store::{
    AdmissionClaim, AdmissionClaimOutcome, AdmissionStartOutcome, AdmissionState,
    AdmissionTerminal, SessionStore, StoreError,
};

fn claim(source_tool_call_id: ToolCallId, fingerprint: u8) -> AdmissionClaim {
    AdmissionClaim {
        operation_id: OperationId::from_tool_call(source_tool_call_id),
        source_tool_call_id,
        root_session: SessionId::new(),
        request_fingerprint: [fingerprint; 32],
        admission_units: 2,
    }
}

#[tokio::test]
async fn concurrent_start_has_exactly_one_dispatch_winner() {
    let store = SessionStore::connect_memory().await.unwrap();
    let admission = claim(ToolCallId::new(), 9);
    store.claim_admission(&admission).await.unwrap();

    let (left, right) = tokio::join!(
        store.start_admission(admission.operation_id),
        store.start_admission(admission.operation_id)
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
        )
        .await
        .unwrap();
    let aborted_again = store
        .finalize_admission(
            accepted.operation_id,
            AdmissionTerminal::Aborted,
            "overloaded",
        )
        .await
        .unwrap();
    let conflict = store
        .finalize_admission(
            accepted.operation_id,
            AdmissionTerminal::Cancelled,
            "different terminal",
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
        store.start_admission(started.operation_id).await.unwrap(),
        AdmissionStartOutcome::Started(_)
    ));
    let completed = store
        .finalize_admission(
            started.operation_id,
            AdmissionTerminal::Completed,
            "completed",
        )
        .await
        .unwrap();
    let completed_again = store
        .finalize_admission(
            started.operation_id,
            AdmissionTerminal::Completed,
            "completed",
        )
        .await
        .unwrap();

    assert_eq!(completed.record.state, AdmissionState::Completed);
    assert!(completed.release_required);
    assert!(!completed_again.release_required);
    assert!(completed_again.record.logical_released);
}

#[tokio::test]
async fn startup_recovery_atomically_aborts_nonterminal_without_public_events() {
    let store = SessionStore::connect_memory().await.unwrap();
    let accepted = claim(ToolCallId::new(), 12);
    let started = claim(ToolCallId::new(), 13);
    store.claim_admission(&accepted).await.unwrap();
    store.claim_admission(&started).await.unwrap();
    store.start_admission(started.operation_id).await.unwrap();

    let recovered = store
        .abort_nonterminal_admissions("startup recovery")
        .await
        .unwrap();
    let repeated = store
        .abort_nonterminal_admissions("startup recovery")
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
    assert_eq!(accepted_record.state, AdmissionState::Aborted);
    assert!(!accepted_record.logical_released);
    assert_eq!(started_record.state, AdmissionState::Aborted);
    assert!(started_record.logical_released);
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
