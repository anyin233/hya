//! Goal-mode iteration: independent evaluator, safety caps, and lead-turn executor.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use hya_proto::{
    Event, Message, MessageId, ModelRef, Part, PartId, PartProjection, Projection, SessionId,
};
use hya_provider::{CompletionRequest, ProviderRouter};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::engine::{AgentSpec, SessionEngine};
use crate::error::CoreError;

/// Hard stops for goal/loop iteration drivers.
#[derive(Clone, Copy, Debug)]
pub struct SafetyCaps {
    /// Maximum worker iterations before `Capped`.
    pub max_iterations: u32,
    /// Wall-clock budget for the whole run.
    pub max_wall_clock: Duration,
    /// Reserved token budget field (driver may use for future preflight).
    pub max_tokens: u64,
}

impl Default for SafetyCaps {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            max_wall_clock: Duration::from_secs(1800),
            max_tokens: 2_000_000,
        }
    }
}

/// Terminal status of a goal/loop run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// Gate reported stop (goal met or loop satisfied).
    Achieved {
        /// Iterations completed including the final one.
        iterations: u32,
        /// Gate reason string.
        reason: String,
    },
    /// Hit a safety cap before success.
    Capped {
        /// Iterations completed.
        iterations: u32,
        /// Which cap fired (`max_iterations`, `max_wall_clock`, …).
        which: &'static str,
    },
    /// Cancellation token tripped.
    Cancelled,
}

/// Decision from an [`IterationGate`] after one transcript.
pub enum GateOutcome {
    /// Stop the driver successfully.
    Stop {
        /// Reason recorded on [`RunOutcome::Achieved`].
        reason: String,
    },
    /// Run another worker iteration with this directive.
    Continue {
        /// Next prompt/directive for the executor.
        directive: String,
    },
}

/// Independent stop authority for iterative runs (goal or loop).
///
/// **Contract:** Called after each executor iteration with the rendered
/// transcript. Must not mutate session state. Errors abort the whole run.
#[async_trait]
pub trait IterationGate: Send + Sync {
    /// Judge whether to stop or continue.
    ///
    /// # Errors
    /// Return [`CoreError`] to fail the run.
    async fn judge(&self, transcript: &str) -> Result<GateOutcome, CoreError>;
}

/// Runs one agent iteration given a directive string.
///
/// **Contract:** Implementors admit work and return a transcript string for the
/// gate. Must honour `cancel`. Errors abort the driver.
#[async_trait]
pub trait IterationExecutor: Send + Sync {
    /// Execute one iteration and return a transcript for judging.
    ///
    /// # Errors
    /// Propagate turn/store failures.
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, CoreError>;
}

/// Drives executor/gate pairs under [`SafetyCaps`].
pub struct IterationDriver {
    /// Caps applied each loop.
    pub caps: SafetyCaps,
}

impl IterationDriver {
    /// Create a driver with the given caps.
    #[must_use]
    pub fn new(caps: SafetyCaps) -> Self {
        Self { caps }
    }

    /// Run until the gate stops, a cap trips, or cancel fires.
    ///
    /// # Errors
    /// Propagates executor/gate errors.
    pub async fn run(
        &self,
        executor: &dyn IterationExecutor,
        gate: &dyn IterationGate,
        initial_directive: String,
        cancel: CancellationToken,
    ) -> Result<RunOutcome, CoreError> {
        let start = Instant::now();
        let mut directive = initial_directive;
        let mut iterations = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Ok(RunOutcome::Cancelled);
            }
            if iterations >= self.caps.max_iterations {
                return Ok(RunOutcome::Capped {
                    iterations,
                    which: "max_iterations",
                });
            }
            if start.elapsed() >= self.caps.max_wall_clock {
                return Ok(RunOutcome::Capped {
                    iterations,
                    which: "max_wall_clock",
                });
            }
            iterations += 1;
            let transcript = executor.run_iteration(&directive, &cancel).await?;
            match gate.judge(&transcript).await? {
                GateOutcome::Stop { reason } => {
                    return Ok(RunOutcome::Achieved { iterations, reason });
                }
                GateOutcome::Continue { directive: next } => directive = next,
            }
        }
    }
}

/// Result of an independent goal evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// Whether the goal condition is satisfied by the transcript.
    pub met: bool,
    /// Short reason for the model and logs.
    pub reason: String,
}

/// Independent goal judge (must not be the worker that claims success).
///
/// **Contract:** Evaluate only the provided condition + transcript. No tools.
/// Malformed model output should map to `met = false`, not a hard error, when
/// using [`ModelGoalEvaluator`].
#[async_trait]
pub trait GoalEvaluator: Send + Sync {
    /// Return whether the goal is met.
    ///
    /// # Errors
    /// Propagate provider/runtime failures.
    async fn evaluate(&self, condition: &str, transcript: &str) -> Result<Verdict, CoreError>;
}

