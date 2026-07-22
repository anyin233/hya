//! Pure assembler for the recursive subagent run tree.
//!
//! The tree is a deterministic function of already-reduced [`SessionProjection`]s:
//! it joins each session's `members[].child` links recursively. It performs no I/O
//! and holds no mutable state of its own, so server and TUI can both build it from
//! projections they already have without a separate read model that could drift.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ids::SessionId;
use crate::model::{AgentName, ModelRef};
use crate::projection::{MemberProjection, RosterEntry, SessionProjection};

/// One node of the run tree: a session plus the member metadata that spawned it
/// (`member` is `None` for the root) and its child members.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunTreeNode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster: Option<RosterEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<RunTreeNode>,
}

/// Build the run tree rooted at `root` by joining member/child links across the
/// supplied session projections. Sessions absent from `lookup` become leaves; a
/// cycle (should never happen) is broken by a visited set.
#[must_use]
pub fn build_run_tree(
    root: SessionId,
    lookup: &HashMap<SessionId, SessionProjection>,
    roster: &HashMap<SessionId, RosterEntry>,
) -> RunTreeNode {
    let mut visited = HashSet::new();
    build_node(root, None, lookup, roster, &mut visited)
}

fn build_node(
    session: SessionId,
    member: Option<&MemberProjection>,
    lookup: &HashMap<SessionId, SessionProjection>,
    roster: &HashMap<SessionId, RosterEntry>,
    visited: &mut HashSet<SessionId>,
) -> RunTreeNode {
    let proj = lookup.get(&session);
    let mut node = RunTreeNode {
        session: Some(session),
        agent: proj.and_then(|p| p.agent.clone()),
        model: proj.and_then(|p| p.model.clone()),
        title: proj.and_then(|p| p.title.clone()),
        member: member.cloned(),
        roster: member.and_then(|_| roster.get(&session).cloned()),
        children: Vec::new(),
    };
    if !visited.insert(session) {
        return node; // cycle guard: do not recurse into an already-seen session
    }
    if let Some(proj) = proj {
        for m in &proj.members {
            match m.child {
                Some(child) => {
                    // Collapse historical duplicates: a resume used to emit a
                    // second MemberSpawned for the same child session, which made
                    // the TUI roster show (and multi-select) two rows for one agent.
                    // Later member state wins.
                    let child_node = build_node(child, Some(m), lookup, roster, visited);
                    if let Some(idx) = node
                        .children
                        .iter()
                        .position(|existing| existing.session == Some(child))
                    {
                        node.children[idx] = child_node;
                    } else {
                        node.children.push(child_node);
                    }
                }
                None => node.children.push(RunTreeNode {
                    session: None,
                    agent: None,
                    model: None,
                    title: None,
                    member: Some(m.clone()),
                    roster: None,
                    children: Vec::new(),
                }),
            }
        }
    }
    node
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::ids::MemberId;
    use crate::message::{MemberRunStatus, RosterStatus, SubagentMode};
    use crate::projection::RosterEntry;

    fn proj_with_members(
        id: SessionId,
        agent: &str,
        members: Vec<MemberProjection>,
    ) -> SessionProjection {
        SessionProjection {
            id: Some(id),
            agent: Some(AgentName::new(agent)),
            members,
            ..SessionProjection::default()
        }
    }

    fn member(child: SessionId, kind: &str) -> MemberProjection {
        MemberProjection {
            member: MemberId::new(),
            child: Some(child),
            subagent_type: AgentName::new(kind),
            description: "d".to_string(),
            depth: 1,
            status: MemberRunStatus::Done,
            summary: "s".to_string(),
        }
    }

    #[test]
    fn builds_nested_tree_across_sessions() {
        let root = SessionId::new();
        let child_a = SessionId::new();
        let child_b = SessionId::new();
        let grandchild = SessionId::new();

        let mut lookup = HashMap::new();
        lookup.insert(
            root,
            proj_with_members(
                root,
                "build",
                vec![member(child_a, "explore"), member(child_b, "plan")],
            ),
        );
        // child_a spawned a grandchild; child_b is a leaf.
        lookup.insert(
            child_a,
            proj_with_members(child_a, "explore", vec![member(grandchild, "explore")]),
        );
        lookup.insert(child_b, proj_with_members(child_b, "plan", vec![]));
        lookup.insert(grandchild, proj_with_members(grandchild, "explore", vec![]));

        let tree = build_run_tree(root, &lookup, &HashMap::new());
        assert_eq!(tree.session, Some(root));
        assert!(tree.member.is_none(), "root is not a member");
        assert_eq!(tree.children.len(), 2);
        let a = tree
            .children
            .iter()
            .find(|n| n.session == Some(child_a))
            .unwrap();
        assert_eq!(a.children.len(), 1, "child_a has a grandchild");
        assert_eq!(a.children[0].session, Some(grandchild));
        assert_eq!(
            a.member.as_ref().unwrap().subagent_type,
            AgentName::new("explore")
        );
    }

    #[test]
    fn cycle_is_broken() {
        let a = SessionId::new();
        let b = SessionId::new();
        let mut lookup = HashMap::new();
        lookup.insert(a, proj_with_members(a, "x", vec![member(b, "y")]));
        lookup.insert(b, proj_with_members(b, "y", vec![member(a, "x")]));
        // Must terminate despite the a→b→a cycle.
        let tree = build_run_tree(a, &lookup, &HashMap::new());
        assert_eq!(tree.session, Some(a));
    }

    #[test]
    fn attaches_roster_metadata_by_session() {
        let root = SessionId::new();
        let child = SessionId::new();
        let mut pending = member(SessionId::new(), "plan");
        pending.child = None;

        let mut lookup = HashMap::new();
        lookup.insert(
            root,
            proj_with_members(root, "build", vec![member(child, "explore"), pending]),
        );
        lookup.insert(child, proj_with_members(child, "explore", vec![]));

        let entry = RosterEntry {
            handle: "explorer-1".to_string(),
            session: child,
            agent_type: AgentName::new("explore"),
            mode: SubagentMode::Transient,
            status: RosterStatus::Busy,
            current_task: Some("inspect tree".to_string()),
        };
        let roster = HashMap::from([(child, entry.clone())]);

        let tree = build_run_tree(root, &lookup, &roster);
        assert_eq!(tree.children[0].roster, Some(entry));
        assert!(tree.roster.is_none());
        assert!(tree.children[1].roster.is_none());

        let root_json = serde_json::to_value(&tree).expect("serialize root");
        let child_json = serde_json::to_value(&tree.children[0]).expect("serialize child");
        let pending_json = serde_json::to_value(&tree.children[1]).expect("serialize pending");
        assert!(root_json.get("roster").is_none());
        assert!(child_json.get("roster").is_some());
        assert!(pending_json.get("roster").is_none());
    }

    /// Historical logs may contain two MemberSpawned rows for the same child
    /// (pre-fix resume path). The tree must surface one observation node so the
    /// roster cannot multi-highlight two rows that share a session id.
    #[test]
    fn collapses_duplicate_members_for_same_child_session() {
        let root = SessionId::new();
        let child = SessionId::new();
        let mut first = member(child, "explore");
        first.status = MemberRunStatus::Failed;
        first.summary = "first attempt".to_string();
        let mut second = member(child, "explore");
        second.status = MemberRunStatus::Running;
        second.summary = "restarted".to_string();

        let mut lookup = HashMap::new();
        lookup.insert(
            root,
            proj_with_members(root, "build", vec![first, second.clone()]),
        );
        lookup.insert(child, proj_with_members(child, "explore", vec![]));

        let tree = build_run_tree(root, &lookup, &HashMap::new());
        assert_eq!(
            tree.children.len(),
            1,
            "same child session must appear once in the run tree"
        );
        assert_eq!(tree.children[0].session, Some(child));
        assert_eq!(
            tree.children[0].member.as_ref().map(|m| m.status),
            Some(MemberRunStatus::Running),
            "later member state wins when collapsing duplicates"
        );
        assert_eq!(
            tree.children[0].member.as_ref().map(|m| m.member),
            Some(second.member)
        );
    }
}
