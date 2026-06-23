use std::sync::Arc;

use tokio::sync::mpsc;
use yaca_core::{
    AgentSpec, MemberSpec, MemberStatus, SessionEngine, TeamEvidenceEnvelope, project_envelope,
    run_team,
};
use yaca_proto::{MemberId, SessionId};
use yaca_tool::{MemberOutcome, SpawnMember, SpawnRequest};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamStatusUpdate {
    pub parent: SessionId,
    pub members: Vec<(String, String)>,
}

pub fn active_update(parent: SessionId, members: &[SpawnMember]) -> TeamStatusUpdate {
    TeamStatusUpdate {
        parent,
        members: members
            .iter()
            .map(|member| (active_member_label(member), "running".to_string()))
            .collect(),
    }
}

pub fn finished_update(parent: SessionId, outcomes: &[MemberOutcome]) -> TeamStatusUpdate {
    TeamStatusUpdate {
        parent,
        members: outcomes
            .iter()
            .map(|outcome| (outcome.member.clone(), outcome.status.clone()))
            .collect(),
    }
}

pub fn spawn_team_supervisor(
    mut rx: mpsc::UnboundedReceiver<SpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    updates: mpsc::UnboundedSender<TeamStatusUpdate>,
) {
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            let engine = engine.clone();
            let base = base.clone();
            let updates = updates.clone();
            let parent = req.parent;
            let members = req.members;
            let cancel = req.cancel;
            let reply = req.reply;
            let _ = updates.send(active_update(parent, &members));
            let specs: Vec<MemberSpec> = members
                .into_iter()
                .map(|member| MemberSpec {
                    id: MemberId::new(),
                    agent: base.clone(),
                    directive: member.prompt,
                })
                .collect();
            tokio::spawn(async move {
                let evidence = run_team(engine.clone(), parent, specs, cancel).await;
                let envelope = TeamEvidenceEnvelope {
                    members: evidence.clone(),
                };
                let _ = project_envelope(&engine, parent, &envelope).await;
                let outcomes: Vec<MemberOutcome> = evidence
                    .into_iter()
                    .map(|entry| MemberOutcome {
                        member: entry.member,
                        session: entry.session,
                        status: match entry.status {
                            MemberStatus::Done => "done".to_string(),
                            MemberStatus::Failed => "failed".to_string(),
                        },
                        summary: entry.summary,
                    })
                    .collect();
                let _ = updates.send(finished_update(parent, &outcomes));
                let _ = reply.send(outcomes);
            });
        }
    });
}

fn active_member_label(member: &SpawnMember) -> String {
    let description = member.description.trim();
    if description.is_empty() {
        member.subagent_type.clone()
    } else {
        description.to_string()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use yaca_proto::SessionId;
    use yaca_tool::{MemberOutcome, SpawnMember};

    use super::*;

    #[test]
    fn active_update_uses_subagent_descriptions_with_running_status() {
        // Given: the task tool requested two subagents for a parent session.
        let parent = SessionId::new();
        let members = vec![
            SpawnMember {
                description: "inspect auth flow".to_string(),
                prompt: "read auth code".to_string(),
                subagent_type: "quick".to_string(),
            },
            SpawnMember {
                description: String::new(),
                prompt: "review tests".to_string(),
                subagent_type: "deep".to_string(),
            },
        ];

        // When: the supervisor exposes the active team to the TUI.
        let update = active_update(parent, &members);

        // Then: labels are useful in the Agents card and all active rows are running.
        assert_eq!(update.parent, parent);
        assert_eq!(
            update.members,
            vec![
                ("inspect auth flow".to_string(), "running".to_string()),
                ("deep".to_string(), "running".to_string()),
            ]
        );
    }

    #[test]
    fn finished_update_preserves_real_done_and_failed_statuses() {
        // Given: the CLI supervisor received final member outcomes.
        let parent = SessionId::new();
        let outcomes = vec![
            MemberOutcome {
                member: "mbr_done".to_string(),
                session: "ses_done".to_string(),
                status: "done".to_string(),
                summary: "ok".to_string(),
            },
            MemberOutcome {
                member: "mbr_failed".to_string(),
                session: "-".to_string(),
                status: "failed".to_string(),
                summary: "boom".to_string(),
            },
        ];

        // When: the supervisor exposes the finished team to the TUI.
        let update = finished_update(parent, &outcomes);

        // Then: it keeps the real runtime labels that the renderer must classify.
        assert_eq!(update.parent, parent);
        assert_eq!(
            update.members,
            vec![
                ("mbr_done".to_string(), "done".to_string()),
                ("mbr_failed".to_string(), "failed".to_string()),
            ]
        );
    }
}
