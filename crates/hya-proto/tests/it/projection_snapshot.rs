//! Durable projection snapshots are a pure cache of the shared reducer:
//! decoding a snapshot of any prefix and folding the rest of the log must equal
//! a full replay, and the reducer version pins the snapshot contract.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/event_script.rs"]
mod event_script;

use std::sync::Arc;

use hya_proto::{
    Envelope, Event, EventSeq, PROJECTION_REDUCER_VERSION, Projection, ProjectionReducer,
    ProjectionReducerRegistry, SessionId,
};

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

fn fold_from_snapshot(
    prefix: &[Envelope],
    tail: &[Envelope],
    reducers: &ProjectionReducerRegistry,
) -> Projection {
    let bytes = Projection::from_events_with(prefix, reducers)
        .encode_snapshot_with(&reducers.signature())
        .expect("encode snapshot");
    let mut projection =
        Projection::decode_snapshot_with(&bytes, &reducers.signature()).expect("decode snapshot");
    for envelope in tail {
        projection.apply_with(envelope, reducers);
    }
    projection
}

struct ScriptExtensionReducer {
    bundle: &'static str,
    version: u32,
}

impl ProjectionReducer for ScriptExtensionReducer {
    fn bundle_id(&self) -> &str {
        self.bundle
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn fold_extension(
        &self,
        projection: &mut Projection,
        _session: SessionId,
        kind: &str,
        payload: &serde_json::Value,
    ) {
        let entry = projection
            .extensions
            .entry(self.bundle.to_owned())
            .or_insert_with(|| serde_json::json!({ "count": 0, "last_kind": null }));
        if let Some(obj) = entry.as_object_mut() {
            let count = obj.get("count").and_then(|v| v.as_u64()).unwrap_or(0) + 1;
            obj.insert("count".to_owned(), serde_json::json!(count));
            obj.insert("last_kind".to_owned(), serde_json::json!(kind));
            obj.insert("last_payload".to_owned(), payload.clone());
        }
    }
}

/// Property: for generated logs (streaming, usage, deletes, compaction, forks,
/// extension events), snapshot(prefix) + tail == full replay at every split point.
#[test]
fn snapshot_plus_tail_equals_full_replay_at_every_split() {
    let empty = ProjectionReducerRegistry::new();
    let with_reducers =
        ProjectionReducerRegistry::new().with_reducer(Arc::new(ScriptExtensionReducer {
            bundle: "hya/workflow",
            version: 1,
        }));
    for seed in 0..48_u64 {
        let session = format!("hysec_{:020}", u128::from(seed) + 100)
            .parse::<SessionId>()
            .unwrap();
        let fork_of =
            (seed % 4 == 0).then(|| format!("hysec_{:020}", 99).parse::<SessionId>().unwrap());
        let log = envelopes(event_script::script(seed, session, fork_of, 90));
        let reducers = if seed % 2 == 0 {
            &with_reducers
        } else {
            &empty
        };
        let full = Projection::from_events_with(&log, reducers);
        for split in 0..=log.len() {
            assert_eq!(
                fold_from_snapshot(&log[..split], &log[split..], reducers),
                full,
                "seed {seed} split {split}"
            );
        }
    }
}

/// A snapshot is rejected rather than misread when it is not a snapshot or when
/// the loaded reducer signature differs.
#[test]
fn decode_snapshot_rejects_garbage_and_reducer_mismatch() {
    assert!(Projection::decode_snapshot(b"{\"not\":\"a snapshot\"}").is_err());
    assert!(Projection::decode_snapshot(b"\xff").is_err());

    let reducers =
        ProjectionReducerRegistry::new().with_reducer(Arc::new(ScriptExtensionReducer {
            bundle: "hya/workflow",
            version: 1,
        }));
    let session = format!("hysec_{:020}", 1).parse::<SessionId>().unwrap();
    let log = envelopes(vec![Event::Extension {
        session,
        bundle: "hya/workflow".to_owned(),
        kind: "tick".to_owned(),
        payload: serde_json::json!({"n": 1}),
    }]);
    let bytes = Projection::from_events_with(&log, &reducers)
        .encode_snapshot_with(&reducers.signature())
        .unwrap();
    assert!(Projection::decode_snapshot(&bytes).is_err());
    assert!(Projection::decode_snapshot_with(&bytes, "hya/workflow@2").is_err());
    assert!(Projection::decode_snapshot_with(&bytes, &reducers.signature()).is_ok());
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
        let session = format!("hysec_{:020}", u128::from(seed) + 500)
            .parse::<SessionId>()
            .unwrap();
        let fork_of =
            (seed % 2 == 0).then(|| format!("hysec_{:020}", 499).parse::<SessionId>().unwrap());
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
        (14, 0x096c_af03_cf9c_1253),
        "reducer output changed: bump PROJECTION_REDUCER_VERSION and record \
         the new fingerprint {fingerprint:#018x}"
    );
}

#[test]
fn historical_todo_results_keep_their_list_and_ids_through_snapshot_replay() {
    use hya_proto::{MessageId, PartId, Role, TodoStatus, ToolCallId};
    use serde_json::json;

    let session = SessionId::new();
    let message = MessageId::new();
    let result = |name: &str, output| {
        let part = PartId::new();
        let call = ToolCallId::new();
        [
            Event::ToolCallRequested {
                session,
                message,
                part,
                call,
                name: name.into(),
                input: json!({}),
                provider_data: None,
            },
            Event::ToolResult {
                session,
                message,
                part,
                call,
                output,
                time_ms: 0,
            },
        ]
    };
    let mut events = vec![Event::MessageStarted {
        session,
        message,
        role: Role::Assistant,
        agent: None,
        model: None,
    }];
    events.extend(result(
        "todowrite",
        json!({"metadata":{"todos":[{"content":"older", "status":"completed"}]}}),
    ));
    events.extend(result(
        "todo__update_content",
        json!({"metadata":{"todos":[{"id":"3", "content":"newer", "status":"blocked"}]}}),
    ));
    let log = envelopes(events);
    let older = Projection::from_events(&log[..3]).session.todos.unwrap();
    assert_eq!(older[0].id, "todo-0");
    assert_eq!(older[0].status, TodoStatus::Completed);

    let empty = ProjectionReducerRegistry::new();
    let p = fold_from_snapshot(&log, &[], &empty);
    assert_eq!(p, Projection::from_events(&log));
    assert_eq!(p.session.todo_last_id, 3);
    let newer = p.session.todos.unwrap();
    assert_eq!(newer[0].content, "newer");
    assert_eq!(newer[0].status, TodoStatus::Blocked);
}
