use std::collections::BTreeMap;
use std::sync::Arc;

use hya_proto::{
    Event, FinishReason, MemberId, MemberRunStatus, PartProjection, Projection, Role, SessionId,
    SubagentMode, ToolCallId,
};
use hya_store::ActorClaim;
use hya_tool::AgentDef;
use serde::Serialize;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::engine::{
    AdmissionMemberIdentity, AgentSpec, CreateSession, SessionEngine, scope_admission_member,
};
use crate::error::CoreError;
use crate::hooks::scope_activation_hooks;
use crate::sidecar::{BoundSidecarFactory, SidecarHandle, SidecarStart};
use crate::{AgentResourcePolicy, TurnBinding};

/// Assign a stable, team-scoped handle (`{type}-{ordinal}`) to each member in
/// input order, continuing the per-type ordinal across earlier spawn batches.
///
/// Determinism (required for replay stability): the ordinal is derived only from
/// the current team roster + the batch's input order — no `rand`, no wall-clock.
/// Assigning sequentially here, before the parallel spawn, prevents concurrent
/// members from racing to the same ordinal. The main/root registration (its
/// session is the team root) is excluded from the counts so member handles start
/// at `-1`.
///
/// Resume path: when a member reuses an existing child session that already has
/// a roster binding, that handle is returned as-is. Allocating a second handle
/// for the same session would make the TUI roster list the agent twice and
/// multi-highlight both rows (they share one session id as the select value).
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
        if let Some(session) = spec.session
            && let Some(existing) = roster.values().find(|entry| entry.session == session)
        {
            handles.push(existing.handle.clone());
            continue;
        }
        let agent_type = spec.agent.name.as_str().to_string();
        let ordinal = counts.entry(agent_type.clone()).or_insert(0);
        *ordinal += 1;
        handles.push(format!("{agent_type}-{ordinal}"));
    }
    handles
}

/// Prefer the member id already bound to `child` under `lead`, so a task_id
/// resume upserts the original tree row instead of appending a duplicate.
async fn resolve_member_id(
    engine: &SessionEngine,
    lead: SessionId,
    preferred: MemberId,
    child: SessionId,
) -> MemberId {
    engine
        .read_projection(lead)
        .await
        .ok()
        .and_then(|projection| {
            projection
                .session
                .members
                .iter()
                .find(|entry| entry.child == Some(child))
                .map(|entry| entry.member)
        })
        .unwrap_or(preferred)
}

/// Terminal status of one team member in lead-visible evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    /// Member finished successfully.
    Done,
    /// Member failed or was aborted.
    Failed,
}

/// Lead-visible row for one member (never full child transcripts).
#[derive(Clone, Debug, Serialize)]
pub struct MemberEvidence {
    /// Member handle or label.
    pub member: String,
    /// Child session id as string.
    pub session: String,
    /// Terminal status.
    pub status: MemberStatus,
    /// Short summary for the parent.
    pub summary: String,
}

/// Bounded, lead-visible evidence of a team turn (design.md §10). Carries
/// per-member status + a short summary, NEVER the full child transcripts.
#[derive(Clone, Debug, Serialize)]
pub struct TeamEvidenceEnvelope {
    /// One entry per member that ran in the turn, in spawn order.
    pub members: Vec<MemberEvidence>,
}

/// Failure admitting a multi-member team before any child runs.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum TeamAdmissionError {
    /// Nested depth would exceed the governor max.
    #[error("max recursion depth reached")]
    MaxDepth,
    /// Per-run spawn budget exhausted.
    #[error("run agent budget exhausted")]
    BudgetExhausted,
}

