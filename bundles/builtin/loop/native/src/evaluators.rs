//! Model-backed and hook-backed loop verifiers, planners, and `loop.should_stop`
//! consults built strictly on [`hya_runtime_abi::HostRuntime`].

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{Message, MessageId, ModelRef, Part, PartId, SessionId};
use hya_runtime_abi::{HostError, HostRuntime, ModelCallRequest, NativeLoopPlanInput};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::types::{
    EvidenceQuality, LoopPlanner, LoopShouldStopHook, LoopVerifier, PlannerOutput, VerifierVerdict,
};

/// Default system prompt for [`ModelLoopVerifier`]: an independent grader with
/// no stake in the work and no tools.
pub const DEFAULT_LOOP_VERIFIER_SYSTEM: &str = "You are an independent loop verifier. You have no stake \
     in the work and no tools. Grade only the target against the transcript you are given.";

/// Default system prompt for [`ModelLoopPlanner`].
pub const DEFAULT_LOOP_PLANNER_SYSTEM: &str =
    "You are an independent loop planner. No tools. Plan only from the inputs you are given.";

/// Neutral directive used when the planner's reply is unusable: the loop keeps
/// making progress instead of aborting or repeating a broken plan.
const NEUTRAL_LOOP_DIRECTIVE: &str = "Continue working toward the target. Re-check what the \
     verifier flagged as missing, keep changes small and verifiable, and summarize evidence.";

/// Reason recorded when the verifier's reply is not a parseable verdict.
const VERIFIER_MALFORMED_REASON: &str = "verifier returned malformed output";

/// Tolerant strict-JSON object extraction: pulls the outermost `{...}` span out
/// of a reply that may be fenced or prose-wrapped.
fn extract_json_object(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (end >= start).then_some(&trimmed[start..=end])
}

/// The not-satisfied verdict every tolerance path degrades to.
fn not_satisfied(reason: String) -> VerifierVerdict {
    VerifierVerdict {
        score: 0,
        satisfied: false,
        evidence_quality: EvidenceQuality::Missing,
        critical_gaps: Vec::new(),
        iteration_summary: String::new(),
        reason,
    }
}

/// The neutral continue plan every planner tolerance path degrades to.
fn neutral_plan() -> PlannerOutput {
    PlannerOutput {
        directive: NEUTRAL_LOOP_DIRECTIVE.to_string(),
        continuity_brief: String::new(),
        planner_notes: String::new(),
        strategy_change: false,
        change_note: String::new(),
    }
}

/// Production verifier: a separate model call with NO tools through
/// [`HostRuntime::model_caller`] that grades the target against the transcript
/// and replies with strict JSON.
///
/// Tolerance contract: a malformed reply or failing provider degrades to a
/// not-satisfied verdict with score 0 and [`EvidenceQuality::Missing`] so a
/// broken verdict counts against no-progress/cap machinery instead of aborting
/// or falsely satisfying the run.
pub struct ModelLoopVerifier {
    runtime: Arc<dyn HostRuntime>,
    model: Option<ModelRef>,
    system_prompt: String,
}

impl ModelLoopVerifier {
    /// Build a verifier that calls `model` through `runtime.model_caller()`.
    #[must_use]
    pub fn new(runtime: Arc<dyn HostRuntime>, model: Option<ModelRef>) -> Self {
        Self {
            runtime,
            model,
            system_prompt: DEFAULT_LOOP_VERIFIER_SYSTEM.to_string(),
        }
    }

    /// Replace the built-in verifier instructions with a bundle-owned prompt.
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }
}

