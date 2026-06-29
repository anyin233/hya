use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::SessionId;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::completion::{
    GateOutcome, IterationDriver, IterationExecutor, IterationGate, RunOutcome, SafetyCaps,
    render_transcript,
};
use crate::engine::{AgentSpec, CreateSession, SessionEngine};
use crate::error::CoreError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceQuality {
    Missing,
    ClaimOnly,
    Supported,
    Verified,
}

#[derive(Clone, Debug)]
pub struct VerifierVerdict {
    pub score: u8,
    pub satisfied: bool,
    pub evidence_quality: EvidenceQuality,
    pub critical_gaps: Vec<String>,
    pub iteration_summary: String,
    pub reason: String,
}

#[async_trait]
pub trait LoopVerifier: Send + Sync {
    async fn grade(&self, target: &str, transcript: &str) -> Result<VerifierVerdict, CoreError>;
}

#[derive(Clone, Debug)]
pub struct PlannerOutput {
    pub directive: String,
    pub continuity_brief: String,
    pub planner_notes: String,
    pub strategy_change: bool,
    pub change_note: String,
}

#[async_trait]
pub trait LoopPlanner: Send + Sync {
    async fn plan_next(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        planner_notes: &str,
    ) -> Result<PlannerOutput, CoreError>;
}

#[derive(Clone, Copy, Debug)]
pub struct LoopConfig {
    pub budget: u32,
    pub stop_when_satisfied: bool,
    pub satisfaction_threshold: u8,
    pub max_no_progress: u32,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            budget: 10,
            stop_when_satisfied: true,
            satisfaction_threshold: 90,
            max_no_progress: 3,
        }
    }
}

const HARD_MAX_ITERATIONS: u32 = 100;

/// Reject an unservable loop before any worker runs (engine authority): an
/// explicit budget within the hard ceiling. Returns a worst-case token estimate.
pub fn cost_preflight(config: &LoopConfig) -> Result<u64, CoreError> {
    if config.budget == 0 || config.budget > HARD_MAX_ITERATIONS {
        return Err(CoreError::Invalid(format!(
            "loop budget must be 1..={HARD_MAX_ITERATIONS}, got {}",
            config.budget
        )));
    }
    let per_iteration = 500_000u64 + 256 + 2_000;
    Ok(u64::from(config.budget) * per_iteration)
}

fn fingerprint(items: &[String]) -> u64 {
    let mut h = DefaultHasher::new();
    items.hash(&mut h);
    h.finish()
}

#[derive(Default)]
struct LoopState {
    history: Vec<String>,
    planner_notes: String,
    recent_directive_fps: Vec<u64>,
    last_gap_fp: Option<u64>,
    no_progress: u32,
}

pub struct LoopGate {
    target: String,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    config: LoopConfig,
    state: Mutex<LoopState>,
}

impl LoopGate {
    #[must_use]
    pub fn new(
        target: String,
        verifier: Arc<dyn LoopVerifier>,
        planner: Arc<dyn LoopPlanner>,
        config: LoopConfig,
    ) -> Self {
        Self {
            target,
            verifier,
            planner,
            config,
            state: Mutex::new(LoopState::default()),
        }
    }
}

#[async_trait]
impl IterationGate for LoopGate {
    async fn judge(&self, transcript: &str) -> Result<GateOutcome, CoreError> {
        let mut st = self.state.lock().await;
        let verdict = self.verifier.grade(&self.target, transcript).await?;
        st.history.push(verdict.iteration_summary.clone());

        // Engine authority: only the verifier (not the planner) can declare success.
        if self.config.stop_when_satisfied
            && verdict.satisfied
            && verdict.score >= self.config.satisfaction_threshold
            && verdict.critical_gaps.is_empty()
            && verdict.evidence_quality >= EvidenceQuality::Supported
        {
            return Ok(GateOutcome::Stop {
                reason: format!("satisfied: score {}", verdict.score),
            });
        }

        let gap_fp = fingerprint(&verdict.critical_gaps);
        if self.config.max_no_progress > 0 {
            if st.last_gap_fp == Some(gap_fp) {
                st.no_progress += 1;
            } else {
                st.no_progress = 1;
                st.last_gap_fp = Some(gap_fp);
            }
            if st.no_progress >= self.config.max_no_progress {
                return Ok(GateOutcome::Stop {
                    reason: format!("no progress for {} iterations", self.config.max_no_progress),
                });
            }
        }

        let notes = st.planner_notes.clone();
        let plan = self
            .planner
            .plan_next(&self.target, &st.history, &verdict, &notes)
            .await?;

        let directive_fp = fingerprint(std::slice::from_ref(&plan.directive));
        if st.recent_directive_fps.contains(&directive_fp) && !plan.strategy_change {
            return Ok(GateOutcome::Stop {
                reason: "repeated directive without strategy change".to_string(),
            });
        }
        st.recent_directive_fps.push(directive_fp);
        if st.recent_directive_fps.len() > 2 {
            st.recent_directive_fps.remove(0);
        }
        st.planner_notes = plan.planner_notes;

        Ok(GateOutcome::Continue {
            directive: format!("{}\n\n{}", plan.directive, plan.continuity_brief),
        })
    }
}

pub struct WorkerSessionExecutor {
    engine: Arc<SessionEngine>,
    lead_session: SessionId,
    agent: AgentSpec,
}

#[async_trait]
impl IterationExecutor for WorkerSessionExecutor {
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, CoreError> {
        let child = self
            .engine
            .create(CreateSession {
                parent: Some(self.lead_session),
                agent: self.agent.name.clone(),
                model: self.agent.model.clone(),
                workdir: self.agent.workdir.to_string_lossy().into_owned(),
            })
            .await?;
        self.engine
            .admit_user_prompt(child, directive.to_string())
            .await?;
        self.engine
            .run_turn(child, &self.agent, cancel.clone())
            .await?;
        let projection = self.engine.read_projection(child).await?;
        Ok(render_transcript(&projection))
    }
}

pub async fn drive_loop(
    executor: &dyn IterationExecutor,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    target: String,
    config: LoopConfig,
    cancel: CancellationToken,
) -> Result<RunOutcome, CoreError> {
    let gate = LoopGate::new(target.clone(), verifier, planner, config);
    let caps = SafetyCaps {
        max_iterations: config.budget,
        ..SafetyCaps::default()
    };
    IterationDriver::new(caps)
        .run(executor, &gate, target, cancel)
        .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_loop(
    engine: Arc<SessionEngine>,
    lead_session: SessionId,
    agent: AgentSpec,
    target: String,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    config: LoopConfig,
    cancel: CancellationToken,
) -> Result<RunOutcome, CoreError> {
    cost_preflight(&config)?;
    let executor = WorkerSessionExecutor {
        engine,
        lead_session,
        agent,
    };
    drive_loop(&executor, verifier, planner, target, config, cancel).await
}
