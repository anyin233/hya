//! Conformance: `hya_sdk::TeamProjection` must fold an event stream to the same
//! team state as the backend's `hya_proto::Projection` (task 08-07, AC10).
//!
//! `hya-sdk` deliberately does not depend on `hya-proto` at runtime — the TUI
//! client stays dependency-light, so `team.rs` is a hand-written mirror rather
//! than a shared type. A hand-written mirror drifts unless something forces the
//! two to agree. This is that something.
//!
//! Both sides fold the SAME JSON envelopes: the backend deserializes them into
//! `Event`, the mirror reads the raw `Value`. Anything the two disagree about —
//! a canonical path derived differently, a channel key qualified differently, a
//! fan-out rule applied on one side only — surfaces here as a mismatch.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use hya_sdk::team::TeamProjection as MirrorProjection;
use serde_json::{json, Value};

/// Fold `events` through the backend reducer.
fn backend(events: &[Value]) -> hya_proto::TeamProjection {
    let envelopes: Vec<hya_proto::Envelope> = events
        .iter()
        .enumerate()
        .map(|(index, value)| hya_proto::Envelope {
            seq: hya_proto::EventSeq(index as u64 + 1),
            ts_millis: 1_700_000_000_000 + index as i64,
            event: serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("event {index} must deserialize: {e}\n{value}")),
        })
        .collect();
    hya_proto::Projection::from_events(&envelopes).team
}

/// Fold the same `events` through the TUI mirror.
fn mirror(events: &[Value]) -> MirrorProjection {
    let mut projection = MirrorProjection::default();
    for (index, value) in events.iter().enumerate() {
        assert!(
            projection.apply_event(value),
            "the mirror must recognize event {index}: {value}"
        );
    }
    projection
}

/// Compare the two folds on everything the mirror models. The mirror carries
/// mode/status as wire strings, so both sides are reduced to a comparable shape
/// rather than compared structurally.
fn assert_agree(events: &[Value]) {
    let backend = backend(events);
    let mirror = mirror(events);

    // --- roster: same keys, same bindings ---
    let backend_roster: BTreeMap<&str, (String, String, String, String)> = backend
        .roster
        .iter()
        .map(|(path, entry)| {
            (
                path.as_str(),
                (
                    entry.handle.clone(),
                    entry.session.to_string(),
                    entry.agent_type.as_str().to_string(),
                    format!("{:?}", entry.mode).to_lowercase(),
                ),
            )
        })
        .collect();
    let mirror_roster: BTreeMap<&str, (String, String, String, String)> = mirror
        .roster
        .iter()
        .map(|(path, entry)| {
            (
                path.as_str(),
                (
                    entry.handle.clone(),
                    entry.session.clone(),
                    entry.agent_type.clone(),
                    entry.mode.clone(),
                ),
            )
        })
        .collect();
    assert_eq!(
        backend_roster.keys().collect::<Vec<_>>(),
        mirror_roster.keys().collect::<Vec<_>>(),
        "roster must be keyed by the same canonical paths"
    );
    assert_eq!(backend_roster, mirror_roster, "roster bindings must agree");

    // --- inboxes: same keys, same bodies, same order ---
    let backend_inboxes: BTreeMap<&str, Vec<&str>> = backend
        .inboxes
        .iter()
        .map(|(path, inbox)| {
            (
                path.as_str(),
                inbox.iter().map(|m| m.body.as_str()).collect(),
            )
        })
        .collect();
    let mirror_inboxes: BTreeMap<&str, Vec<&str>> = mirror
        .inboxes
        .iter()
        .map(|(path, inbox)| {
            (
                path.as_str(),
                inbox.iter().map(|m| m.body.as_str()).collect(),
            )
        })
        .collect();
    assert_eq!(
        backend_inboxes, mirror_inboxes,
        "every inbox must hold the same messages, in the same order, under the \
         same canonical key"
    );

    // --- channels: same qualified keys, same members, same log ---
    let backend_channels: BTreeMap<&str, (BTreeSet<&str>, Vec<&str>)> = backend
        .channels
        .iter()
        .map(|(key, channel)| {
            (
                key.as_str(),
                (
                    channel.members.iter().map(String::as_str).collect(),
                    channel.log.iter().map(|m| m.body.as_str()).collect(),
                ),
            )
        })
        .collect();
    let mirror_channels: BTreeMap<&str, (BTreeSet<&str>, Vec<&str>)> = mirror
        .channels
        .iter()
        .map(|(key, channel)| {
            (
                key.as_str(),
                (
                    channel.members.iter().map(String::as_str).collect(),
                    channel.log.iter().map(|m| m.body.as_str()).collect(),
                ),
            )
        })
        .collect();
    assert_eq!(
        backend_channels, mirror_channels,
        "channels must agree on unit-qualified keys, membership, and log"
    );
}

