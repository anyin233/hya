//! Workflow execution over the governed subagent team primitives.
//!
//! Each topological level is executed as ONE batch through
//! [`pre_admit_team`](crate::subagent::pre_admit_team) /
//! [`run_pre_admitted_team`](crate::subagent::run_pre_admitted_team), so the
//! [`SubagentGovernor`](crate::SubagentGovernor) still bounds depth, per-run
//! spawn budget, and streaming concurrency for user-authored DAGs exactly as it
//! does for model-decided `task` batches. Loop stages iterate through
//! [`IterationDriver`](crate::completion::IterationDriver) with an independent
//! verifier member — never a second loop implementation.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{MemberId, SessionId};
use hya_tool::AgentDef;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::AgentResourcePolicy;
use crate::completion::{
    GateOutcome, IterationDriver, IterationExecutor, IterationGate, SafetyCaps,
};
use crate::engine::{AgentSpec, SessionEngine};
use crate::error::CoreError;
use crate::sidecar::BoundSidecarFactory;
use crate::subagent::{
    MemberEvidence, MemberSpec, MemberStatus, pre_admit_team, run_pre_admitted_team,
};

use super::model::{FailurePolicy, VerifySpec, WorkflowDef};
use super::plan::{StageSection, render_template};
use super::{WorkflowError, plan::build_plan};

/// Upper bound (bytes) of one upstream section rendered into a join directive.
pub const MAX_STAGE_OUTPUT_CHARS: usize = 4_000;

/// Per-run inputs for executing a workflow.
///
/// One value per run; everything here comes from the calling surface (CLI,
/// tool plane) which owns the session binding and caller authorization. Each
/// member's own execution context (spawnable roster, resource policy, sidecar
/// factory) is derived from the STAGE agent's definition through the same
/// engine accessors the task-tool spawn path uses — never inherited from the
/// caller — so a stage can only delegate further targets listed in its own
/// `can_spawn` roster.
#[derive(Clone)]
pub struct WorkflowRunContext {
    /// Immutable runtime snapshot binding used to resolve stage agents.
    pub binding: crate::runtime_registry::TurnBinding,
    /// Caller agent id whose `can_spawn` roster authorizes every stage agent.
    pub caller: String,
    /// Base agent spec for member derivation (model fallback context).
    pub base_agent: AgentSpec,
    /// Values for declared workflow inputs. Every declared key must be present.
    pub inputs: BTreeMap<String, String>,
}

/// Terminal status of one workflow stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    /// The member finished successfully.
    Done,
    /// The member failed, was cancelled, or the scripted provider errored.
    Failed,
}

impl std::fmt::Display for StageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Done => write!(f, "done"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Lead-visible result of one stage.
#[derive(Clone, Debug, Serialize)]
pub struct StageReport {
    /// Stage id from the definition.
    pub stage: String,
    /// Resolved agent id that ran the stage.
    pub agent: String,
    /// Terminal member status.
    pub status: StageStatus,
    /// Child session carrying the transcript, when the member ran.
    pub session: Option<String>,
    /// For [`StageStatus::Done`]: bounded final assistant output handed to
    /// downstream joins. For [`StageStatus::Failed`]: the failure summary.
    pub output: String,
}

/// Overall terminal state of a workflow run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    /// Every level of the DAG ran per its declared failure policy.
    Completed,
    /// Fail-fast aborted the run because a level had a failed member.
    Failed,
    /// Cancellation tripped before some levels ran.
    Cancelled,
}

impl std::fmt::Display for WorkflowStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Result of a full workflow execution: overall status plus one row per stage
/// that ran, in declaration order.
#[derive(Clone, Debug, Serialize)]
pub struct WorkflowRunReport {
    /// Overall terminal state.
    pub status: WorkflowStatus,
    /// Reports for stages that ran; absent stages did not start.
    pub stages: Vec<StageReport>,
}

/// Execution context of one stage agent, computed up front so authorization
/// failures surface before any member spawns. Mirrors
/// `resolve_authorized_spawn_member` in the task-tool path: the target's own
/// spec, spawnable roster, resource policy, and bound sidecar factory.
#[derive(Clone)]
struct ResolvedAgent {
    spec: AgentSpec,
    agents: Arc<[AgentDef]>,
    resources: Option<AgentResourcePolicy>,
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
}

