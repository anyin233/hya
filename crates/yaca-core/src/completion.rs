use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use yaca_proto::{
    Event, Message, MessageId, ModelRef, Part, PartId, PartProjection, Projection, SessionId,
};
use yaca_provider::{CompletionRequest, ProviderRouter};

use crate::engine::{AgentSpec, SessionEngine};
use crate::error::CoreError;

#[derive(Clone, Copy, Debug)]
pub struct SafetyCaps {
    pub max_iterations: u32,
    pub max_wall_clock: Duration,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Achieved {
        iterations: u32,
        reason: String,
    },
    Capped {
        iterations: u32,
        which: &'static str,
    },
    Cancelled,
}

pub enum GateOutcome {
    Stop { reason: String },
    Continue { directive: String },
}

#[async_trait]
pub trait IterationGate: Send + Sync {
    async fn judge(&self, transcript: &str) -> Result<GateOutcome, CoreError>;
}

#[async_trait]
pub trait IterationExecutor: Send + Sync {
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, CoreError>;
}

pub struct IterationDriver {
    pub caps: SafetyCaps,
}

impl IterationDriver {
    #[must_use]
    pub fn new(caps: SafetyCaps) -> Self {
        Self { caps }
    }

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub met: bool,
    pub reason: String,
}

#[async_trait]
pub trait GoalEvaluator: Send + Sync {
    async fn evaluate(&self, condition: &str, transcript: &str) -> Result<Verdict, CoreError>;
}

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
