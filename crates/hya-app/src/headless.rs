//! Application setup for headless goal and loop drivers.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use hya_engine::completion::PluginGoalEvaluator;
use hya_engine::hooks::HookDispatcher;
use hya_engine::loop_mode::{LoopConfig, ModelLoopPlanner, ModelLoopVerifier, run_loop};
use hya_engine::{
    AgentSpec, CoreError, GoalEvaluator, HookChain, HookLoopPlanner, HookLoopVerifier,
    ModelGoalEvaluator, RunOutcome, SafetyCaps, SessionEngine, TurnBinding, run_goal,
};
use hya_proto::{ModelRef, SessionId};
use hya_provider::ProviderRouter;
use tokio_util::sync::CancellationToken;

use crate::config;

/// Route used by the model evaluator, verifier and planner for a headless run.
pub struct HeadlessEvaluatorRoute<'a> {
    /// Provider router for the independent judgments.
    pub router: &'a ProviderRouter,
    /// Explicit model override, when supplied by the host.
    pub model_override: Option<&'a str>,
    /// Model already resolved from config or the worker route.
    pub default_model: &'a str,
}

/// Bind the goal evaluator and prepare the cancellable goal driver.
///
/// Await setup first, then drive the returned future under the host's cancellation
/// and shutdown policy.
///
/// # Errors
/// Returns an error if the evaluator resources cannot be bound.
pub async fn prepare_goal(
    engine: Arc<SessionEngine>,
    session: SessionId,
    agent: AgentSpec,
    route: HeadlessEvaluatorRoute<'_>,
    goal: String,
    max_iterations: u32,
    cancel: CancellationToken,
) -> anyhow::Result<impl Future<Output = Result<RunOutcome, CoreError>> + use<>> {
    let evaluator_router = route.router.clone();
    let evaluator_model =
        config::resolve_evaluator_model(route.model_override, None, route.default_model);
    let binding = engine
        .bind_root_runtime(&agent.workdir)
        .await
        .context("bind goal evaluator resources")?;
    // Selection point (design §4.5): a registered `goal.evaluate` hook
    // provider outranks the built-in evaluator. The probe is the trait
    // capability probe (default false), so any dispatcher without the hook —
    // or a selection that cannot confirm registration — fails open to the D7
    // model evaluator instead of erroring startup.
    let fallback: Arc<dyn GoalEvaluator> = Arc::new(
        ModelGoalEvaluator::new(Arc::new(evaluator_router), ModelRef::new(evaluator_model))
            .with_system_prompt(
                binding
                    .bundle_skill_content("hya/goal-loop", "evaluator-prompt")
                    .map(skill_prompt_body)
                    .unwrap_or("You are an independent goal verifier. No tools."),
            ),
    );
    let dispatcher = goal_loop_hook_chain(&binding, agent.name.as_str());
    let evaluator: Arc<dyn GoalEvaluator> = if dispatcher.has_goal_evaluate() {
        Arc::new(PluginGoalEvaluator::new(dispatcher).with_fallback(Arc::clone(&fallback)))
    } else {
        fallback
    };
    let caps = SafetyCaps {
        max_iterations,
        ..SafetyCaps::default()
    };
    Ok(run_goal(
        engine, session, agent, goal, evaluator, caps, cancel,
    ))
}