fn resolve_agent(
    engine: &SessionEngine,
    ctx: &WorkflowRunContext,
    agent_id: &str,
) -> Result<ResolvedAgent, WorkflowError> {
    let definition = ctx
        .binding
        .resolve_spawn(&ctx.caller, agent_id)
        .map_err(|error| WorkflowError::Unauthorized {
            caller: ctx.caller.clone(),
            agent_id: agent_id.to_string(),
            detail: error.to_string(),
        })?;
    let spec =
        engine.agent_spec_for_binding(&ctx.binding, &ctx.base_agent, definition.stable_id)?;
    // The member's OWN reachable roster comes from the TARGET agent's
    // `can_spawn`, not the caller's; this bounds what the stage can itself
    // delegate during its turn to exactly what the task path would grant.
    let unauthorized = |detail: String| WorkflowError::Unauthorized {
        caller: ctx.caller.clone(),
        agent_id: agent_id.to_string(),
        detail,
    };
    let agents = engine
        .agent_roster_for_binding(&ctx.binding, definition.stable_id)
        .map_err(|error| unauthorized(error.to_string()))?;
    let resources = engine
        .agent_resource_policy_for_binding(&ctx.binding, definition.stable_id)
        .map_err(|error| unauthorized(error.to_string()))?
        .into();
    let sidecar_factory = match engine.sidecar_environment() {
        Some(environment) => environment
            .factory_for(&ctx.binding, definition.stable_id)
            .map_err(|error| unauthorized(error.to_string()))?,
        None => None,
    };
    Ok(ResolvedAgent {
        spec,
        agents,
        resources,
        sidecar_factory,
    })
}

/// Execute `def` under `lead`, honoring `ctx`.
///
/// # Errors
/// [`WorkflowError`] for invalid definitions, unauthorized/unknown stage
/// agents, admission rejections, and engine failures during member turns.
pub async fn run_workflow(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    def: &WorkflowDef,
    ctx: WorkflowRunContext,
    cancel: CancellationToken,
) -> Result<WorkflowRunReport, WorkflowError> {
    let plan = build_plan(def)?;

    // Fast-fail before any spawn when the static graph cannot fit the run
    // budget; loops may spend more at runtime and trip exact admission then.
    if let Some(governor) = engine.governor() {
        let wanted = u64::try_from(def.stages.len()).unwrap_or(u64::MAX);
        let budget = governor.limits().per_run_budget;
        if wanted > budget {
            return Err(WorkflowError::Invalid {
                workflow: def.name.clone(),
                detail: format!("{wanted} stages exceed the per-run budget of {budget}"),
            });
        }
    }

    // Resolve every worker and verifier agent against the caller's roster up
    // front; a typo must abort before the first batch instead of mid-run.
    let mut resolved: Vec<ResolvedAgent> = Vec::with_capacity(def.stages.len());
    let mut verifiers: Vec<Option<ResolvedAgent>> = Vec::with_capacity(def.stages.len());
    for stage in &def.stages {
        resolved.push(resolve_agent(&engine, &ctx, &stage.agent)?);
        verifiers.push(
            stage
                .verify
                .as_ref()
                .map(|verify| resolve_agent(&engine, &ctx, &verify.agent))
                .transpose()?,
        );
    }

    // Declared inputs must be provided for this run.
    for key in def.inputs.keys() {
        if !ctx.inputs.contains_key(key) {
            return Err(WorkflowError::Invalid {
                workflow: def.name.clone(),
                detail: format!("declared input `{key}` was not provided"),
            });
        }
    }

    // outputs[i] records finished stages for fan-in rendering.
    let mut reports: Vec<Option<StageReport>> = (0..def.stages.len()).map(|_| None).collect();
    let mut overall = WorkflowStatus::Completed;

    for level in plan.levels() {
        if cancel.is_cancelled() {
            return Ok(WorkflowRunReport {
                status: WorkflowStatus::Cancelled,
                stages: collect_reports(def, reports),
            });
        }

        let sections = joined_sections(def, &reports);
        let mut specs = Vec::with_capacity(level.len());
        for &index in level {
            let stage = &def.stages[index];
            let directive = render_template(&def.name, &stage.prompt, &ctx.inputs, &sections)?;
            let resolved_stage = resolved[index].clone();
            specs.push(MemberSpec {
                id: MemberId::new(),
                agent: resolved_stage.spec,
                binding: ctx.binding.clone(),
                agents: resolved_stage.agents,
                resources: resolved_stage.resources,
                guidance: None,
                directive,
                tool_call: None,
                description: format!("workflow {} / {}", def.name, stage.id),
                session: None,
                sidecar_factory: resolved_stage.sidecar_factory,
            });
        }

        pre_admit_team(&engine, lead, specs.len())
            .await
            .map_err(WorkflowError::Admission)?;
        let evidence =
            run_pre_admitted_team(engine.clone(), lead, specs, cancel.child_token()).await;
        // Pre-admitted batches never reject members, so evidence aligns 1:1
        // with the level's input order.
        for (&index, evidence) in level.iter().zip(evidence.iter()) {
            let report = stage_report(&engine, &def.stages[index], evidence).await?;
            reports[index] = Some(report);
        }

        // Loop stages iterate through the shared driver now that their first
        // round completed inside this level's batch.
        for &index in level {
            let (Some(report), Some(verify)) = (&reports[index], &def.stages[index].verify) else {
                continue;
            };
            if report.status == StageStatus::Done {
                let Some(verifier) = verifiers[index].clone() else {
                    continue;
                };
                let mut report = report.clone();
                match drive_loop_stage(
                    engine.clone(),
                    lead,
                    def,
                    &ctx,
                    index,
                    verify,
                    &resolved[index],
                    verifier,
                    report.clone(),
                    cancel.clone(),
                )
                .await
                {
                    Ok(completed) => reports[index] = Some(completed),
                    Err(error) => {
                        report.status = StageStatus::Failed;
                        report.output = clamp(format!("loop stage failed: {error}"));
                        reports[index] = Some(report);
                    }
                }
            }
        }

        if cancel.is_cancelled() {
            return Ok(WorkflowRunReport {
                status: WorkflowStatus::Cancelled,
                stages: collect_reports(def, reports),
            });
        }

        // Join-side failure contract declared by the author.
        if def.on_member_failure == FailurePolicy::FailFast && level_failed(&reports, level) {
            overall = WorkflowStatus::Failed;
            break;
        }
    }

    Ok(WorkflowRunReport {
        status: overall,
        stages: collect_reports(def, reports),
    })
}

