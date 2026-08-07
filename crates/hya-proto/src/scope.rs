//! Hierarchy-scoped mailbox addressing: canonical agent paths, the scope rule,
//! and unit-qualified channel keys (task 08-07).
//!
//! # The model
//!
//! A **unit** is one leader plus the agents it directly leads. An agent's
//! canonical handle is its path from the team root:
//!
//! ```text
//! main                     the team root
//! main/lead-1              a child of the root
//! main/lead-1/worker-2     a child of lead-1
//! ```
//!
//! An agent may address only its **parent**, its **same-parent siblings**, and
//! its **direct reports**. Everything else is out of scope; the only cross-unit
//! route is a relay through the common ancestor.
//!
//! # Why this module is pure string arithmetic
//!
//! [`relation`] and [`in_scope`] decide the rule from two path strings alone —
//! no roster walk, no store access, no lineage query. That is what lets the same
//! definition serve all three enforcement sites (the `hya-store` write gate, the
//! `hya-core` read filter, and the `hya-proto` reducer) without any of them
//! re-deriving the rule and drifting from the others.

/// Canonical handle of a team's root agent. Every path starts with this segment.
pub const ROOT_HANDLE: &str = "main";

/// Separator between segments of a canonical agent path.
pub const PATH_SEPARATOR: char = '/';

/// Separator between a unit path and a channel name in a qualified channel key
/// (`main/lead-1#build`).
pub const CHANNEL_SEPARATOR: char = '#';

/// Reserved channel name carrying a unit's one-way announcements.
///
/// Every agent auto-joins its parent's announce channel, so the membership set
/// is exactly the unit's direct children. Only the unit's leader may post to it,
/// which is what makes announce one-way. It is hidden from channel listings and
/// cannot be joined or left explicitly.
pub const ANNOUNCE_CHANNEL: &str = "announce";

/// How `to` stands in relation to `from`, or `None` when they are unrelated.
///
/// The three in-scope relations are exactly the addressable set; [`Relation::Own`]
/// is reported separately because an agent may not mail itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Relation {
    /// `to` and `from` are the same agent.
    Own,
    /// `to` is `from`'s parent.
    Parent,
    /// `to` shares `from`'s parent — a sibling in the same unit.
    Peer,
    /// `to` is a direct child of `from`.
    Report,
}

impl Relation {
    /// Whether an agent may address someone standing in this relation to it.
    ///
    /// [`Relation::Own`] is excluded: self-mail would wake an agent with its own
    /// message, which the resident supervisor already refuses.
    #[must_use]
    pub fn is_addressable(self) -> bool {
        matches!(self, Relation::Parent | Relation::Peer | Relation::Report)
    }
}

/// The parent of a canonical path, or `None` for a root (a path with no
/// separator).
///
/// ```
/// # use hya_proto::scope::parent_path;
/// assert_eq!(parent_path("main/lead-1/worker-2"), Some("main/lead-1"));
/// assert_eq!(parent_path("main"), None);
/// ```
#[must_use]
pub fn parent_path(path: &str) -> Option<&str> {
    path.rsplit_once(PATH_SEPARATOR).map(|(parent, _)| parent)
}

/// The last segment of a canonical path — the agent's short name within its unit.
///
/// ```
/// # use hya_proto::scope::leaf;
/// assert_eq!(leaf("main/lead-1/worker-2"), "worker-2");
/// assert_eq!(leaf("main"), "main");
/// ```
#[must_use]
pub fn leaf(path: &str) -> &str {
    match path.rsplit_once(PATH_SEPARATOR) {
        Some((_, leaf)) => leaf,
        None => path,
    }
}

/// Build a child's canonical path from its parent's path and its own leaf.
#[must_use]
pub fn join_path(parent: &str, leaf: &str) -> String {
    format!("{parent}{PATH_SEPARATOR}{leaf}")
}

/// Number of separators in a path: the root is 0, its children 1, and so on.
#[must_use]
pub fn depth(path: &str) -> usize {
    path.matches(PATH_SEPARATOR).count()
}

/// Whether `leaf` is usable as a path segment.
///
/// Rejects the empty string and anything carrying a structural separator or
/// surrounding whitespace, so a stored path always parses back to the segments
/// it was built from.
#[must_use]
pub fn is_valid_leaf(leaf: &str) -> bool {
    !leaf.is_empty()
        && leaf.trim() == leaf
        && !leaf.contains(PATH_SEPARATOR)
        && !leaf.contains(CHANNEL_SEPARATOR)
}

/// Whether `name` is usable as a channel name (same rules as a path leaf).
#[must_use]
pub fn is_valid_channel_name(name: &str) -> bool {
    is_valid_leaf(name)
}