fn registered(session: &str, agent_session: &str, handle: &str, parent: Option<&str>) -> Value {
    let mut event = json!({
        "type": "agent_registered",
        "session": session,
        "agent_session": agent_session,
        "handle": handle,
        "agent_type": "worker",
        "mode": "resident",
    });
    if let Some(parent) = parent {
        event["parent"] = json!(parent);
    }
    event
}

const ROOT: &str = "hysec_MirrorConformRoot001";

/// A post-scoping stream: real parents, canonical addresses, qualified channels.
#[test]
fn mirror_agrees_on_a_scoped_two_unit_team() {
    let events = vec![
        registered(ROOT, ROOT, "main", None),
        registered(ROOT, "hysec_MirrorConformLead001", "lead-1", Some("main")),
        registered(ROOT, "hysec_MirrorConformLead002", "lead-2", Some("main")),
        registered(
            ROOT,
            "hysec_MirrorConformWork001",
            "worker-1",
            Some("main/lead-1"),
        ),
        registered(
            ROOT,
            "hysec_MirrorConformWork007",
            "worker-1",
            Some("main/lead-2"),
        ),
        // Each unit owns a #build; the same name, two different channels.
        json!({"type": "channel_joined", "session": ROOT,
               "channel": "main/lead-1#build", "member": "main/lead-1/worker-1"}),
        json!({"type": "channel_joined", "session": ROOT,
               "channel": "main/lead-2#build", "member": "main/lead-2/worker-1"}),
        json!({"type": "mail_sent", "session": ROOT, "from": "main/lead-1",
               "to": {"kind": "channel", "id": "main/lead-1#build"},
               "kind": "message", "body": "unit one only"}),
        // A direct message between same-parent siblings.
        json!({"type": "mail_sent", "session": ROOT, "from": "main/lead-1",
               "to": {"kind": "handle", "id": "main/lead-1/worker-1"},
               "kind": "message", "body": "direct"}),
        // The reserved announce channel, auto-joined at registration.
        json!({"type": "channel_joined", "session": ROOT,
               "channel": "main#announce", "member": "main/lead-1"}),
        json!({"type": "channel_joined", "session": ROOT,
               "channel": "main#announce", "member": "main/lead-2"}),
        json!({"type": "mail_sent", "session": ROOT, "from": "main",
               "to": {"kind": "channel", "id": "main#announce"},
               "kind": "announcement", "body": "all hands"}),
        json!({"type": "agent_activity_changed", "session": ROOT,
               "handle": "main/lead-1", "status": "busy",
               "current_task": "coordinating"}),
        json!({"type": "channel_left", "session": ROOT,
               "channel": "main/lead-1#build", "member": "main/lead-1/worker-1"}),
    ];
    assert_agree(&events);
}