fn joined_sections(
    def: &WorkflowDef,
    reports: &[Option<StageReport>],
) -> BTreeMap<String, StageSection> {
    let mut sections = BTreeMap::new();
    for (report, stage) in reports.iter().zip(def.stages.iter()) {
        if let Some(report) = report {
            sections.insert(stage.id.clone(), StageSection::from_report(report));
        }
    }
    sections
}

fn level_failed(reports: &[Option<StageReport>], level: &[usize]) -> bool {
    level.iter().any(|&i| {
        reports[i]
            .as_ref()
            .is_some_and(|r| r.status == StageStatus::Failed)
    })
}

async fn stage_report(
    engine: &Arc<SessionEngine>,
    stage: &super::model::StageDef,
    evidence: &MemberEvidence,
) -> Result<StageReport, WorkflowError> {
    let base = StageReport {
        stage: stage.id.clone(),
        agent: stage.agent.clone(),
        status: StageStatus::Failed,
        session: None,
        output: String::new(),
    };
    if evidence.status != MemberStatus::Done {
        return Ok(StageReport {
            output: evidence.summary.clone(),
            ..base
        });
    }
    let Ok(child) = evidence.session.parse::<SessionId>() else {
        return Ok(base);
    };
    let projection = engine.read_projection(child).await?;
    Ok(StageReport {
        status: StageStatus::Done,
        session: Some(evidence.session.clone()),
        output: final_assistant_text(&projection),
        ..base
    })
}

/// Last assistant text of the projection, clamped to
/// [`MAX_STAGE_OUTPUT_CHARS`]; empty when the member produced nothing.
fn final_assistant_text(projection: &hya_proto::Projection) -> String {
    for message in projection.session.messages.iter().rev() {
        if !matches!(message.role, hya_proto::Role::Assistant) {
            continue;
        }
        let mut text = String::new();
        for part in &message.parts {
            if let hya_proto::PartProjection::Text { text: t, .. } = part {
                text.push_str(t);
            }
        }
        return clamp(text);
    }
    String::new()
}

