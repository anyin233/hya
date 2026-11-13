//! Canonical agent paths.

/// Canonical handle of a team's root agent. Every path starts with this segment.
pub const ROOT_HANDLE: &str = "main";

/// Sender of harness-authored team mail (for example the leader-failed
/// wrap-up notice). Not an agent: it never appears on the roster, is not a
/// path, and cannot be addressed; only the harness writes it.
pub const HARNESS_HANDLE: &str = "harness";

/// Separator between segments of a canonical agent path.
pub const PATH_SEPARATOR: char = '/';

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
    !leaf.is_empty() && leaf.trim() == leaf && !leaf.contains(PATH_SEPARATOR) && !leaf.contains('#')
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

    #[test]
    fn parent_of_root_is_none_and_leaf_is_itself() {
        assert_eq!(parent_path(ROOT), None);
        assert_eq!(leaf(ROOT), ROOT);
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
}
