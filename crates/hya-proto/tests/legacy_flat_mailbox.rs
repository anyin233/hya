//! Back-compat oracle for the scoped-mailbox change (task 08-07, AC8).
//!
//! `fixtures/legacy_flat_mailbox.json` is an event log in the **pre-scoping**
//! wire form: `AgentRegistered` carries no `parent`, handles are bare names, and
//! channels are bare names. It was captured against the flat-mailbox build and
//! must never be edited to accommodate a code change — that would destroy the
//! oracle.
//!
//! Scoping re-keys the projection's maps from bare names to canonical paths
//! (`reviewer-1` → `main/reviewer-1`, channel `build` → `main#build`). Map
//! equality is therefore the wrong assertion. What must hold across the change
//! is the **topology and every delivery outcome**: the same agents, the same
//! inbox contents in the same order, and one flat unit in which every agent may
//! address every other. These tests assert exactly that, addressing the
//! projection by *leaf* name so they read identically before and after.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{Envelope, Event, EventSeq, MailKind, Projection, RosterStatus, SubagentMode};

const FIXTURE: &str = include_str!("fixtures/legacy_flat_mailbox.json");

/// Fold the legacy fixture exactly as store replay would.
fn legacy_projection() -> Projection {
    let events: Vec<Event> = serde_json::from_str(FIXTURE).expect("legacy fixture parses");
    assert_eq!(events.len(), 8, "fixture shape is pinned");
    assert!(
        !events.iter().any(|event| matches!(event, Event::Unknown)),
        "every fixture event must be a known variant; an Unknown here means a \
         variant was renamed and the oracle silently stopped testing anything"
    );
    let envelopes: Vec<Envelope> = events
        .into_iter()
        .enumerate()
        .map(|(index, event)| Envelope {
            seq: EventSeq(index as u64 + 1),
            ts_millis: 1_700_000_000_000 + index as i64,
            event,
        })
        .collect();
    Projection::from_events(&envelopes)
}

/// The last `/`-separated segment of a canonical path. Before scoping every key
/// already *is* its own leaf, so this is the identity; after scoping it strips
/// the `main/` prefix. Either way the assertions below read the same.
fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Look a roster entry up by leaf, asserting the leaf is unambiguous.
fn roster_by_leaf<'a>(projection: &'a Projection, name: &str) -> &'a hya_proto::RosterEntry {
    let mut found = projection
        .team
        .roster
        .iter()
        .filter(|(key, _)| leaf(key) == name);
    let entry = found
        .next()
        .unwrap_or_else(|| panic!("no roster leaf `{name}`"));
    assert!(
        found.next().is_none(),
        "leaf `{name}` is ambiguous in the roster"
    );
    entry.1
}

/// Inbox bodies for a leaf, in delivery order. Absent inbox reads as empty.
fn inbox_bodies(projection: &Projection, name: &str) -> Vec<String> {
    projection
        .team
        .inboxes
        .iter()
        .filter(|(key, _)| leaf(key) == name)
        .flat_map(|(_, inbox)| inbox.iter().map(|message| message.body.clone()))
        .collect()
}

/// The one channel whose name (after any unit qualifier) is `name`.
fn channel_by_name<'a>(projection: &'a Projection, name: &str) -> &'a hya_proto::ChannelProjection {
    let mut found = projection
        .team
        .channels
        .iter()
        .filter(|(key, _)| key.rsplit('#').next().unwrap_or(key) == name);
    let channel = found
        .next()
        .unwrap_or_else(|| panic!("no channel named `{name}`"));
    assert!(
        found.next().is_none(),
        "channel `{name}` is ambiguous — more than one unit owns it"
    );
    channel.1
}