/// Fully resolved specification for one team member spawn.
pub struct MemberSpec {
    /// Member id for tree rows.
    pub id: MemberId,
    /// Effective agent for the child turn.
    pub agent: AgentSpec,
    /// Exact immutable runtime snapshot captured by the parent turn.
    pub binding: TurnBinding,
    /// Caller-authorized catalog reachability projected at spawn resolution.
    /// Inline public names never become catalog identities or roster keys.
    pub agents: Arc<[AgentDef]>,
    /// Catalog resource policy captured alongside the authorized target.
    /// `None` requires an exact lookup of `agent.name` in the captured binding.
    pub resources: Option<AgentResourcePolicy>,
    /// Immutable triggering-turn guidance Arc (request-scoped; not persisted).
    pub guidance: Option<Arc<str>>,
    /// Full task prompt for the child turn.
    pub directive: String,
    /// Tool call that caused this spawn, when it came from one.
    ///
    /// Recorded on `MemberSpawned` so an offline graph can anchor the spawn edge
    /// to an exact point in the parent's trajectory instead of inferring it from
    /// event ordering. `None` for supervisor-started resident members.
    pub tool_call: Option<ToolCallId>,
    /// Short UI label from the task tool (3–5 words). Empty falls back to a
    /// truncated directive so observers still get a readable row title.
    pub description: String,
    /// Optional existing child session to resume.
    pub session: Option<SessionId>,
    /// Request-scoped sidecar factory already bound by the application.
    /// This opaque capability is not persisted with the member specification.
    pub sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
}