#[derive(Deserialize)]
struct VerifierReplyJson {
    #[serde(default)]
    score: Option<u8>,
    #[serde(default)]
    satisfied: Option<bool>,
    #[serde(default)]
    evidence_quality: Option<String>,
    #[serde(default)]
    critical_gaps: Option<Vec<String>>,
    #[serde(default)]
    iteration_summary: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[async_trait]
impl LoopVerifier for ModelLoopVerifier {
    async fn grade(&self, target: &str, transcript: &str) -> Result<VerifierVerdict, HostError> {
        let prompt = format!(
            "## TARGET\n{target}\n\n## TRANSCRIPT\n{transcript}\n\nReply with ONLY a JSON \
             object: {{\"score\": 0-100, \"satisfied\": true|false, \"evidence_quality\": \
             \"missing\"|\"claim_only\"|\"supported\"|\"verified\", \"critical_gaps\": [\"...\"], \
             \"iteration_summary\": \"...\", \"reason\": \"...\"}}. Judge only from the \
             transcript; if you cannot see evidence the target is met, answer satisfied=false."
        );
        let request = ModelCallRequest {
            model: self.model.clone(),
            system: Some(self.system_prompt.clone()),
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: prompt,
                }],
            }],
            tools: Vec::new(),
            temperature: Some(0.0),
            max_tokens: Some(1024),
            reasoning_effort: None,
            json_schema: None,
            purpose: None,
        };
        let response = match self
            .runtime
            .model_caller()
            .complete(request, CancellationToken::new())
            .await
        {
            Ok(response) => response,
            Err(HostError::Cancelled) => return Err(HostError::Cancelled),
            Err(error) => {
                return Ok(not_satisfied(format!("verifier provider failed: {error}")));
            }
        };
        let Some(json) = extract_json_object(&response.text) else {
            return Ok(not_satisfied(VERIFIER_MALFORMED_REASON.to_string()));
        };
        match serde_json::from_str::<VerifierReplyJson>(json) {
            Ok(reply) => Ok(VerifierVerdict {
                score: reply.score.unwrap_or(0),
                satisfied: reply.satisfied.unwrap_or(false),
                evidence_quality: EvidenceQuality::from_wire(reply.evidence_quality.as_deref()),
                critical_gaps: reply.critical_gaps.unwrap_or_default(),
                iteration_summary: reply.iteration_summary.unwrap_or_default(),
                reason: reply
                    .reason
                    .unwrap_or_else(|| "no reason given".to_string()),
            }),
            Err(_) => Ok(not_satisfied(VERIFIER_MALFORMED_REASON.to_string())),
        }
    }
}

/// Production planner: a separate model call with NO tools through
/// [`HostRuntime::model_caller`] that plans the next directive from history and
/// the last verdict.
///
/// Tolerance contract: a malformed reply or failing provider degrades to a
/// neutral continue directive so the loop keeps making progress and remains
/// cappable instead of erroring the run.
pub struct ModelLoopPlanner {
    runtime: Arc<dyn HostRuntime>,
    model: Option<ModelRef>,
    system_prompt: String,
}

impl ModelLoopPlanner {
    /// Build a planner that calls `model` through `runtime.model_caller()`.
    #[must_use]
    pub fn new(runtime: Arc<dyn HostRuntime>, model: Option<ModelRef>) -> Self {
        Self {
            runtime,
            model,
            system_prompt: DEFAULT_LOOP_PLANNER_SYSTEM.to_string(),
        }
    }

    /// Replace the built-in planner instructions with a bundle-owned prompt.
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }
}

#[derive(Deserialize)]
struct PlannerReplyJson {
    #[serde(default)]
    directive: Option<String>,
    #[serde(default)]
    continuity_brief: Option<String>,
    #[serde(default)]
    planner_notes: Option<String>,
    #[serde(default)]
    strategy_change: Option<bool>,
    #[serde(default)]
    change_note: Option<String>,
}

#[async_trait]
impl LoopPlanner for ModelLoopPlanner {
    async fn plan_next(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        planner_notes: &str,
    ) -> Result<PlannerOutput, HostError> {
        let prompt = format!(
            "## TARGET\n{target}\n\n## ITERATION SUMMARIES\n{}\n\n## LAST VERDICT\nscore={} \
             satisfied={} evidence_quality={:?} gaps={:?} reason={}\n\n## PLANNER NOTES\n\
             {planner_notes}\n\nReply with ONLY a JSON object: {{\"directive\": \"...\", \
             \"continuity_brief\": \"...\", \"planner_notes\": \"...\", \"strategy_change\": \
             true|false}}. Plan the single next step that closes the largest gap.",
            history.join("\n"),
            last.score,
            last.satisfied,
            last.evidence_quality,
            last.critical_gaps,
            last.reason,
        );
        let request = ModelCallRequest {
            model: self.model.clone(),
            system: Some(self.system_prompt.clone()),
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: prompt,
                }],
            }],
            tools: Vec::new(),
            temperature: Some(0.0),
            max_tokens: Some(1024),
            reasoning_effort: None,
            json_schema: None,
            purpose: None,
        };
        let response = match self
            .runtime
            .model_caller()
            .complete(request, CancellationToken::new())
            .await
        {
            Ok(response) => response,
            Err(HostError::Cancelled) => return Err(HostError::Cancelled),
            Err(error) => {
                tracing::warn!(%error, "loop planner provider failed; planning neutrally");
                return Ok(neutral_plan());
            }
        };
        let Some(json) = extract_json_object(&response.text) else {
            return Ok(neutral_plan());
        };
        match serde_json::from_str::<PlannerReplyJson>(json) {
            Ok(reply) => {
                let Some(directive) = reply.directive.filter(|d| !d.trim().is_empty()) else {
                    return Ok(neutral_plan());
                };
                Ok(PlannerOutput {
                    directive,
                    continuity_brief: reply.continuity_brief.unwrap_or_default(),
                    planner_notes: reply.planner_notes.unwrap_or_default(),
                    strategy_change: reply.strategy_change.unwrap_or(false),
                    change_note: reply.change_note.unwrap_or_default(),
                })
            }
            Err(_) => Ok(neutral_plan()),
        }
    }
}

