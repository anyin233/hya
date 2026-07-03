use std::collections::BTreeMap;
use std::sync::Arc;

use hya_proto::{MemberId, MemberRunStatus, PartProjection, Projection, Role, SessionId};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::engine::{AgentSpec, CreateSession, SessionEngine};
use crate::error::CoreError;

/// Assign a stable, team-scoped handle (`{type}-{ordinal}`) to each member in
/// input order, continuing the per-type ordinal across earlier spawn batches.
///
/// Determinism (required for replay stability): the ordinal is derived only from
/// the current team roster + the batch's input order — no `rand`, no wall-clock.
/// Assigning sequentially here, before the parallel spawn, prevents concurrent
/// members from racing to the same ordinal. The main/root registration (its
/// session is the team root) is excluded from the counts so member handles start
/// at `-1`.
async fn assign_handles(
    engine: &SessionEngine,
    root: SessionId,
    specs: &[MemberSpec],
) -> Vec<String> {
    let roster = engine
        .read_projection(root)
        .await
        .map(|p| p.team.roster)
        .unwrap_or_default();
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for entry in roster.values() {
        if entry.session != root {
            *counts
                .entry(entry.agent_type.as_str().to_string())
                .or_insert(0) += 1;
        }
    }
    let mut handles = Vec::with_capacity(specs.len());
    for spec in specs {
        let agent_type = spec.agent.name.as_str().to_string();
        let ordinal = counts.entry(agent_type.clone()).or_insert(0);
        *ordinal += 1;
        handles.push(format!("{agent_type}-{ordinal}"));
    }
    handles
}

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
    handle: String,
    cancel: CancellationToken,
) -> Result<(SessionId, String), CoreError> {
    let member = spec.id;
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
    // Announce the member so observers can render it live in the agent tree.
    let (root, depth) = engine.session_lineage(child).await.unwrap_or((child, 0));
    let description: String = spec.directive.chars().take(80).collect();
    let _ = engine
        .record_member_spawned(
            lead,
            member,
            Some(child),
            spec.agent.name.clone(),
            description,
            depth,
        )
        .await;
    // Bind the member's session to its stable, team-scoped handle in the team-root
    // log (ADR-0001). The roster is then read from the projection, never disk.
    let _ = engine
        .record_agent_registered(root, child, handle, spec.agent.name.clone())
        .await;
    let _ = engine
        .record_member_status(lead, member, MemberRunStatus::Running)
        .await;
    engine.admit_user_prompt(child, spec.directive).await?;
    engine.run_turn(child, &spec.agent, cancel).await?;
    let projection = engine.read_projection(child).await?;
    Ok((child, summarize_member(&projection)))
}

fn rejected_evidence(id: MemberId, reason: &str) -> MemberEvidence {
    MemberEvidence {
        member: id.to_string(),
        session: "-".to_string(),
        status: MemberStatus::Failed,
        summary: reason.to_string(),
    }
}

/// Spawn each member as a supervised task in its own child session, run them in
/// parallel, and collect evidence. A panicking or failing member becomes a
/// `Failed` entry; it never takes down the supervisor or its peers.
///
/// When the engine has a [`SubagentGovernor`](crate::orchestrator::SubagentGovernor),
/// two bounds are enforced before spawning: a member that would exceed
/// `max_depth` is rejected, and members beyond the top-level run's remaining
/// budget are rejected. Rejected members surface as `Failed` evidence (in input
/// order) so the calling model gets a clean error instead of an unbounded fan-out.
/// The per-round streaming-concurrency cap is applied inside the turn loop.
pub async fn run_team(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    specs: Vec<MemberSpec>,
    cancel: CancellationToken,
) -> Vec<MemberEvidence> {
    let mut rejected: Vec<MemberEvidence> = Vec::new();
    let specs: Vec<MemberSpec> = if let Some(gov) = engine.governor() {
        let (root, lead_depth) = engine.session_lineage(lead).await.unwrap_or((lead, 0));
        if lead_depth.saturating_add(1) > gov.max_depth() {
            let mut out = Vec::new();
            for s in specs {
                let _ = engine
                    .record_member_finished(
                        lead,
                        s.id,
                        MemberRunStatus::Failed,
                        "max recursion depth reached".to_string(),
                        None,
                    )
                    .await;
                out.push(rejected_evidence(s.id, "max recursion depth reached"));
            }
            return out;
        }
        let want = u64::try_from(specs.len()).unwrap_or(u64::MAX);
        let granted = usize::try_from(gov.reserve(root, want)).unwrap_or(usize::MAX);
        let mut iter = specs.into_iter();
        let granted_specs: Vec<MemberSpec> = iter.by_ref().take(granted).collect();
        for s in iter {
            let _ = engine
                .record_member_finished(
                    lead,
                    s.id,
                    MemberRunStatus::Failed,
                    "run agent budget exhausted".to_string(),
                    None,
                )
                .await;
            rejected.push(rejected_evidence(s.id, "run agent budget exhausted"));
        }
        granted_specs
    } else {
        specs
    };

    // Assign stable, team-scoped handles deterministically BEFORE the parallel
    // spawn so concurrent members cannot race to the same ordinal. The main agent
    // is registered first so it appears in the roster and is addressable.
    let (root, _) = engine.session_lineage(lead).await.unwrap_or((lead, 0));
    let _ = engine.ensure_root_registered(root).await;
    let handles = assign_handles(&engine, root, &specs).await;

    let mut member_tasks = Vec::new();
    for (spec, handle) in specs.into_iter().zip(handles) {
        let engine = engine.clone();
        let child_cancel = cancel.child_token();
        let id = spec.id;
        let task =
            tokio::spawn(async move { run_member(engine, lead, spec, handle, child_cancel).await });
        member_tasks.push((id, task));
    }

    let mut evidence = Vec::new();
    for (id, task) in member_tasks {
        let (entry, member_status, child) = match task.await {
            Ok(Ok((session, summary))) => (
                MemberEvidence {
                    member: id.to_string(),
                    session: session.to_string(),
                    status: MemberStatus::Done,
                    summary: summary.clone(),
                },
                MemberRunStatus::Done,
                Some(session),
            ),
            Ok(Err(e)) => (
                MemberEvidence {
                    member: id.to_string(),
                    session: "-".to_string(),
                    status: MemberStatus::Failed,
                    summary: e.to_string(),
                },
                MemberRunStatus::Failed,
                None,
            ),
            Err(join_err) => {
                let (summary, status) = if join_err.is_panic() {
                    ("member panicked".to_string(), MemberRunStatus::Failed)
                } else {
                    ("member cancelled".to_string(), MemberRunStatus::Cancelled)
                };
                (
                    MemberEvidence {
                        member: id.to_string(),
                        session: "-".to_string(),
                        status: MemberStatus::Failed,
                        summary: summary.clone(),
                    },
                    status,
                    None,
                )
            }
        };
        let _ = engine
            .record_member_finished(lead, id, member_status, entry.summary.clone(), child)
            .await;
        evidence.push(entry);
    }
    evidence.extend(rejected);
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
