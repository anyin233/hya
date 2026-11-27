use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hya_proto::{ActorClaim, ModelRef, SessionId, WorkflowModelAssignment, WorkflowRunId};
use hya_provider::ReasoningEffort;
use hya_tool::AgentDef;
use hya_workflow::{CompiledWorkflow, StageMode, WorkflowStage};
use tokio_util::sync::CancellationToken;

use crate::AgentResourcePolicy;
use crate::engine::{AgentSpec, SessionEngine};
use crate::error::CoreError;
use crate::sidecar::BoundSidecarFactory;

use super::super::{WorkflowError, WorkflowModelRoute};
use super::{PreparedWorkflowRun, WorkflowRunContext, WorkflowRunReport};

/// Execution context of one stage agent, computed up front so authorization
/// failures surface before any member spawns. Mirrors
/// `resolve_authorized_spawn_member` in the task-tool path: the target's own
/// spec, spawnable roster, resource policy, and bound sidecar factory.
#[derive(Clone)]
pub(super) struct ResolvedAgent {
    pub(super) spec: AgentSpec,
    pub(super) agents: Arc<[AgentDef]>,
    pub(super) resources: AgentResourcePolicy,
    pub(super) sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
    pub(super) route: Option<WorkflowModelRoute>,
}

fn resolve_agent(
    engine: &SessionEngine,
    ctx: &WorkflowRunContext,
    router: &hya_provider::ProviderRouter,
    agent_id: &str,
    workflow: &str,
    role: &str,
    assignment: Option<&WorkflowModelAssignment>,
) -> Result<ResolvedAgent, WorkflowError> {
    let definition = ctx
        .binding
        .resolve_workflow_agent(&ctx.caller, agent_id)
        .map_err(|error| WorkflowError::Unauthorized {
            caller: ctx.caller.clone(),
            agent_id: agent_id.to_string(),
            detail: error.to_string(),
        })?;
    let mut spec =
        engine.agent_spec_for_binding(&ctx.binding, &ctx.base_agent, definition.stable_id)?;

    let is_servable = |model: &ModelRef| router.resolve(model).is_some();
    spec = crate::category::apply_agent_model_preference(
        spec,
        &definition,
        ctx.binding.agent_model_preference(definition.stable_id),
        &is_servable,
    );
    if let Some(routing) = ctx.routing.as_ref() {
        let member = hya_tool::SpawnMember::default();

        spec = crate::category::apply_spawn_model_policy(
            spec,
            &definition,
            &member,
            &routing.categories,
            &is_servable,
        );
    }
    let route = assignment
        .map(|assignment| resolve_model_route(workflow, agent_id, role, assignment, router))
        .transpose()?;
    if let Some(route) = &route {
        let selected = route.selected().ok_or_else(|| WorkflowError::Invalid {
            workflow: workflow.to_string(),
            detail: format!("Stage `{agent_id}` {role} route selected no candidate"),
        })?;
        // An explicit assignment replaces inherited Agent effort. Keep the
        // effective value on the child spec as well as the request-local route.
        spec.model = selected.model.clone();
        spec.reasoning = Some(selected.reasoning);
    }
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
        .map_err(|error| unauthorized(error.to_string()))?;
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
        route,
    })
}

/// Resolve one authored assignment to typed efforts and an immutable start index.
fn resolve_model_route(
    workflow: &str,
    stage: &str,
    role: &str,
    assignment: &WorkflowModelAssignment,
    router: &hya_provider::ProviderRouter,
) -> Result<WorkflowModelRoute, WorkflowError> {
    let mut candidates = Vec::with_capacity(1 + assignment.fallback().len());
    let mut seen = BTreeSet::new();
    let entries = std::iter::once((assignment.id(), assignment.reasoning())).chain(
        assignment
            .fallback()
            .iter()
            .map(|candidate| (candidate.id(), candidate.reasoning())),
    );
    for (entry_index, (model_id, authored_reasoning)) in entries.enumerate() {
        if model_id.contains('#') || model_id.trim().is_empty() {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{stage}` {role} route has invalid model id at index {entry_index}"
                ),
            });
        }
        let model = ModelRef::new(model_id);
        let reasoning = match authored_reasoning {
            Some(label) => ReasoningEffort::parse(label).ok_or_else(|| WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{stage}` {role} route has unknown reasoning effort `{label}` at index {entry_index}"
                ),
            })?,
            None => router
                .reasoning_default(&model)
                .unwrap_or(ReasoningEffort::Off),
        };
        if !seen.insert((model.to_string(), reasoning)) {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{stage}` {role} route contains duplicate effective candidate `{model_id}` / `{}`",
                    reasoning.as_str()
                ),
            });
        }
        if let Some(supported) = router.supports_reasoning_effort(&model, reasoning) {
            if !supported {
                return Err(WorkflowError::Invalid {
                    workflow: workflow.to_string(),
                    detail: format!(
                        "Stage `{stage}` {role} candidate `{model_id}` does not support reasoning `{}`",
                        reasoning.as_str()
                    ),
                });
            }
        } else if reasoning != ReasoningEffort::Off
            && router
                .capabilities(&model)
                .is_some_and(|caps| !caps.reasoning_request)
        {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{stage}` {role} candidate `{model_id}` does not support reasoning requests"
                ),
            });
        }
        candidates.push(crate::workflow::WorkflowModelRouteCandidate { model, reasoning });
    }
    let Some(selected_index) = candidates
        .iter()
        .position(|candidate| router.resolve(&candidate.model).is_some())
    else {
        return Err(WorkflowError::Invalid {
            workflow: workflow.to_string(),
            detail: format!("Stage `{stage}` {role} route has no routable candidate"),
        });
    };
    Ok(WorkflowModelRoute {
        candidates: candidates.into(),
        selected_index,
    })
}

