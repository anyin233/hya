//! Workflow execution over the governed subagent team primitives.
//!
//! Ready Stages register parked resident actors and activate them through the
//! episode API as soon as their predecessors complete. The controller accepts at-rest settlement or
//! a terminal report as Stage evidence. Loop continuations reactivate the same
//! actor; each independent verifier runs a fresh, capacity-counted episode.

use std::collections::BTreeMap;
use std::sync::Arc;

use hya_proto::{ActorClaim, SessionId, WorkflowRunId};
use hya_workflow::CompiledWorkflow;
use serde::Serialize;

use crate::engine::{AgentSpec, SessionEngine};
use crate::resident::ResidentSupervisor;

use self::preflight::ResolvedAgent;
use super::WorkflowRoutingContext;

mod preflight;
mod scheduling;
mod stage;

pub use preflight::{prepare_workflow_run, prepare_workflow_run_for_actor, run_workflow};

/// Per-run inputs for executing a workflow.
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
    /// Resident scheduling owner required for every Stage activation.
    pub resident_supervisor: Option<Arc<ResidentSupervisor>>,
    /// Configured category/provider context for explicit Stage route admission.
    /// `None` preserves direct core callers and the old no-assignment path.
    pub routing: Option<WorkflowRoutingContext>,
}
/// Terminal status of one workflow stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    /// The Stage has not started.
    Pending,
    /// The Stage finished successfully.
    Done,
    /// The Stage finished with an execution failure.
    Failed,
    /// The activated Stage was cancelled.
    Cancelled,
    /// The Stage never activated (failure policy, predecessor, or cancellation).
    Skipped,
}

impl std::fmt::Display for StageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Done => write!(f, "done"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Skipped => write!(f, "skipped"),
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
    /// Bounded terminal output or failure detail; empty while pending/skipped.
    pub output: String,
}

/// Overall terminal state of a workflow run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    /// Every Stage of the DAG ran successfully.
    Completed,
    /// At least one Stage failed.
    Failed,
    /// The run was cancelled.
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
    /// One row per compiled Stage; unstarted Stages are skipped.
    pub stages: Vec<StageReport>,
}

#[cfg(test)]
struct StageConnectionHold {
    hold_stage: &'static str,
    fast_stage: &'static str,
    both_activated: Arc<tokio::sync::Barrier>,
    tx_acquired: Arc<tokio::sync::Notify>,
    release_tx: Arc<tokio::sync::Notify>,
}

/// Fully preflighted Workflow execution.
///
/// Preflight creates no child Session or mail and reserves no live capacity.
pub struct PreparedWorkflowRun {
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: CompiledWorkflow,
    context: WorkflowRunContext,
    resolved: Vec<ResolvedAgent>,
    verifiers: Vec<Option<ResolvedAgent>>,
    durable_run: Option<WorkflowRunId>,
    actor_claim: Option<ActorClaim>,
    #[cfg(test)]
    stage_hold: Option<Arc<StageConnectionHold>>,
}

#[cfg(test)]
impl PreparedWorkflowRun {
    fn with_stage_hold(mut self, hold: Arc<StageConnectionHold>) -> Self {
        self.stage_hold = Some(hold);
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{CreateSession, EventBus};
    use hya_proto::Event;
    use hya_proto::{AgentName, ModelRef, SessionKind};
    use hya_provider::{DevProvider, ProviderRouter};
    use hya_store::SessionStore;
    use hya_tool::{PermissionPlane, PermissionRules};
    use hya_workflow::{WorkflowSource, compile};
    use tokio_util::sync::CancellationToken;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_stage_holding_sole_store_connection_does_not_self_deadlock() {
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        let engine = Arc::new(SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
            crate::test_support::runtime(hya_tool_loader::builtins()),
            permission,
            EventBus::default(),
        ));
        let lead = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("hya-main"),
                model: ModelRef::new("hya/offline"),
                workdir: ".".into(),
                project: None,
                kind: SessionKind::Project,
            })
            .await
            .unwrap();
        let supervisor = ResidentSupervisor::start(engine.clone());
        let binding = engine.bind_runtime(std::path::Path::new(".")).unwrap();
        let base_agent = AgentSpec {
            name: AgentName::new("hya-main"),
            model: ModelRef::new("hya/offline"),
            workdir: ".".into(),
            system_prompt: String::new(),
            reasoning: None,
        };
        let source = r#"---
kind: Workflow
name: self-deadlock-pin
description: One stage holds the sole store connection while another finishes.
nodes:
  hold:
    agent: hya-scout
    directive: Hold the single store connection cleanly.
  fast:
    agent: hya-scout
    directive: Finish first so the coordinator requests a store write.
---
flowchart TD
  hold
  fast
"#;
        let workflow = compile(WorkflowSource::new("self-deadlock.hya.md", source)).unwrap();
        let hold = Arc::new(StageConnectionHold {
            hold_stage: "hold",
            fast_stage: "fast",
            both_activated: Arc::new(tokio::sync::Barrier::new(2)),
            tx_acquired: Arc::new(tokio::sync::Notify::new()),
            release_tx: Arc::new(tokio::sync::Notify::new()),
        });
        let prepared = prepare_workflow_run(
            engine.clone(),
            lead,
            &workflow,
            WorkflowRunContext {
                binding,
                caller: "hya-main".to_string(),
                base_agent,
                inputs: BTreeMap::new(),
                resident_supervisor: Some(supervisor.clone()),
                routing: None,
            },
            Some(WorkflowRunId::new()),
        )
        .await
        .unwrap()
        .with_stage_hold(hold);

        let report = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            prepared.execute(CancellationToken::new()),
        )
        .await
        .expect(
            "workflow coordinator must not self-deadlock when a sibling holds the sole connection",
        )
        .unwrap();

        assert_eq!(report.status, WorkflowStatus::Completed);
        assert_eq!(report.stages.len(), 2);
        assert!(
            report
                .stages
                .iter()
                .all(|stage| stage.status == StageStatus::Done),
            "unexpected stage reports: {:#?}",
            report.stages
        );
        let finished_stages: Vec<String> = engine
            .replay(lead)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|env| match env.event {
                Event::WorkflowStageFinished { stage, .. } => Some(stage),
                _ => None,
            })
            .collect();
        assert!(finished_stages.contains(&"fast".to_string()));
        assert!(finished_stages.contains(&"hold".to_string()));
        supervisor
            .drain(hya_proto::FinishCause::Shutdown, crate::DRAIN_DEADLINE)
            .await;
    }
}
