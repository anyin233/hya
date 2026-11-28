//! Application setup for headless goal and loop drivers.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use hya_engine::AgentSpec;
use hya_proto::{Message, ModelRef, SessionId};
use hya_provider::ProviderRouter;
use hya_runtime_abi::{
    DriverRunOutcome, GoalDriver, GoalRunRequest, HostBundleStorage, HostCatalogReader, HostError,
    HostEventBus, HostExtensionRegistry, HostModelCaller, HostRuntime, HostSessionStore,
    HostTurnRunner, LoopDriver, LoopRunRequest, ModelCallRequest, ModelCallResponse,
    ModelStreamChunk, ModelWindowInfo,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config;
use crate::host_runtime::complete_with_router;

/// Default iteration budget for `hya loop` runs.
pub const DEFAULT_LOOP_BUDGET: u32 = 10;

/// Maximum iteration budget accepted by `hya loop` (`1..=100`).
pub const HARD_MAX_LOOP_BUDGET: u32 = 100;

/// Clamp a requested loop budget into the `1..=100` range enforced by `hya/loop`.
#[must_use]
pub fn clamp_loop_budget(requested: u32) -> u32 {
    requested.clamp(1, HARD_MAX_LOOP_BUDGET)
}

/// Direction of a deterministic loop-exit predicate (`--while` / `--until`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopPredicateMode {
    /// Exit 0 continues the loop; exit 1 stops it.
    While,
    /// Exit 0 stops the loop; exit 1 continues it.
    Until,
}

/// Serializable deterministic loop predicate forwarded to `hya/loop`'s [`LoopDriver`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopPredicateSpec {
    /// Shell command executed via `sh -c <command>`.
    pub command: String,
    /// Evaluation direction (`while` or `until`).
    pub mode: LoopPredicateMode,
    /// Working directory where the command executes.
    pub workdir: PathBuf,
}

/// Headless loop options supplied by callers of [`prepare_loop`].
#[derive(Clone, Debug)]
pub struct LoopRunOptions {
    /// Maximum worker iterations (`1..=100`).
    pub budget: u32,
    /// Maximum wall-clock duration in seconds.
    pub max_wall_clock_secs: u64,
    /// Stop when verifier marks satisfied above threshold.
    pub stop_when_satisfied: bool,
    /// Minimum verifier score treated as satisfied (`0..=100`).
    pub satisfaction_threshold: u8,
    /// Consecutive no-progress iterations before stopping as stalled.
    pub max_no_progress: u32,
    /// Optional deterministic `--while` / `--until` predicate.
    pub predicate: Option<LoopPredicateSpec>,
}

impl Default for LoopRunOptions {
    fn default() -> Self {
        Self {
            budget: DEFAULT_LOOP_BUDGET,
            max_wall_clock_secs: 1800,
            stop_when_satisfied: true,
            satisfaction_threshold: 90,
            max_no_progress: 3,
            predicate: None,
        }
    }
}

/// Error returned when `hya -p` is invoked without the `hya/goal` native bundle loaded.
pub const GOAL_DRIVER_UNAVAILABLE_MSG: &str = "goal mode (`hya -p`) requires the `hya/goal` \
    native bundle, which is unavailable in `--bare` mode or when `hya/goal` is disabled in \
    `config.yaml` (`bundles.builtin`); re-run without `--bare` or enable `hya/goal` to use \
    goal mode";

/// Route used by the model evaluator, verifier and planner for a headless run.
pub struct HeadlessEvaluatorRoute<'a> {
    /// Provider router for the independent judgments.
    pub router: &'a ProviderRouter,
    /// Explicit model override, when supplied by the host.
    pub model_override: Option<&'a str>,
    /// Model already resolved from config or the worker route.
    pub default_model: &'a str,
}

/// Resolve the registered native [`GoalDriver`] from `host_runtime`, failing
/// with an actionable error when `hya/goal` is disabled or running in `--bare`.
///
/// # Errors
/// Returns [`GOAL_DRIVER_UNAVAILABLE_MSG`] when no [`GoalDriver`] is registered.
pub fn resolve_goal_driver(host_runtime: &dyn HostRuntime) -> anyhow::Result<Arc<dyn GoalDriver>> {
    host_runtime
        .goal_driver()
        .ok_or_else(|| anyhow::anyhow!("{GOAL_DRIVER_UNAVAILABLE_MSG}"))
}

/// Prepare a cancellable goal-mode run backed by the registered native
/// [`GoalDriver`] (`hya/goal`).
///
/// # Errors
/// Returns an actionable error when `hya/goal` is disabled or unavailable
/// (for example under `--bare`).
pub fn prepare_goal_driver(
    host_runtime: Arc<dyn HostRuntime>,
    request: GoalRunRequest,
    cancel: CancellationToken,
) -> anyhow::Result<impl Future<Output = Result<DriverRunOutcome, HostError>> + use<>> {
    let driver = resolve_goal_driver(host_runtime.as_ref())?;
    Ok(async move { driver.run_goal(host_runtime, request, cancel).await })
}