/// How `to` stands in relation to `from`. `None` means out of scope.
///
/// Pure path arithmetic — see the module docs for why that matters.
///
/// Two roots are never peers. Only one root exists per team log, so this cannot
/// arise in practice; refusing it keeps a malformed log from silently making
/// unrelated agents addressable.
#[must_use]
pub fn relation(from: &str, to: &str) -> Option<Relation> {
    if from == to {
        return Some(Relation::Own);
    }
    let from_parent = parent_path(from);
    if from_parent == Some(to) {
        return Some(Relation::Parent);
    }
    if parent_path(to) == Some(from) {
        return Some(Relation::Report);
    }
    // Both must actually HAVE a parent; `None == None` would make two roots peers.
    match (from_parent, parent_path(to)) {
        (Some(left), Some(right)) if left == right => Some(Relation::Peer),
        _ => None,
    }
}

/// Whether `from` may address `to` under the unit rule.
///
/// This is the single definition of the feature's rule. Every enforcement site
/// calls it rather than re-deriving the comparison.
#[must_use]
pub fn in_scope(from: &str, to: &str) -> bool {
    relation(from, to).is_some_and(Relation::is_addressable)
}

/// The unit an agent belongs to as a member: its parent's unit.
///
/// `None` for the root, which has no home unit — it leads one but belongs to
/// none, exactly like a company's top of the org chart.
#[must_use]
pub fn home_unit(path: &str) -> Option<&str> {
    parent_path(path)
}

/// The unit an agent leads. Identified by the leader's own path.
///
/// Every agent names a led unit; whether that unit has any members is a question
/// for the roster, not for path arithmetic.
#[must_use]
pub fn led_unit(path: &str) -> &str {
    path
}

/// Build a unit-qualified channel key: `main/lead-1` + `build` → `main/lead-1#build`.
#[must_use]
pub fn qualify_channel(unit: &str, name: &str) -> String {
    format!("{unit}{CHANNEL_SEPARATOR}{name}")
}

/// Split a qualified channel key back into `(unit, name)`.
///
/// `None` when the key carries no qualifier, which happens only for a channel
/// name read straight out of a pre-scoping log.
#[must_use]
pub fn split_channel_key(key: &str) -> Option<(&str, &str)> {
    key.rsplit_once(CHANNEL_SEPARATOR)
}

/// The channel name from a qualified key, or the whole key when unqualified.
#[must_use]
pub fn channel_name(key: &str) -> &str {
    split_channel_key(key).map_or(key, |(_, name)| name)
}

/// The owning unit of a qualified channel key, or `None` when unqualified.
#[must_use]
pub fn channel_unit(key: &str) -> Option<&str> {
    split_channel_key(key).map(|(unit, _)| unit)
}

/// Whether a qualified channel key names a unit's reserved announce channel.
#[must_use]
pub fn is_announce_channel(key: &str) -> bool {
    channel_name(key) == ANNOUNCE_CHANNEL
}