fn clamp(mut text: String) -> String {
    if text.len() <= MAX_STAGE_OUTPUT_CHARS {
        return text;
    }
    let mut end = MAX_STAGE_OUTPUT_CHARS;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

fn collect_reports(_def: &WorkflowDef, reports: Vec<Option<StageReport>>) -> Vec<StageReport> {
    reports.into_iter().flatten().collect()
}

// ---------------------------------------------------------------------------
// Loop stages
// ---------------------------------------------------------------------------

/// Drive one loop stage through the shared [`IterationDriver`]: the stored
/// first-round output counts as iteration one, further rounds resume the same
/// child session, and an independent verifier member judges each transcript.
#[allow(clippy::too_many_arguments)]
async fn drive_loop_stage(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    def: &WorkflowDef,
    ctx: &WorkflowRunContext,
    index: usize,
    verify: &VerifySpec,
    resolved_worker: &ResolvedAgent,
    resolved_verifier: ResolvedAgent,
    first_report: StageReport,
    cancel: CancellationToken,
) -> Result<StageReport, WorkflowError> {
    let worker_session = first_report
        .session
        .as_deref()
        .and_then(|s| s.parse::<SessionId>().ok());

    let executor = LoopWorkerExecutor {
        engine: engine.clone(),
        lead,
        ctx: ctx.clone(),
        worker: resolved_worker.clone(),
        session: tokio::sync::Mutex::new(worker_session),
        first: tokio::sync::Mutex::new(Some(first_report.output.clone())),
        latest_output: tokio::sync::Mutex::new(first_report.output.clone()),
        label: format!("workflow {} / {}", def.name, def.stages[index].id),
    };

    let gate = VerifierGate {
        engine,
        lead,
        ctx: ctx.clone(),
        verifier: resolved_verifier,
        until: verify.until.clone(),
        label: executor.label.clone(),
        cancel: cancel.clone(),
    };

    let caps = SafetyCaps {
        max_iterations: verify.max_iterations.max(1),
        ..SafetyCaps::default()
    };
    let outcome = IterationDriver::new(caps)
        .run(
            &executor,
            &gate,
            format!(
                "{}
Continue working toward the verified condition.",
                def.stages[index].prompt
            ),
            cancel.child_token(),
        )
        .await?;

    let mut report = first_report;
    report.output = match outcome {
        crate::completion::RunOutcome::Achieved { reason, .. } => {
            format!("{}\n\n[verified: {reason}]", clamp(executor.latest().await))
        }
        crate::completion::RunOutcome::Capped { iterations, .. } => {
            format!(
                "{}\n\n[loop capped after {iterations} iterations without verification]",
                clamp(executor.latest().await)
            )
        }
        crate::completion::RunOutcome::Cancelled => {
            report.status = StageStatus::Failed;
            format!("{}\n\n[loop cancelled]", clamp(executor.latest().await))
        }
    };
    Ok(report)
}

/// Worker-side [`IterationExecutor`]-built gate wrapper; see [`VerifierGate`].
struct LoopWorkerExecutor {
    engine: Arc<SessionEngine>,
    lead: SessionId,
    ctx: WorkflowRunContext,
    worker: ResolvedAgent,
    session: tokio::sync::Mutex<Option<SessionId>>,
    first: tokio::sync::Mutex<Option<String>>,
    latest_output: tokio::sync::Mutex<String>,
    label: String,
}

impl LoopWorkerExecutor {
    async fn latest(&self) -> String {
        self.latest_output.lock().await.clone()
    }
}

#[async_trait]
impl IterationExecutor for LoopWorkerExecutor {
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, CoreError> {
        if let Some(first) = self.first.lock().await.take() {
            *self.latest_output.lock().await = first.clone();
            return Ok(first);
        }
        let resumed = *self.session.lock().await;
        let spec = MemberSpec {
            id: MemberId::new(),
            agent: self.worker.spec.clone(),
            binding: self.ctx.binding.clone(),
            agents: self.worker.agents.clone(),
            resources: self.worker.resources.clone(),
            guidance: None,
            directive: directive.to_string(),
            tool_call: None,
            description: format!("{} (iteration)", self.label),
            session: resumed,
            sidecar_factory: self.worker.sidecar_factory.clone(),
        };
        pre_admit_team(&self.engine, self.lead, 1)
            .await
            .map_err(admission_error)?;
        let evidence = run_pre_admitted_team(
            self.engine.clone(),
            self.lead,
            vec![spec],
            cancel.child_token(),
        )
        .await;
        let Some(entry) = evidence.first() else {
            return Err(CoreError::Invalid(
                "workflow worker produced no evidence".to_string(),
            ));
        };
        if entry.status != MemberStatus::Done {
            return Err(CoreError::Cancelled);
        }
        if let Ok(session) = entry.session.parse::<SessionId>() {
            *self.session.lock().await = Some(session);
        }
        let projection = self
            .engine
            .read_projection(
                entry
                    .session
                    .parse::<SessionId>()
                    .map_err(|_| CoreError::Invalid("unparsable member session".to_string()))?,
            )
            .await?;
        let text = final_assistant_text(&projection);
        *self.latest_output.lock().await = text.clone();
        Ok(text)
    }
}

fn admission_error(error: crate::subagent::TeamAdmissionError) -> CoreError {
    CoreError::Invalid(format!("loop stage admission rejected: {error}"))
}

/// Independent stop authority: spawns a fresh verifier member per judgment and
/// parses its strict JSON verdict tolerantly (malformed ⇒ not met).
struct VerifierGate {
    engine: Arc<SessionEngine>,
    lead: SessionId,
    ctx: WorkflowRunContext,
    verifier: ResolvedAgent,
    until: String,
    label: String,
    cancel: CancellationToken,
}

#[async_trait]
impl IterationGate for VerifierGate {
    async fn judge(&self, transcript: &str) -> Result<GateOutcome, CoreError> {
        let directive = format!(
            "You are an independent verifier with no stake in the work. Judge ONLY \
             whether the condition below is satisfied by the latest worker output.\n\
             CONDITION: {}\n\nLATEST WORKER OUTPUT:\n{}\n\nRespond with ONLY a JSON \
             object: {{\"met\": true|false, \"reason\": \"...\"}}. If you cannot see \
             evidence the condition holds, answer met=false.",
            self.until,
            render_transcript_bounded(transcript)
        );
        let spec = MemberSpec {
            id: MemberId::new(),
            agent: self.verifier.spec.clone(),
            binding: self.ctx.binding.clone(),
            agents: self.verifier.agents.clone(),
            resources: self.verifier.resources.clone(),
            guidance: None,
            directive,
            tool_call: None,
            description: format!("{} verify", self.label),
            session: None,
            sidecar_factory: self.verifier.sidecar_factory.clone(),
        };
        pre_admit_team(&self.engine, self.lead, 1)
            .await
            .map_err(admission_error)?;
        let evidence = run_pre_admitted_team(
            self.engine.clone(),
            self.lead,
            vec![spec],
            self.cancel.child_token(),
        )
        .await;
        let Some(entry) = evidence.first() else {
            return Err(CoreError::Invalid(
                "verifier produced no evidence".to_string(),
            ));
        };
        if entry.status != MemberStatus::Done {
            return Err(CoreError::Cancelled);
        }
        let verdict_text = if let Ok(session) = entry.session.parse::<SessionId>() {
            final_assistant_text(&self.engine.read_projection(session).await?)
        } else {
            entry.summary.clone()
        };
        let verdict = parse_verdict(&verdict_text);
        if verdict.met {
            Ok(GateOutcome::Stop {
                reason: verdict.reason,
            })
        } else {
            Ok(GateOutcome::Continue {
                directive: format!(
                    "{}\n\nThe verifier reports the condition is not yet met: {}\n\
                     Continue working toward it.",
                    self.until, verdict.reason
                ),
            })
        }
    }
}

/// Clamp verifier-bound transcripts so judgments stay bounded.
fn render_transcript_bounded(transcript: &str) -> String {
    clamp(transcript.to_string())
}

struct Verdict {
    met: bool,
    reason: String,
}

/// Tolerant strict-JSON verdict parsing: any malformed output counts as
/// "not met" toward the cap instead of failing the whole run.
fn parse_verdict(text: &str) -> Verdict {
    let trimmed = text.trim();
    let Some(start) = trimmed.find('{') else {
        return Verdict {
            met: false,
            reason: "verifier returned malformed output".to_string(),
        };
    };
    let Some(end) = trimmed.rfind('}') else {
        return Verdict {
            met: false,
            reason: "verifier returned malformed output".to_string(),
        };
    };
    match serde_json::from_str::<serde_json::Value>(&trimmed[start..=end]) {
        Ok(value) => Verdict {
            met: value.get("met").and_then(|v| v.as_bool()).unwrap_or(false),
            reason: value
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("no reason given")
                .to_string(),
        },
        Err(_) => Verdict {
            met: false,
            reason: "verifier returned malformed output".to_string(),
        },
    }
}