/// Preflight and reserve one compiled Workflow before any child or mail effect.
///
/// # Errors
/// Returns validation, authorization, semantic, lineage, or admission failures.
pub async fn prepare_workflow_run(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: &CompiledWorkflow,
    context: WorkflowRunContext,
    durable_run: Option<WorkflowRunId>,
) -> Result<PreparedWorkflowRun, WorkflowError> {
    prepare_workflow_run_for_actor(engine, lead, workflow, context, durable_run, None).await
}

/// Preflight a Workflow while retaining an optional actor capability for every
/// durable child and lifecycle mutation.
///
/// # Errors
/// Returns the same validation, authorization, semantic, lineage, or admission
/// failures as [`prepare_workflow_run`].
pub async fn prepare_workflow_run_for_actor(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: &CompiledWorkflow,
    context: WorkflowRunContext,
    durable_run: Option<WorkflowRunId>,
    actor_claim: Option<ActorClaim>,
) -> Result<PreparedWorkflowRun, WorkflowError> {
    workflow.validate_inputs(&context.inputs)?;
    let plan = workflow.plan();
    let name = workflow.definition().name();
    let wanted = worst_case_admission_units(plan.stages());
    let router = context.routing.as_ref().map_or_else(
        || engine.provider_router(),
        |routing| Arc::clone(&routing.router),
    );

    let mut resolved = Vec::with_capacity(plan.stages().len());
    let mut verifiers = Vec::with_capacity(plan.stages().len());
    for stage in plan.stages() {
        resolved.push(resolve_agent(
            &engine,
            &context,
            &router,
            stage.agent(),
            name,
            "worker",
            stage.model(),
        )?);
        verifiers.push(
            stage
                .verify()
                .map(|verify| {
                    resolve_agent(
                        &engine,
                        &context,
                        &router,
                        verify.agent(),
                        name,
                        "verifier",
                        verify.model(),
                    )
                })
                .transpose()?,
        );
    }
    validate_resolved_semantics(name, plan.stages(), &resolved, &context)?;
    preflight_workflow_capacity(&engine, lead, name, wanted).await?;
    Ok(PreparedWorkflowRun {
        engine,
        lead,
        workflow: workflow.clone(),
        context,
        resolved,
        verifiers,
        durable_run,
        actor_claim,
        #[cfg(test)]
        stage_hold: None,
    })
}

/// Execute `workflow` under `lead` after shared preflight.
///
/// # Errors
/// Returns validation, authorization, admission, or execution failures.
pub async fn run_workflow(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: &CompiledWorkflow,
    context: WorkflowRunContext,
    cancel: CancellationToken,
) -> Result<WorkflowRunReport, WorkflowError> {
    prepare_workflow_run(engine, lead, workflow, context, None)
        .await?
        .execute(cancel)
        .await
}

fn worst_case_admission_units(stages: &[WorkflowStage]) -> u64 {
    stages.iter().fold(0_u64, |total, stage| {
        let units = match stage.verify() {
            Some(verify) => u64::from(verify.max_iterations()).saturating_mul(2),
            None => 1,
        };
        total.saturating_add(units)
    })
}

/// Check the compiled worst-case activation count before child or mail effects.
/// Registration and revival still enforce capacity in their own transactions.
async fn preflight_workflow_capacity(
    engine: &SessionEngine,
    lead: SessionId,
    workflow: &str,
    wanted: u64,
) -> Result<(), WorkflowError> {
    let Some(governor) = engine.governor().cloned() else {
        return Ok(());
    };
    let (root, depth) = engine.session_lineage(lead).await?;
    if depth.saturating_add(1) > governor.max_depth() {
        return Err(WorkflowError::MaxDepth);
    }
    let limit = governor.limits().per_run_budget;
    if wanted > limit {
        return Err(WorkflowError::Invalid {
            workflow: workflow.to_string(),
            detail: format!(
                "worst-case {wanted} Stage activations exceed the per-run budget ({limit})"
            ),
        });
    }
    use crate::store_bridge::SessionStoreExt as _;
    let live = engine.store().live_member_count(root).await?;
    if wanted > limit.saturating_sub(live) {
        return Err(CoreError::CapacityExceeded {
            requested: wanted,
            limit,
        }
        .into());
    }
    Ok(())
}

fn validate_resolved_semantics(
    workflow: &str,
    stages: &[WorkflowStage],
    resolved: &[ResolvedAgent],
    ctx: &WorkflowRunContext,
) -> Result<(), WorkflowError> {
    if ctx.resident_supervisor.is_none() {
        return Err(WorkflowError::Invalid {
            workflow: workflow.to_string(),
            detail: "every Workflow Stage requires a resident supervisor".into(),
        });
    }
    let mut actor_routes: BTreeMap<&str, Option<&WorkflowModelRoute>> = BTreeMap::new();
    for (stage, worker) in stages.iter().zip(resolved) {
        if let Some(actor) = stage.actor() {
            if let Some(existing) = actor_routes.get(actor) {
                if *existing != worker.route.as_ref() {
                    return Err(WorkflowError::Invalid {
                        workflow: workflow.to_string(),
                        detail: format!(
                            "Stages sharing actor `{actor}` must use an identical effective worker route"
                        ),
                    });
                }
            } else {
                actor_routes.insert(actor, worker.route.as_ref());
            }
        }
        // Every Agent is a resident actor (0.41.0): a Stage's `actor` key,
        // not the Agent definition, selects a persistent actor Session; a
        // Stage without one runs a fresh one-shot member Session.
        if stage.actor().is_some() && stage.mode() == StageMode::Loop {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!("Stage `{}` cannot combine actor and loop modes", stage.id()),
            });
        }
    }
    Ok(())
}