/// The qualified key of a unit's reserved announce channel.
#[must_use]
pub fn announce_channel_of(unit: &str) -> String {
    qualify_channel(unit, ANNOUNCE_CHANNEL)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small org used across the scope tests:
    ///
    /// ```text
    /// main
    /// ├── lead-1
    /// │   ├── worker-1
    /// │   └── worker-2
    /// └── lead-2
    ///     └── worker-7
    /// ```
    const ROOT: &str = "main";
    const LEAD_1: &str = "main/lead-1";
    const LEAD_2: &str = "main/lead-2";
    const WORKER_1: &str = "main/lead-1/worker-1";
    const WORKER_2: &str = "main/lead-1/worker-2";
    const WORKER_7: &str = "main/lead-2/worker-7";

    #[test]
    fn parent_of_root_is_none_and_leaf_is_itself() {
        assert_eq!(parent_path(ROOT), None);
        assert_eq!(leaf(ROOT), ROOT);
        assert_eq!(depth(ROOT), 0);
        assert_eq!(home_unit(ROOT), None, "the root belongs to no unit");
        assert_eq!(led_unit(ROOT), ROOT, "but it leads its own");
    }

    #[test]
    fn path_parts_round_trip() {
        assert_eq!(join_path(LEAD_1, "worker-2"), WORKER_2);
        assert_eq!(parent_path(WORKER_2), Some(LEAD_1));
        assert_eq!(leaf(WORKER_2), "worker-2");
        assert_eq!(depth(WORKER_2), 2);
    }

    #[test]
    fn an_agent_may_not_address_itself() {
        assert_eq!(relation(WORKER_2, WORKER_2), Some(Relation::Own));
        assert!(!Relation::Own.is_addressable());
        assert!(!in_scope(WORKER_2, WORKER_2));
    }

    #[test]
    fn parent_sibling_and_report_are_in_scope() {
        assert_eq!(relation(WORKER_2, LEAD_1), Some(Relation::Parent));
        assert_eq!(relation(WORKER_2, WORKER_1), Some(Relation::Peer));
        assert_eq!(relation(LEAD_1, WORKER_2), Some(Relation::Report));
        for (from, to) in [(WORKER_2, LEAD_1), (WORKER_2, WORKER_1), (LEAD_1, WORKER_2)] {
            assert!(in_scope(from, to), "{from} -> {to}");
        }
    }

    /// The four ways to be out of scope. Each of these is a path the flat
    /// mailbox allowed and this design closes.
    #[test]
    fn everything_outside_the_unit_is_out_of_scope() {
        // Grandparent: skip-level is closed.
        assert_eq!(relation(WORKER_2, ROOT), None);
        // Grandchild: a leader does not reach past its direct reports.
        assert_eq!(relation(ROOT, WORKER_2), None);
        // Uncle: the parent's sibling.
        assert_eq!(relation(WORKER_2, LEAD_2), None);
        // Cousin: another unit's worker — the case that made large swarms a mess.
        assert_eq!(relation(WORKER_2, WORKER_7), None);
        for (from, to) in [
            (WORKER_2, ROOT),
            (ROOT, WORKER_2),
            (WORKER_2, LEAD_2),
            (WORKER_2, WORKER_7),
        ] {
            assert!(!in_scope(from, to), "{from} -> {to} must be refused");
        }
    }

    #[test]
    fn scope_is_symmetric_for_peers_and_paired_for_parent_report() {
        // Peers see each other identically.
        assert!(in_scope(WORKER_1, WORKER_2) && in_scope(WORKER_2, WORKER_1));
        // A parent/child pair is mutually addressable, by two different relations.
        assert_eq!(relation(LEAD_1, WORKER_1), Some(Relation::Report));
        assert_eq!(relation(WORKER_1, LEAD_1), Some(Relation::Parent));
    }

    /// Two roots must not be peers. `parent_path` returns `None` for both, and a
    /// naive equality check would make every root addressable from every other.
    #[test]
    fn two_roots_are_not_peers() {
        assert_eq!(relation("main", "other"), None);
        assert!(!in_scope("main", "other"));
    }

    /// A leaf that carries a separator would make a stored path ambiguous.
    #[test]
    fn leaf_validation_rejects_separators_and_padding() {
        assert!(is_valid_leaf("worker-1"));
        assert!(is_valid_leaf("reviewer_2"));
        assert!(!is_valid_leaf(""), "empty");
        assert!(!is_valid_leaf("a/b"), "path separator");
        assert!(!is_valid_leaf("a#b"), "channel separator");
        assert!(!is_valid_leaf(" a"), "leading space");
        assert!(!is_valid_leaf("a "), "trailing space");
    }

    #[test]
    fn channel_keys_qualify_and_split() {
        let key = qualify_channel(LEAD_1, "build");
        assert_eq!(key, "main/lead-1#build");
        assert_eq!(split_channel_key(&key), Some((LEAD_1, "build")));
        assert_eq!(channel_unit(&key), Some(LEAD_1));
        assert_eq!(channel_name(&key), "build");
    }

    /// The same channel name in two units is two different channels. This is the
    /// whole point of qualifying the key.
    #[test]
    fn same_name_in_two_units_is_two_channels() {
        let left = qualify_channel(LEAD_1, "build");
        let right = qualify_channel(LEAD_2, "build");
        assert_ne!(left, right);
        assert_eq!(channel_name(&left), channel_name(&right));
        assert_ne!(channel_unit(&left), channel_unit(&right));
    }

    /// An unqualified key is what a pre-scoping log contains; it must not be
    /// mistaken for a qualified one.
    #[test]
    fn unqualified_legacy_key_reports_no_unit() {
        assert_eq!(split_channel_key("build"), None);
        assert_eq!(channel_unit("build"), None);
        assert_eq!(channel_name("build"), "build", "the whole key is the name");
    }

    #[test]
    fn announce_channel_is_recognized_per_unit() {
        let announce = announce_channel_of(LEAD_1);
        assert_eq!(announce, "main/lead-1#announce");
        assert!(is_announce_channel(&announce));
        assert!(!is_announce_channel(&qualify_channel(LEAD_1, "build")));
        // Recognized independently of which unit owns it.
        assert!(is_announce_channel(&announce_channel_of(ROOT)));
        assert!(is_announce_channel(&announce_channel_of(WORKER_7)));
    }
}