/// Bind loop resources and prepare the cancellable verifier/planner driver.
///
/// `configure` resolves the host's loop options for the agent workdir after
/// binding resources. Drive the returned future under the host's cancellation
/// and shutdown policy.
///
/// # Errors
/// Returns an error if resource binding or configuration fails.
pub async fn prepare_loop<F>(
    engine: Arc<SessionEngine>,
    session: SessionId,
    agent: AgentSpec,
    route: HeadlessEvaluatorRoute<'_>,
    target: String,
    configure: F,
    cancel: CancellationToken,
) -> anyhow::Result<impl Future<Output = Result<RunOutcome, CoreError>> + use<F>>
where
    F: FnOnce(&Path) -> anyhow::Result<LoopConfig>,
{
    let evaluator_router = route.router.clone();
    let evaluator_model =
        config::resolve_evaluator_model(route.model_override, None, route.default_model);
    let binding = engine
        .bind_root_runtime(&agent.workdir)
        .await
        .context("bind loop evaluator resources")?;
    let loop_config = configure(&agent.workdir)?;
    // The engine-supplied `loop.should_stop` consult: registered plugin
    // providers may force a legitimate stop; without registrations the
    // dispatcher answers `None` and the gate falls through untouched.
    let evaluator_dispatcher = goal_loop_hook_chain(&binding, agent.name.as_str());
    let verifier_fallback: Arc<dyn hya_engine::LoopVerifier> = Arc::new(
        ModelLoopVerifier::new(
            Arc::new(evaluator_router.clone()),
            ModelRef::new(evaluator_model),
        )
        .with_system_prompt(
            binding
                .bundle_skill_content("hya/goal-loop", "loop-verifier-prompt")
                .map(skill_prompt_body)
                .unwrap_or("You are an independent loop verifier. You have no stake in the work and no tools."),
        ),
    );
    let planner_fallback: Arc<dyn hya_engine::LoopPlanner> = Arc::new(
        ModelLoopPlanner::new(Arc::new(evaluator_router), ModelRef::new(evaluator_model))
            .with_system_prompt(
                binding
                    .bundle_skill_content("hya/goal-loop", "loop-planner-prompt")
                    .map(skill_prompt_body)
                    .unwrap_or("You are an independent loop planner. No tools."),
            ),
    );
    let should_stop = Arc::clone(&evaluator_dispatcher);
    Ok(run_loop(
        engine,
        session,
        agent,
        target,
        Arc::new(HookLoopVerifier::new(
            Arc::clone(&evaluator_dispatcher),
            verifier_fallback,
        )),
        Arc::new(HookLoopPlanner::new(evaluator_dispatcher, planner_fallback)),
        loop_config,
        cancel,
        Some(should_stop),
    ))
}

fn skill_prompt_body(content: &str) -> &str {
    content
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n").map(|(_, body)| body.trim()))
        .unwrap_or(content)
}

fn goal_loop_hook_chain(binding: &TurnBinding, stable_agent_id: &str) -> Arc<dyn HookDispatcher> {
    let active_is_goal_loop = binding
        .resolve_agent(stable_agent_id)
        .is_some_and(|agent| agent.origin.bundle_id() == Some("hya/goal-loop"));
    // The goal-loop owner is appended unfiltered below; retain global/plugin hooks.
    let selected = binding.bundle_hooks_for_agent(if active_is_goal_loop {
        hya_engine::MAIN_AGENT_ID
    } else {
        stable_agent_id
    });
    let agentless = binding
        .bundle_catalog()
        .bundles()
        .iter()
        .filter(|bundle| {
            (bundle.agents().is_empty() && bundle.workflow().is_none())
                && bundle.identity().id != "hya/goal-loop"
        })
        .filter_map(|bundle| binding.bundle_hooks(&bundle.identity().id))
        .collect();
    Arc::new(HookChain::new(ordered_evaluator_hooks(
        binding.global_plugin_hooks(),
        agentless,
        selected,
        binding.bundle_hooks("hya/goal-loop"),
    )))
}

fn ordered_evaluator_hooks(
    global: Vec<Arc<dyn HookDispatcher>>,
    agentless: Vec<Arc<dyn HookDispatcher>>,
    selected: Vec<Arc<dyn HookDispatcher>>,
    intelligent: Option<Arc<dyn HookDispatcher>>,
) -> Vec<Arc<dyn HookDispatcher>> {
    let mut hooks = Vec::new();
    for hook in global
        .into_iter()
        .chain(agentless)
        .chain(selected)
        .chain(intelligent)
    {
        if !hooks.iter().any(|existing| Arc::ptr_eq(existing, &hook)) {
            hooks.push(hook);
        }
    }
    hooks
}

#[cfg(test)]
mod tests {
    use super::*;
    use hya_engine::hooks::NoopHookHost;

    #[test]
    fn evaluator_hook_order_is_global_agentless_selected_intelligent_and_deduplicated() {
        let global: Arc<dyn HookDispatcher> = Arc::new(NoopHookHost);
        let agentless: Arc<dyn HookDispatcher> = Arc::new(NoopHookHost);
        let selected: Arc<dyn HookDispatcher> = Arc::new(NoopHookHost);
        let intelligent: Arc<dyn HookDispatcher> = Arc::new(NoopHookHost);
        let ordered = ordered_evaluator_hooks(
            vec![Arc::clone(&global)],
            vec![Arc::clone(&agentless)],
            vec![Arc::clone(&selected), Arc::clone(&intelligent)],
            Some(Arc::clone(&intelligent)),
        );
        assert_eq!(ordered.len(), 4);
        assert!(Arc::ptr_eq(&ordered[0], &global));
        assert!(Arc::ptr_eq(&ordered[1], &agentless));
        assert!(Arc::ptr_eq(&ordered[2], &selected));
        assert!(Arc::ptr_eq(&ordered[3], &intelligent));
    }
}