/// Host-hook loop verifier (`loop.verifier`) with fallback when unregistered or
/// failing.
pub struct HookLoopVerifier {
    runtime: Arc<dyn HostRuntime>,
    session: SessionId,
    fallback: Arc<dyn LoopVerifier>,
}

impl HookLoopVerifier {
    /// Try host `loop.verifier` hooks first, then fall back to `fallback`.
    #[must_use]
    pub fn new(
        runtime: Arc<dyn HostRuntime>,
        session: SessionId,
        fallback: Arc<dyn LoopVerifier>,
    ) -> Self {
        Self {
            runtime,
            session,
            fallback,
        }
    }
}

#[async_trait]
impl LoopVerifier for HookLoopVerifier {
    async fn grade(&self, target: &str, transcript: &str) -> Result<VerifierVerdict, HostError> {
        match self
            .runtime
            .extension_registry()
            .invoke_loop_verify(self.session, target, transcript)
            .await
        {
            Ok(Some(native)) => Ok(VerifierVerdict::from_native(native)),
            Ok(None) => self.fallback.grade(target, transcript).await,
            Err(error) => {
                tracing::warn!(%error, "loop.verifier hook failed; using model fallback");
                self.fallback.grade(target, transcript).await
            }
        }
    }
}

/// Host-hook loop planner (`loop.planner`) with fallback when unregistered or
/// failing.
pub struct HookLoopPlanner {
    runtime: Arc<dyn HostRuntime>,
    session: SessionId,
    fallback: Arc<dyn LoopPlanner>,
}

impl HookLoopPlanner {
    /// Try host `loop.planner` hooks first, then fall back to `fallback`.
    #[must_use]
    pub fn new(
        runtime: Arc<dyn HostRuntime>,
        session: SessionId,
        fallback: Arc<dyn LoopPlanner>,
    ) -> Self {
        Self {
            runtime,
            session,
            fallback,
        }
    }
}

#[async_trait]
impl LoopPlanner for HookLoopPlanner {
    async fn plan_next(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        planner_notes: &str,
    ) -> Result<PlannerOutput, HostError> {
        let input = NativeLoopPlanInput {
            target: target.to_string(),
            history: history.to_vec(),
            last: last.to_native(),
            planner_notes: planner_notes.to_string(),
        };
        match self
            .runtime
            .extension_registry()
            .invoke_loop_plan(self.session, input)
            .await
        {
            Ok(Some(native)) => Ok(PlannerOutput {
                directive: native.directive,
                continuity_brief: native.continuity_brief,
                planner_notes: native.planner_notes,
                strategy_change: native.strategy_change,
                change_note: native.change_note,
            }),
            Ok(None) => {
                self.fallback
                    .plan_next(target, history, last, planner_notes)
                    .await
            }
            Err(error) => {
                tracing::warn!(%error, "loop.planner hook failed; using model fallback");
                self.fallback
                    .plan_next(target, history, last, planner_notes)
                    .await
            }
        }
    }
}

/// [`LoopShouldStopHook`] backed by [`HostRuntime::extension_registry`].
pub struct RuntimeShouldStopHook {
    runtime: Arc<dyn HostRuntime>,
    session: SessionId,
}

impl RuntimeShouldStopHook {
    /// Create a fail-open `loop.should_stop` consult bound to `session`.
    #[must_use]
    pub fn new(runtime: Arc<dyn HostRuntime>, session: SessionId) -> Self {
        Self { runtime, session }
    }
}

#[async_trait]
impl LoopShouldStopHook for RuntimeShouldStopHook {
    async fn should_stop(&self, target: &str, transcript: &str) -> Option<String> {
        match self
            .runtime
            .extension_registry()
            .invoke_loop_should_stop(self.session, target, transcript)
            .await
        {
            Ok(reason) => reason,
            Err(error) => {
                tracing::warn!(%error, "loop.should_stop hook failed; continuing loop");
                None
            }
        }
    }
}
