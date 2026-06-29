use std::sync::Arc;

use hya_proto::{MemberId, PartProjection, Projection, Role, SessionId};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::engine::{AgentSpec, CreateSession, SessionEngine};
use crate::error::CoreError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    Done,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemberEvidence {
    pub member: String,
    pub session: String,
    pub status: MemberStatus,
    pub summary: String,
}

/// Bounded, lead-visible evidence of a team turn (design.md §10). Carries
/// per-member status + a short summary, NEVER the full child transcripts.
#[derive(Clone, Debug, Serialize)]
pub struct TeamEvidenceEnvelope {
    pub members: Vec<MemberEvidence>,
}

pub struct MemberSpec {
    pub id: MemberId,
    pub agent: AgentSpec,
    pub directive: String,
    pub session: Option<SessionId>,
}

fn summarize_member(projection: &Projection) -> String {
    for m in projection.session.messages.iter().rev() {
        if matches!(m.role, Role::Assistant) {
            let mut text = String::new();
            for p in &m.parts {
                if let PartProjection::Text { text: t, .. } = p {
                    text.push_str(t);
                }
            }
            return text.chars().take(120).collect();
        }
    }
    "no assistant output".to_string()
}

async fn run_member(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    spec: MemberSpec,
    cancel: CancellationToken,
) -> Result<(SessionId, String), CoreError> {
    let child = if let Some(session) = spec.session {
        session
    } else {
        engine
            .create(CreateSession {
                parent: Some(lead),
                agent: spec.agent.name.clone(),
                model: spec.agent.model.clone(),
                workdir: spec.agent.workdir.to_string_lossy().into_owned(),
            })
            .await?
    };
    engine.admit_user_prompt(child, spec.directive).await?;
    engine.run_turn(child, &spec.agent, cancel).await?;
    let projection = engine.read_projection(child).await?;
    Ok((child, summarize_member(&projection)))
}

/// Spawn each member as a supervised task in its own child session, run them in
/// parallel, and collect evidence. A panicking or failing member becomes a
/// `Failed` entry; it never takes down the supervisor or its peers.
pub async fn run_team(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    specs: Vec<MemberSpec>,
    cancel: CancellationToken,
) -> Vec<MemberEvidence> {
    let mut handles = Vec::new();
    for spec in specs {
        let engine = engine.clone();
        let child_cancel = cancel.child_token();
        let id = spec.id;
        let handle =
            tokio::spawn(async move { run_member(engine, lead, spec, child_cancel).await });
        handles.push((id, handle));
    }

    let mut evidence = Vec::new();
    for (id, handle) in handles {
        let entry = match handle.await {
            Ok(Ok((session, summary))) => MemberEvidence {
                member: id.to_string(),
                session: session.to_string(),
                status: MemberStatus::Done,
                summary,
            },
            Ok(Err(e)) => MemberEvidence {
                member: id.to_string(),
                session: "-".to_string(),
                status: MemberStatus::Failed,
                summary: e.to_string(),
            },
            Err(join_err) => MemberEvidence {
                member: id.to_string(),
                session: "-".to_string(),
                status: MemberStatus::Failed,
                summary: if join_err.is_panic() {
                    "member panicked".to_string()
                } else {
                    "member cancelled".to_string()
                },
            },
        };
        evidence.push(entry);
    }
    evidence
}

/// Project the envelope into the LEAD transcript as a System message so the
/// completion engine's evaluator can judge it — without replaying child transcripts.
pub async fn project_envelope(
    engine: &SessionEngine,
    lead: SessionId,
    envelope: &TeamEvidenceEnvelope,
) -> Result<(), CoreError> {
    let json = serde_json::to_string(envelope).unwrap_or_else(|_| "{}".to_string());
    engine
        .inject_system_message(lead, format!("TEAM EVIDENCE ENVELOPE\n{json}"))
        .await?;
    Ok(())
}
