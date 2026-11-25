use hya_proto::SessionId;

use crate::engine::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
    /// `search_agent` rows (ADR-0015 §7): the caller's own archived DIRECT
    /// children, digested from each child's latest handoff. The archive index
    /// is derived from the logs (archived map + per-child handoff projection).
    pub(crate) async fn search_archived_agents(
        &self,
        session: SessionId,
        query: &str,
    ) -> Result<Vec<hya_tool::ArchivedAgentRow>, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        let projection = self.read_projection_shared(root).await?;
        let needle = query.trim().to_ascii_lowercase();
        let mut rows = Vec::new();
        for (path, entry) in &projection.team.archived {
            if hya_proto::scope::parent_path(path) != Some(handle.as_str()) {
                continue;
            }
            let child_projection = self.read_projection_shared(entry.session).await?;
            let handoff = child_projection.session.handoff.clone();
            let (goal, pending, degraded) = match &handoff {
                Some(handoff) => (
                    handoff_section(&handoff.doc, "Goal"),
                    handoff_section(&handoff.doc, "Pending tasks"),
                    handoff.degraded,
                ),
                None => ("(no handoff)".to_string(), String::new(), true),
            };
            if !needle.is_empty() {
                let haystack =
                    format!("{} {} {}", goal, pending, entry.agent_type).to_ascii_lowercase();
                if !haystack.contains(&needle) {
                    continue;
                }
            }
            rows.push(hya_tool::ArchivedAgentRow {
                handle: path.clone(),
                agent_type: entry.agent_type.as_str().to_string(),
                session: entry.session.to_string(),
                goal,
                pending,
                degraded,
            });
        }
        rows.sort_by(|left, right| left.handle.cmp(&right.handle));
        Ok(rows)
    }
}

/// Extract one numbered section's text from a six-section handoff document.
fn handoff_section(doc: &str, heading: &str) -> String {
    let prefix = heading.to_string();
    let mut take = false;
    let mut out = String::new();
    for line in doc.lines() {
        let trimmed = line.trim_start();
        let is_heading = trimmed
            .split_once(". ")
            .map(|(num, rest)| num.len() <= 2 && rest.starts_with(|c: char| c.is_uppercase()))
            .unwrap_or(false);
        if is_heading {
            take = trimmed.contains(&prefix);
            if take {
                out.push_str(
                    trimmed
                        .split_once(". ")
                        .map(|(_, rest)| rest)
                        .unwrap_or(trimmed),
                );
                out.push(' ');
            }
            continue;
        }
        if take {
            out.push_str(trimmed);
            out.push(' ');
        }
    }
    out.trim().chars().take(200).collect()
}
