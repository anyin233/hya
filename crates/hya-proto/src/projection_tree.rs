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
use crate::projection::{MemberProjection, SessionProjection};

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
) -> RunTreeNode {
    let mut visited = HashSet::new();
    build_node(root, None, lookup, &mut visited)
}

fn build_node(
    session: SessionId,
    member: Option<&MemberProjection>,
    lookup: &HashMap<SessionId, SessionProjection>,
    visited: &mut HashSet<SessionId>,
) -> RunTreeNode {
    let proj = lookup.get(&session);
    let mut node = RunTreeNode {
        session: Some(session),
        agent: proj.and_then(|p| p.agent.clone()),
        model: proj.and_then(|p| p.model.clone()),
        title: proj.and_then(|p| p.title.clone()),
        member: member.cloned(),
        children: Vec::new(),
    };
    if !visited.insert(session) {
        return node; // cycle guard: do not recurse into an already-seen session
    }
    if let Some(proj) = proj {
        for m in &proj.members {
            match m.child {
                Some(child) => node
                    .children
                    .push(build_node(child, Some(m), lookup, visited)),
                None => node.children.push(RunTreeNode {
                    session: None,
                    agent: None,
                    model: None,
                    title: None,
                    member: Some(m.clone()),
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
    use crate::message::MemberRunStatus;

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

        let tree = build_run_tree(root, &lookup);
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
        let tree = build_run_tree(a, &lookup);
        assert_eq!(tree.session, Some(a));
    }
}