/// The roster is exactly the three agents the fixture registered, with their
/// declared type, mode, and own session preserved.
#[test]
fn legacy_log_replays_the_same_three_agents() {
    let projection = legacy_projection();
    assert_eq!(projection.team.roster.len(), 3, "no agent gained or lost");

    let main = roster_by_leaf(&projection, "main");
    assert_eq!(main.agent_type.as_str(), "build");
    assert_eq!(main.mode, SubagentMode::Transient);
    assert_eq!(main.status, RosterStatus::Idle);

    for name in ["reviewer-1", "reviewer-2"] {
        let entry = roster_by_leaf(&projection, name);
        assert_eq!(entry.agent_type.as_str(), "reviewer", "{name} type");
        assert_eq!(entry.mode, SubagentMode::Resident, "{name} mode");
    }

    // The root and the two members are distinct sessions, and the root's roster
    // entry is the one whose session is the log's own session.
    let root_session = roster_by_leaf(&projection, "main").session;
    assert_ne!(
        root_session,
        roster_by_leaf(&projection, "reviewer-1").session
    );
    assert_ne!(
        root_session,
        roster_by_leaf(&projection, "reviewer-2").session
    );
}

/// Every delivery outcome the legacy log produced, in order. This is the heart
/// of AC8: scoping must not add, drop, or reorder a single message.
#[test]
fn legacy_log_replays_identical_deliveries() {
    let projection = legacy_projection();

    // reviewer-1: the direct mail from main, then the #build announcement.
    assert_eq!(
        inbox_bodies(&projection, "reviewer-1"),
        vec!["direct hello".to_string(), "ship it".to_string()],
    );
    // reviewer-2: the #build announcement, then the sibling's direct mail.
    assert_eq!(
        inbox_bodies(&projection, "reviewer-2"),
        vec!["ship it".to_string(), "sibling ping".to_string()],
    );
    // main subscribed to nothing and was addressed by nobody.
    assert_eq!(inbox_bodies(&projection, "main"), Vec::<String>::new());
}

/// Channel membership and log survive, and the channel keeps exactly one post.
#[test]
fn legacy_log_replays_the_build_channel() {
    let projection = legacy_projection();
    let build = channel_by_name(&projection, "build");

    let members: Vec<&str> = build.members.iter().map(|m| leaf(m)).collect();
    assert_eq!(members, vec!["reviewer-1", "reviewer-2"]);

    assert_eq!(build.log.len(), 1, "one post to #build");
    assert_eq!(build.log[0].body, "ship it");
    assert_eq!(build.log[0].kind, MailKind::Announcement);
    assert_eq!(leaf(&build.log[0].from), "main");
}

/// The fold invents nothing: exactly the one channel the fixture created.
#[test]
fn legacy_log_synthesizes_no_extra_channel() {
    let projection = legacy_projection();
    assert_eq!(
        projection.team.channels.len(),
        1,
        "only the fixture's #build; an extra channel means the fold synthesized \
         something a legacy log never contained"
    );
}

/// A legacy log has no hierarchy, so scoping must fold it to ONE flat unit:
/// every member a DIRECT child of the root, hence every agent a sibling of every
/// other, hence mutually addressable. This is the property that keeps a
/// pre-existing swarm working unchanged (AC8).
#[test]
fn legacy_log_is_one_flat_unit() {
    let projection = legacy_projection();

    let depth = |path: &str| path.matches('/').count();
    let root_depth = depth(roster_by_leaf(&projection, "main").handle.as_str());
    for name in ["reviewer-1", "reviewer-2"] {
        assert_eq!(
            depth(roster_by_leaf(&projection, name).handle.as_str()),
            root_depth + 1,
            "{name} must be a DIRECT child of the root — a legacy log has no \
             deeper nesting to recover, so anything else means the fallback \
             invented a hierarchy that was never in the log"
        );
    }
}

/// The consequence that actually matters for a pre-existing swarm: under the new
/// scope rule, every agent in a legacy log can still address every other. If any
/// pair fell out of scope, upgrading would silently sever a working swarm's
/// communication (AC8).
#[test]
fn every_legacy_pair_stays_mutually_addressable() {
    let projection = legacy_projection();
    let paths: Vec<&str> = projection.team.roster.keys().map(String::as_str).collect();
    assert_eq!(paths.len(), 3);

    for from in &paths {
        for to in &paths {
            if from == to {
                continue;
            }
            assert!(
                hya_proto::in_scope(from, to),
                "{from} must still be able to address {to} after the upgrade"
            );
        }
    }
}