/// [`IterationGate`] that wraps a [`GoalEvaluator`].
pub struct GoalGate {
    condition: String,
    evaluator: Arc<dyn GoalEvaluator>,
}

#[async_trait]
impl IterationGate for GoalGate {
    async fn judge(&self, transcript: &str) -> Result<GateOutcome, CoreError> {
        let verdict = self.evaluator.evaluate(&self.condition, transcript).await?;
        if verdict.met {
            Ok(GateOutcome::Stop {
                reason: verdict.reason,
            })
        } else {
            Ok(GateOutcome::Continue {
                directive: format!(
                    "{}\n\nThe goal is not yet met: {}\nContinue working toward it.",
                    self.condition, verdict.reason
                ),
            })
        }
    }
}

/// [`IterationExecutor`] that admits a user prompt and runs one lead turn.
pub struct LeadTurnExecutor {
    engine: Arc<SessionEngine>,
    session: SessionId,
    agent: AgentSpec,
}

#[async_trait]
impl IterationExecutor for LeadTurnExecutor {
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, CoreError> {
        self.engine
            .admit_user_prompt(self.session, directive.to_string())
            .await?;
        self.engine
            .run_turn(self.session, &self.agent, cancel.clone())
            .await?;
        let projection = self.engine.read_projection(self.session).await?;
        Ok(render_transcript(&projection))
    }
}

/// Render a simple role-tagged transcript string from a session projection.
#[must_use]
pub fn render_transcript(projection: &Projection) -> String {
    let mut s = String::new();
    for m in &projection.session.messages {
        let mut text = String::new();
        for p in &m.parts {
            if let PartProjection::Text { text: t, .. } = p {
                text.push_str(t);
            }
        }
        s.push_str(&format!("[{:?}] {}\n", m.role, text));
    }
    s
}

/// Run goal mode: loop the lead session until the independent evaluator reports
/// the condition met, or a cap trips. The evaluator judges only the transcript.
pub async fn run_goal(
    engine: Arc<SessionEngine>,
    session: SessionId,
    agent: AgentSpec,
    condition: String,
    evaluator: Arc<dyn GoalEvaluator>,
    caps: SafetyCaps,
    cancel: CancellationToken,
) -> Result<RunOutcome, CoreError> {
    let executor = LeadTurnExecutor {
        engine,
        session,
        agent,
    };
    let gate = GoalGate {
        condition: condition.clone(),
        evaluator,
    };
    IterationDriver::new(caps)
        .run(&executor, &gate, condition, cancel)
        .await
}

/// Production evaluator: a separate cheap-model call with NO tools that judges the
/// transcript and returns strict `{ "met": bool, "reason": str }`. Malformed
/// output is treated as not-met (so a bad eval counts toward the cap, never loops).
pub struct ModelGoalEvaluator {
    providers: Arc<ProviderRouter>,
    model: ModelRef,
}

impl ModelGoalEvaluator {
    /// Build an evaluator that routes to `model` through `providers`.
    #[must_use]
    pub fn new(providers: Arc<ProviderRouter>, model: ModelRef) -> Self {
        Self { providers, model }
    }
}

#[derive(Deserialize)]
struct VerdictJson {
    met: bool,
    #[serde(default)]
    reason: String,
}

#[async_trait]
impl GoalEvaluator for ModelGoalEvaluator {
    async fn evaluate(&self, condition: &str, transcript: &str) -> Result<Verdict, CoreError> {
        let prompt = format!(
            "## CONDITION\n{condition}\n\n## TRANSCRIPT\n{transcript}\n\nReply with ONLY \
             a JSON object: {{\"met\": true|false, \"reason\": \"...\"}}. Judge only from the \
             transcript; if you cannot see evidence the work was done, answer met=false."
        );
        let request = CompletionRequest {
            model: self.model.clone(),
            system: Some("You are an independent goal verifier. No tools.".to_string()),
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: prompt,
                }],
            }],
            tools: Vec::new(),
            temperature: Some(0.0),
            max_output_tokens: Some(256),
            reasoning: None,
            headers: Default::default(),
        };
        let mut stream = self
            .providers
            .stream(request, SessionId::new(), MessageId::new())
            .await?;
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            if let Event::TextDelta { delta, .. } = item? {
                text.push_str(&delta);
            }
        }
        match serde_json::from_str::<VerdictJson>(text.trim()) {
            Ok(v) => Ok(Verdict {
                met: v.met,
                reason: v.reason,
            }),
            Err(_) => Ok(Verdict {
                met: false,
                reason: "evaluator returned malformed output".to_string(),
            }),
        }
    }
}