/// Wraps a [`HostRuntime`] so verifier/planner model calls use the caller's
/// explicit evaluator [`ProviderRouter`] while delegating all other host
/// capabilities unchanged.
struct EvaluatorRouterHostRuntime {
    inner: Arc<dyn HostRuntime>,
    evaluator_router: Arc<ProviderRouter>,
    default_model: ModelRef,
}

#[async_trait]
impl HostModelCaller for EvaluatorRouterHostRuntime {
    async fn complete(
        &self,
        request: ModelCallRequest,
        cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        complete_with_router(&self.evaluator_router, &self.default_model, request, cancel).await
    }

    async fn stream(
        &self,
        request: ModelCallRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
        self.inner.model_caller().stream(request, cancel).await
    }

    async fn compact_window(
        &self,
        model: &ModelRef,
        messages: &[Message],
    ) -> Result<Option<String>, HostError> {
        self.inner
            .model_caller()
            .compact_window(model, messages)
            .await
    }

    async fn model_window(&self, model: &ModelRef) -> Result<Option<ModelWindowInfo>, HostError> {
        self.inner.model_caller().model_window(model).await
    }
}

impl HostRuntime for EvaluatorRouterHostRuntime {
    fn session_store(&self) -> &dyn HostSessionStore {
        self.inner.session_store()
    }

    fn bundle_storage(&self) -> &dyn HostBundleStorage {
        self.inner.bundle_storage()
    }

    fn turn_runner(&self) -> &dyn HostTurnRunner {
        self.inner.turn_runner()
    }

    fn model_caller(&self) -> &dyn HostModelCaller {
        self
    }

    fn event_bus(&self) -> &dyn HostEventBus {
        self.inner.event_bus()
    }

    fn catalog_reader(&self) -> &dyn HostCatalogReader {
        self.inner.catalog_reader()
    }

    fn extension_registry(&self) -> &dyn HostExtensionRegistry {
        self.inner.extension_registry()
    }

    fn goal_driver(&self) -> Option<Arc<dyn GoalDriver>> {
        self.inner.goal_driver()
    }

    fn loop_driver(&self) -> Option<Arc<dyn LoopDriver>> {
        self.inner.loop_driver()
    }
}

/// Bind loop resources and prepare a cancellable [`LoopDriver`] run backed by
/// the `hya/loop` native bundle via [`HostRuntime::loop_driver`].
///
/// Returns a clear, actionable error when `hya/loop` is not loaded (for
/// example under `--bare` or when `hya/loop` is disabled in `config.yaml`).
///
/// # Errors
/// Returns an error if `hya/loop` is unavailable or `configure` fails.
pub async fn prepare_loop<F>(
    host_runtime: Arc<dyn HostRuntime>,
    session: SessionId,
    agent: AgentSpec,
    route: HeadlessEvaluatorRoute<'_>,
    target: String,
    configure: F,
    cancel: CancellationToken,
) -> anyhow::Result<impl Future<Output = Result<DriverRunOutcome, HostError>> + use<F>>
where
    F: FnOnce(&Path) -> anyhow::Result<LoopRunOptions>,
{
    let Some(driver) = host_runtime.loop_driver() else {
        anyhow::bail!(
            "`hya loop` requires the `hya/loop` built-in bundle, which is currently disabled \
             (running in `--bare` mode or `hya/loop` is disabled in `config.yaml`). \
             Remove `--bare` or enable `hya/loop` under `bundles.builtin` in `config.yaml` \
             to run loop mode."
        );
    };
    let evaluator_router = Arc::new(route.router.clone());
    let evaluator_model =
        config::resolve_evaluator_model(route.model_override, None, route.default_model)
            .to_string();
    let options = configure(&agent.workdir)?;
    let predicate = options
        .predicate
        .map(serde_json::to_value)
        .transpose()
        .context("serialize loop predicate")?;
    let request = LoopRunRequest {
        session,
        target,
        budget: options.budget,
        max_wall_clock_secs: options.max_wall_clock_secs,
        satisfaction_threshold: options.satisfaction_threshold,
        max_no_progress: options.max_no_progress,
        predicate,
        agent: Some(agent.name.clone()),
        model: Some(agent.model.clone()),
        config: json!({
            "evaluator_model": evaluator_model,
            "stop_when_satisfied": options.stop_when_satisfied,
            "workdir": agent.workdir,
        }),
    };
    let loop_runtime: Arc<dyn HostRuntime> = Arc::new(EvaluatorRouterHostRuntime {
        inner: host_runtime,
        evaluator_router,
        default_model: ModelRef::new(evaluator_model),
    });
    Ok(async move { driver.run_loop(loop_runtime, request, cancel).await })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_goal_driver_reports_actionable_error_when_unavailable() {
        let noop = hya_tool_loader::noop_host_runtime();
        let Err(err) = resolve_goal_driver(noop.as_ref()) else {
            panic!("missing goal driver must error");
        };
        let err = err.to_string();
        assert!(err.contains("hya/goal"), "{err}");
        assert!(err.contains("--bare"), "{err}");
    }
}
