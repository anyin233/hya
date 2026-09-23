//! Durable projection snapshots are a pure cache of the shared reducer:
//! decoding a snapshot of any prefix and folding the rest of the log must equal
//! a full replay, and the reducer version pins the snapshot contract.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/event_script.rs"]
mod event_script;

use hya_proto::{
    AgentName, Envelope, Event, EventSeq, OwnerRunId, PROJECTION_REDUCER_VERSION, Projection,
    SessionId, WorkflowIdentity, WorkflowRevision, WorkflowRunId, WorkflowRunStatus,
    WorkflowSourceId, WorkflowStagePlan,
};
use uuid::Uuid;

fn envelopes(events: Vec<Event>) -> Vec<Envelope> {
    events
        .into_iter()
        .enumerate()
        .map(|(index, event)| Envelope {
            seq: EventSeq(index as u64 + 1),
            ts_millis: 0,
            event,
        })
        .collect()
}

fn fold_from_snapshot(prefix: &[Envelope], tail: &[Envelope]) -> Projection {
    let bytes = Projection::from_events(prefix)
        .encode_snapshot()
        .expect("encode snapshot");
    let mut projection = Projection::decode_snapshot(&bytes).expect("decode snapshot");
    for envelope in tail {
        projection.apply(envelope);
    }
    projection
}

fn run_started(session: SessionId, run: WorkflowRunId, revision: u8) -> Event {
    Event::WorkflowRunStarted {
        session,
        run,
        workflow: WorkflowIdentity {
            source: WorkflowSourceId::new("project:snapshot"),
            name: "snapshot".to_string(),
            revision: WorkflowRevision::from_bytes([revision; 32]),
        },
        request_hash: format!("hash-{revision}"),
        owner: OwnerRunId::from_storage(Uuid::from_u128(9)),
        stages: vec![WorkflowStagePlan {
            id: "plan".to_string(),
            title: None,
            agent: AgentName::new("planner"),
            mode: "once".to_string(),
            level: 0,
            worker_model: None,
            selected_worker_model: None,
            verifier_model: None,
            selected_verifier_model: None,
        }],
    }
}

/// A re-emitted start for an already-seen run is ignored by replay; the
/// snapshot must carry that replay-only dedupe state or the tail re-applies it.
#[test]
fn snapshot_keeps_replay_only_workflow_dedupe_state() {
    let session = SessionId::from_uuid(Uuid::from_u128(1));
    let first = WorkflowRunId::from_uuid(Uuid::from_u128(2));
    let second = WorkflowRunId::from_uuid(Uuid::from_u128(3));
    let log = envelopes(vec![
        run_started(session, first, 1),
        Event::WorkflowRunFinished {
            session,
            run: first,
            status: WorkflowRunStatus::Completed,
            error: None,
        },
        run_started(session, second, 2),
        run_started(session, first, 1),
    ]);

    let full = Projection::from_events(&log);
    assert_eq!(
        full.session
            .workflow
            .as_ref()
            .and_then(|workflow| workflow.run.as_ref())
            .map(|run| run.id),
        Some(second)
    );
    assert_eq!(fold_from_snapshot(&log[..3], &log[3..]), full);
}

/// Property: for generated logs (streaming, usage, deletes, compaction, forks,
/// Workflow runs, team traffic), snapshot(prefix) + tail == full replay at
/// every split point.
#[test]
fn snapshot_plus_tail_equals_full_replay_at_every_split() {
    for seed in 0..48_u64 {
        let session = SessionId::from_uuid(Uuid::from_u128(u128::from(seed) + 100));
        let fork_of = (seed % 4 == 0).then(|| SessionId::from_uuid(Uuid::from_u128(99)));
        let log = envelopes(event_script::script(seed, session, fork_of, 90));
        let full = Projection::from_events(&log);
        for split in 0..=log.len() {
            assert_eq!(
                fold_from_snapshot(&log[..split], &log[split..]),
                full,
                "seed {seed} split {split}"
            );
        }
    }
}

/// A snapshot is rejected rather than misread when it is not a snapshot.
#[test]
fn decode_snapshot_rejects_garbage() {
    assert!(Projection::decode_snapshot(b"{\"not\":\"a snapshot\"}").is_err());
    assert!(Projection::decode_snapshot(b"\xff").is_err());
}

/// FNV-1a 64: stable across platforms and Rust releases.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Pins the reducer version to the reducer's observable output.
///
/// When this fails, the fold result or the snapshot encoding changed for the
/// same events: bump `PROJECTION_REDUCER_VERSION` (so stored snapshots folded
/// by the old reducer are rebuilt instead of trusted) and record the new
/// fingerprint here together with the new version. (Changing only the event
/// generator also moves the fingerprint; then record it without a bump.)
#[test]
fn reducer_fingerprint_pins_the_version() {
    let mut encoded = Vec::new();
    for seed in 0..8_u64 {
        let session = SessionId::from_uuid(Uuid::from_u128(u128::from(seed) + 500));
        let fork_of = (seed % 2 == 0).then(|| SessionId::from_uuid(Uuid::from_u128(499)));
        let log = envelopes(event_script::script(seed, session, fork_of, 120));
        encoded.extend(
            Projection::from_events(&log)
                .encode_snapshot()
                .expect("encode snapshot"),
        );
    }
    let fingerprint = fnv1a(&encoded);
    assert_eq!(
        (PROJECTION_REDUCER_VERSION, fingerprint),
        (1, 0x1a0d_46a6_8229_d3eb),
        "reducer output changed: bump PROJECTION_REDUCER_VERSION and record \
         the new fingerprint {fingerprint:#018x}"
    );
}