/// Short label for tree / TUI rows: task description when present, else prompt.
fn member_ui_description(spec: &MemberSpec) -> String {
    let short = spec.description.trim();
    if !short.is_empty() {
        return short.chars().take(80).collect();
    }
    spec.directive.chars().take(80).collect()
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

struct MemberRunOutcome {
    child: SessionId,
    summary: String,
    sidecar: Option<Box<dyn SidecarHandle>>,
}

async fn run_member(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    spec: MemberSpec,
    handle: String,
    cancel: CancellationToken,
    actor_claim: Option<ActorClaim>,
) -> Result<MemberRunOutcome, CoreError> {
    engine.validate_actor_claim(actor_claim.as_ref()).await?;
    let child = if let Some(session) = spec.session {
        session
    } else {
        match actor_claim.as_ref() {
            Some(claim) => {
                engine
                    .create_for_actor(
                        claim,
                        CreateSession {
                            parent: Some(lead),
                            agent: spec.agent.name.clone(),
                            model: spec.agent.model.clone(),
                            workdir: spec.agent.workdir.to_string_lossy().into_owned(),
                        },
                    )
                    .await?
            }
            None => {
                engine
                    .create(CreateSession {
                        parent: Some(lead),
                        agent: spec.agent.name.clone(),
                        model: spec.agent.model.clone(),
                        workdir: spec.agent.workdir.to_string_lossy().into_owned(),
                    })
                    .await?
            }
        }
    };
    // Resume (task_id) reuses the child session; keep the original member id so
    // MemberSpawned upserts rather than listing the same agent twice.
    let member = resolve_member_id(&engine, lead, spec.id, child).await;
    // Announce the member so observers can render it live in the agent tree.
    // Prefer the short task-tool description so the main transcript Task rows
    // can match and display status the way OpenCode does.
    let (root, depth) = engine.session_lineage(child).await.unwrap_or((child, 0));
    let description = member_ui_description(&spec);
    engine
        .emit_for_actor(
            actor_claim.as_ref(),
            lead,
            Event::MemberSpawned {
                session: lead,
                member,
                child: Some(child),
                subagent_type: spec.agent.name.clone(),
                description,
                depth,
                // Cloned: `spec.directive` is moved into the child prompt below.
                directive: spec.directive.clone(),
                tool_call: spec.tool_call,
            },
        )
        .await?;
    // Bind the member's session to its stable, team-scoped handle in the team-root
    // log (ADR-0001). The roster is then read from the projection, never disk.
    // run_team is the transient (blocking-join) path by construction; resident
    // members are spawned non-blocking through the ResidentSupervisor instead.
    engine
        .emit_for_actor(
            actor_claim.as_ref(),
            root,
            Event::AgentRegistered {
                session: root,
                agent_session: child,
                handle,
                agent_type: spec.agent.name.clone(),
                mode: SubagentMode::Transient,
            },
        )
        .await?;
    let (sidecar_handle, sidecar_tools, sidecar_hooks) =
        if let Some(factory) = spec.sidecar_factory.clone() {
            let mut handle = factory.start(SidecarStart::transient()).await?;
            if let Err(error) = handle.ready().await {
                let _ = handle.terminate().await;
                return Err(error);
            }
            let sidecar_tools = handle.tool_bindings();
            let sidecar_hooks = handle.hook_dispatcher();
            (Some(handle), sidecar_tools, sidecar_hooks)
        } else {
            (None, Arc::from([]), None)
        };
    let loss_token = sidecar_handle
        .as_ref()
        .and_then(|handle| handle.loss_token());
    let run = async {
        engine
            .emit_for_actor(
                actor_claim.as_ref(),
                lead,
                Event::MemberStatusChanged {
                    session: lead,
                    member,
                    status: MemberRunStatus::Running,
                },
            )
            .await?;
        let finish_reason = match actor_claim.as_ref() {
            Some(claim) => {
                engine
                    .admit_user_prompt_for_actor(claim, child, spec.directive)
                    .await?;
                match spec.resources.clone() {
                    Some(resources) => {
                        engine
                            .run_resolved_turn_with_sidecar_tools_for_actor(
                                child,
                                &spec.agent,
                                (
                                    spec.binding.clone(),
                                    spec.agents.clone(),
                                    resources,
                                    sidecar_tools.clone(),
                                ),
                                claim,
                                cancel,
                                spec.guidance.clone(),
                            )
                            .await?
                    }
                    None => {
                        engine
                            .run_bound_turn_for_actor(
                                child,
                                &spec.agent,
                                spec.binding.clone(),
                                claim,
                                cancel,
                                spec.guidance.clone(),
                            )
                            .await?
                    }
                }
            }
            None => {
                engine.admit_user_prompt(child, spec.directive).await?;
                match spec.resources {
                    Some(resources) => {
                        engine
                            .run_resolved_turn_with_sidecar_tools(
                                child,
                                &spec.agent,
                                (spec.binding, spec.agents.clone(), resources, sidecar_tools),
                                cancel,
                                spec.guidance,
                            )
                            .await?
                    }
                    None => {
                        engine
                            .run_bound_turn(child, &spec.agent, spec.binding, cancel, spec.guidance)
                            .await?
                    }
                }
            }
        };
        if matches!(finish_reason, FinishReason::Cancelled) {
            return Err(CoreError::Cancelled);
        }
        let projection = engine.read_projection(child).await?;
        Ok::<String, CoreError>(summarize_member(&projection))
    };
    let run_result = match (sidecar_hooks, loss_token) {
        (Some(hooks), Some(loss_token)) => {
            tokio::select! {
                biased;
                _ = loss_token.cancelled() => Err(CoreError::Cancelled),
                result = scope_activation_hooks(child, hooks, run) => result,
            }
        }
        (Some(hooks), None) => scope_activation_hooks(child, hooks, run).await,
        (None, Some(loss_token)) => {
            tokio::select! {
                biased;
                _ = loss_token.cancelled() => Err(CoreError::Cancelled),
                result = run => result,
            }
        }
        (None, None) => run.await,
    };
    match run_result {
        Ok(summary) => Ok(MemberRunOutcome {
            child,
            summary,
            sidecar: sidecar_handle,
        }),
        Err(error) => {
            if let Some(mut sidecar) = sidecar_handle {
                let _ = sidecar.terminate().await;
            }
            Err(error)
        }
    }
}

fn rejected_evidence(id: MemberId, reason: &str) -> MemberEvidence {
    MemberEvidence {
        member: id.to_string(),
        session: "-".to_string(),
        status: MemberStatus::Failed,
        summary: reason.to_string(),
    }
}

/// Reserve an entire team before any child session or request-owned task exists.
///
/// Background spawns need an all-or-nothing decision because their caller gets
/// one request-level typed result. Foreground batches retain [`run_team`]'s
/// historical partial-admission evidence.
pub async fn pre_admit_team(
    engine: &SessionEngine,
    lead: SessionId,
    members: usize,
) -> Result<(), TeamAdmissionError> {
    let Some(governor) = engine.governor() else {
        return Ok(());
    };
    let (root, lead_depth) = engine.session_lineage(lead).await.unwrap_or((lead, 0));
    if lead_depth.saturating_add(1) > governor.max_depth() {
        return Err(TeamAdmissionError::MaxDepth);
    }
    let want = u64::try_from(members).unwrap_or(u64::MAX);
    if governor.try_reserve_exact(root, want) {
        Ok(())
    } else {
        Err(TeamAdmissionError::BudgetExhausted)
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
    run_team_inner(engine, lead, specs, cancel, true, None, None).await
}

/// Run a team whose complete member set was reserved by [`pre_admit_team`].
///
/// This is the background continuation path; using it without a successful
/// pre-admission would bypass the governor.
pub async fn run_pre_admitted_team(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    specs: Vec<MemberSpec>,
    cancel: CancellationToken,
) -> Vec<MemberEvidence> {
    run_team_inner(engine, lead, specs, cancel, false, None, None).await
}

/// Run one member that has already been admitted by the durable scheduler.
///
/// The admission identity is process-local orchestration context only; it is
/// available to nested spawn dispatch without changing session or event data.
pub async fn run_pre_admitted_member(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    member: MemberSpec,
    cancel: CancellationToken,
    admission: AdmissionMemberIdentity,
) -> Vec<MemberEvidence> {
    run_team_inner(
        engine,
        lead,
        vec![member],
        cancel,
        false,
        None,
        Some(admission),
    )
    .await
}

/// Run a pre-admitted team under an actor claim fence.
pub async fn run_pre_admitted_team_for_actor(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    specs: Vec<MemberSpec>,
    cancel: CancellationToken,
    actor_claim: ActorClaim,
) -> Vec<MemberEvidence> {
    run_team_inner(engine, lead, specs, cancel, false, Some(actor_claim), None).await
}

async fn run_team_inner(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    specs: Vec<MemberSpec>,
    cancel: CancellationToken,
    reserve_admission: bool,
    actor_claim: Option<ActorClaim>,
    admission: Option<AdmissionMemberIdentity>,
) -> Vec<MemberEvidence> {
    let mut rejected: Vec<MemberEvidence> = Vec::new();
    let specs: Vec<MemberSpec> = if !reserve_admission {
        specs
    } else if let Some(gov) = engine.governor() {
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
    let _ = engine
        .ensure_root_registered_for_actor(root, actor_claim.as_ref())
        .await;
    // Resume (existing child session): reuse member id + roster handle so finish /
    // tree events upsert the original row instead of appending a duplicate.
    let mut specs = specs;
    for spec in &mut specs {
        if let Some(session) = spec.session {
            spec.id = resolve_member_id(&engine, lead, spec.id, session).await;
        }
    }
    let handles = assign_handles(&engine, root, &specs).await;

    let mut member_tasks = Vec::new();
    for (spec, handle) in specs.into_iter().zip(handles) {
        let engine = engine.clone();
        let child_cancel = cancel.child_token();
        let id = spec.id;
        let task = tokio::spawn(async move {
            scope_admission_member(
                admission,
                run_member(engine, lead, spec, handle, child_cancel, actor_claim),
            )
            .await
        });
        member_tasks.push((id, task));
    }

    let mut evidence = Vec::new();
    for (id, task) in member_tasks {
        let (entry, member_status, child, sidecar_handle) = match task.await {
            Ok(Ok(MemberRunOutcome {
                child: session,
                summary,
                sidecar,
            })) => (
                MemberEvidence {
                    member: id.to_string(),
                    session: session.to_string(),
                    status: MemberStatus::Done,
                    summary: summary.clone(),
                },
                MemberRunStatus::Done,
                Some(session),
                sidecar,
            ),
            Ok(Err(CoreError::Cancelled)) => (
                MemberEvidence {
                    member: id.to_string(),
                    session: "-".to_string(),
                    status: MemberStatus::Failed,
                    summary: "member cancelled".to_string(),
                },
                MemberRunStatus::Cancelled,
                None,
                None,
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
                    None,
                )
            }
        };
        let _ = engine
            .emit_for_actor(
                actor_claim.as_ref(),
                lead,
                Event::MemberFinished {
                    session: lead,
                    member: id,
                    status: member_status,
                    summary: entry.summary.clone(),
                    child,
                },
            )
            .await;
        if let Some(mut sidecar) = sidecar_handle {
            let _ = sidecar.shutdown().await;
        }
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

/// Project a team evidence envelope for an actor-scoped observation.
pub async fn project_envelope_for_actor(
    engine: &SessionEngine,
    lead: SessionId,
    envelope: &TeamEvidenceEnvelope,
    actor_claim: &ActorClaim,
) -> Result<(), CoreError> {
    let json = serde_json::to_string(envelope).unwrap_or_else(|_| "{}".to_string());
    engine
        .inject_system_message_for_actor(
            actor_claim,
            lead,
            format!("TEAM EVIDENCE ENVELOPE\n{json}"),
        )
        .await?;
    Ok(())
}
