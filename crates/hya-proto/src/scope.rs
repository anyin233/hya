//! Canonical agent paths and unit-qualified channel keys.

/// Canonical handle of a team's root agent. Every path starts with this segment.
pub const ROOT_HANDLE: &str = "main";

/// Sender of harness-authored team mail (for example the leader-failed
/// wrap-up notice). Not an agent: it never appears on the roster, is not a
/// path, and cannot be addressed; only the harness writes it.
pub const HARNESS_HANDLE: &str = "harness";

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
/// `None` for an unqualified key, including minted channel ids.
#[must_use]
pub fn split_channel_key(key: &str) -> Option<(&str, &str)> {
    key.rsplit_once(CHANNEL_SEPARATOR)
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
    const WORKER_2: &str = "main/lead-1/worker-2";
    const WORKER_7: &str = "main/lead-2/worker-7";

    #[test]
    fn parent_of_root_is_none_and_leaf_is_itself() {
        assert_eq!(parent_path(ROOT), None);
        assert_eq!(leaf(ROOT), ROOT);
        assert_eq!(home_unit(ROOT), None, "the root belongs to no unit");
        assert_eq!(led_unit(ROOT), ROOT, "but it leads its own");
    }

    #[test]
    fn path_parts_round_trip() {
        assert_eq!(join_path(LEAD_1, "worker-2"), WORKER_2);
        assert_eq!(parent_path(WORKER_2), Some(LEAD_1));
        assert_eq!(leaf(WORKER_2), "worker-2");
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
    fn announce_channel_is_qualified_per_unit() {
        assert_eq!(announce_channel_of(LEAD_1), "main/lead-1#announce");
        assert_eq!(announce_channel_of(ROOT), "main#announce");
        assert_eq!(
            announce_channel_of(WORKER_7),
            "main/lead-2/worker-7#announce"
        );
    }
}