/// A pre-scoping stream: no `parent`, bare handles, bare channel names. Both
/// sides must apply the SAME legacy fallback, or an upgraded TUI would render a
/// different team than the backend replayed.
#[test]
fn mirror_agrees_on_a_legacy_flat_team() {
    let events = vec![
        registered(ROOT, ROOT, "main", None),
        registered(ROOT, "hysec_MirrorConformKid0001", "reviewer-1", None),
        registered(ROOT, "hysec_MirrorConformKid0002", "reviewer-2", None),
        json!({"type": "channel_joined", "session": ROOT,
               "channel": "build", "member": "reviewer-1"}),
        json!({"type": "channel_joined", "session": ROOT,
               "channel": "build", "member": "reviewer-2"}),
        json!({"type": "mail_sent", "session": ROOT, "from": "main",
               "to": {"kind": "channel", "id": "build"},
               "kind": "announcement", "body": "ship it"}),
        json!({"type": "mail_sent", "session": ROOT, "from": "reviewer-1",
               "to": {"kind": "handle", "id": "reviewer-2"},
               "kind": "message", "body": "sibling ping"}),
        json!({"type": "agent_activity_changed", "session": ROOT,
               "handle": "reviewer-1", "status": "busy"}),
    ];
    assert_agree(&events);

    // And the fallback actually re-keyed things, rather than both sides simply
    // leaving the bare names alone.
    let folded = mirror(&events);
    assert!(folded.roster.contains_key("main/reviewer-1"));
    assert!(folded.channels.contains_key("main#build"));
    assert!(!folded.channels.contains_key("build"));
}

/// Channel fan-out skips a resident that has reached a terminal status, so a
/// stopped actor's inbox stops growing (ADR-0001). Both folds must apply that
/// rule, or the TUI would show a stopped agent still receiving mail.
#[test]
fn mirror_agrees_on_fanout_past_a_stopped_resident() {
    let events = vec![
        registered(ROOT, ROOT, "main", None),
        registered(
            ROOT,
            "hysec_MirrorConformStop001",
            "stopped-1",
            Some("main"),
        ),
        registered(ROOT, "hysec_MirrorConformLive001", "active-1", Some("main")),
        json!({"type": "channel_joined", "session": ROOT,
               "channel": "main#build", "member": "main/stopped-1"}),
        json!({"type": "channel_joined", "session": ROOT,
               "channel": "main#build", "member": "main/active-1"}),
        // stopped-1 reaches a terminal status BEFORE the post.
        json!({"type": "agent_activity_changed", "session": ROOT,
               "handle": "main/stopped-1", "status": "failed",
               "current_task": "resident stopped"}),
        json!({"type": "mail_sent", "session": ROOT, "from": "main",
               "to": {"kind": "channel", "id": "main#build"},
               "kind": "announcement", "body": "after stop"}),
    ];
    assert_agree(&events);

    let folded = backend(&events);
    assert!(
        !folded.inboxes.contains_key("main/stopped-1"),
        "a terminal resident's inbox must not grow"
    );
    assert_eq!(
        folded
            .inboxes
            .get("main/active-1")
            .map(|inbox| inbox.len())
            .unwrap_or_default(),
        1,
        "the live subscriber still receives it"
    );
}

/// Mail folded BEFORE its recipient registers must still land under the key that
/// registration will later produce — on both sides.
#[test]
fn mirror_agrees_when_mail_precedes_registration() {
    let events = vec![
        registered(ROOT, ROOT, "main", None),
        json!({"type": "mail_sent", "session": ROOT, "from": "main",
               "to": {"kind": "handle", "id": "main/lead-1/worker-1"},
               "kind": "message", "body": "early"}),
        registered(ROOT, "hysec_MirrorConformLead001", "lead-1", Some("main")),
        registered(
            ROOT,
            "hysec_MirrorConformWork001",
            "worker-1",
            Some("main/lead-1"),
        ),
        json!({"type": "mail_sent", "session": ROOT, "from": "main/lead-1",
               "to": {"kind": "handle", "id": "main/lead-1/worker-1"},
               "kind": "message", "body": "late"}),
    ];
    assert_agree(&events);

    let folded = backend(&events);
    let inbox = folded
        .inboxes
        .get("main/lead-1/worker-1")
        .expect("both messages share one inbox key");
    assert_eq!(
        inbox.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
        vec!["early", "late"],
        "fold order must not split one agent's inbox across two keys"
    );
}
