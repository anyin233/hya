//! Independent goal evaluators (`ModelGoalEvaluator` and `PluginGoalEvaluator`)
//! built strictly over [`HostRuntime`].

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{Message, MessageId, ModelRef, Part, PartId, SessionId};
use hya_runtime_abi::{HostError, HostRuntime, ModelCallRequest};
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Default verifier system prompt when no `evaluator-prompt` skill is loaded.
pub const DEFAULT_EVALUATOR_SYSTEM_PROMPT: &str = "You are an independent goal verifier. No tools.";

/// Reason recorded when an evaluator returns non-verdict or unparseable output.
pub const MALFORMED_VERDICT_REASON: &str = "evaluator returned malformed output";

/// Reason recorded when the `hya/goal:evaluate` hook provider chain fails outright.
pub const PLUGIN_HOOK_ERROR_REASON: &str = "hya/goal:evaluate hook error";

/// Local name of the hook point `hya/goal` declares for custom evaluators.
pub const EVALUATE_HOOK_POINT: &str = "evaluate";

/// Full name other bundles implement: `hya/goal:evaluate`.
pub const EVALUATE_HOOK: &str = "hya/goal:evaluate";

/// Input of the `hya/goal:evaluate` hook point.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EvaluateHookInput<'a> {
    /// Goal condition under evaluation.
    pub condition: &'a str,
    /// Rendered iteration transcript.
    pub transcript: &'a str,
}

/// Reply of the `hya/goal:evaluate` hook point: `{met, reason}`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct EvaluateHookReply {
    /// Whether the goal is met.
    pub met: bool,
    /// Short reason.
    #[serde(default)]
    pub reason: String,
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
/// Malformed model output maps to `met = false` (never a hard error) when using
/// [`ModelGoalEvaluator`] so broken evals consume budget instead of looping.
#[async_trait]
pub trait GoalEvaluator: Send + Sync {
    /// Return whether the goal is met.
    ///
    /// # Errors
    /// Propagates host/provider failures or [`HostError::Cancelled`].
    async fn evaluate(
        &self,
        condition: &str,
        transcript: &str,
        cancel: &CancellationToken,
    ) -> Result<Verdict, HostError>;
}

/// Production evaluator: a separate tool-free model call routed through
/// [`HostRuntime::model_caller`] that judges the transcript and returns strict
/// `{ "met": bool, "reason": str }`. Malformed output degrades to not-met.
pub struct ModelGoalEvaluator {
    runtime: Arc<dyn HostRuntime>,
    model: Option<ModelRef>,
    system_prompt: String,
}

impl ModelGoalEvaluator {
    /// Build an evaluator that routes to `model` (or the host default when `None`)
    /// through `runtime.model_caller()`.
    #[must_use]
    pub fn new(runtime: Arc<dyn HostRuntime>, model: Option<ModelRef>) -> Self {
        Self {
            runtime,
            model,
            system_prompt: DEFAULT_EVALUATOR_SYSTEM_PROMPT.to_string(),
        }
    }

    /// Replace the built-in verifier instructions with a bundle-owned prompt.
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Effective system prompt sent to the verifier model.
    #[must_use]
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Configured evaluator model override, if any.
    #[must_use]
    pub fn model(&self) -> Option<&ModelRef> {
        self.model.as_ref()
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
    async fn evaluate(
        &self,
        condition: &str,
        transcript: &str,
        cancel: &CancellationToken,
    ) -> Result<Verdict, HostError> {
        if cancel.is_cancelled() {
            return Err(HostError::Cancelled);
        }
        let prompt = format!(
            "## CONDITION\n{condition}\n\n## TRANSCRIPT\n{transcript}\n\nReply with ONLY \
             a JSON object: {{\"met\": true|false, \"reason\": \"...\"}}. Judge only from the \
             transcript; if you cannot see evidence the work was done, answer met=false."
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
            max_tokens: Some(256),
            reasoning_effort: None,
            json_schema: None,
            purpose: None,
        };
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(HostError::Cancelled),
            reply = self.runtime.model_caller().complete(request, cancel.clone()) => reply?,
        };
        match serde_json::from_str::<VerdictJson>(response.text.trim()) {
            Ok(v) => Ok(Verdict {
                met: v.met,
                reason: v.reason,
            }),
            Err(_) => Ok(Verdict {
                met: false,
                reason: MALFORMED_VERDICT_REASON.to_string(),
            }),
        }
    }
}

/// Goal evaluator dispatched through [`HostRuntime::extension_registry`]'s
/// `goal.evaluate` hook providers.
///
/// Mirrors [`ModelGoalEvaluator`]'s tolerance: a malformed provider reply or a
/// failing provider chain degrades to a fallback evaluator (when configured) or
/// a not-met [`Verdict`], so a broken evaluator consumes an iteration of the cap
/// instead of aborting or looping forever.
pub struct PluginGoalEvaluator {
    runtime: Arc<dyn HostRuntime>,
    session: Option<SessionId>,
    fallback: Option<Arc<dyn GoalEvaluator>>,
}

impl PluginGoalEvaluator {
    /// Build an evaluator that dispatches `goal.evaluate` hooks through `runtime`.
    #[must_use]
    pub fn new(runtime: Arc<dyn HostRuntime>, session: Option<SessionId>) -> Self {
        Self {
            runtime,
            session,
            fallback: None,
        }
    }

    /// Fall back to another [`GoalEvaluator`] (typically [`ModelGoalEvaluator`])
    /// when the hook provider fails, is absent, or returns a malformed verdict.
    #[must_use]
    pub fn with_fallback(mut self, fallback: Arc<dyn GoalEvaluator>) -> Self {
        self.fallback = Some(fallback);
        self
    }
}

#[async_trait]
impl GoalEvaluator for PluginGoalEvaluator {
    async fn evaluate(
        &self,
        condition: &str,
        transcript: &str,
        cancel: &CancellationToken,
    ) -> Result<Verdict, HostError> {
        if cancel.is_cancelled() {
            return Err(HostError::Cancelled);
        }
        let hook_result = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(HostError::Cancelled),
            res = self.runtime.extension_registry().invoke_hook(
                self.session,
                EVALUATE_HOOK,
                serde_json::json!(EvaluateHookInput { condition, transcript }),
            ) => res,
        };
        let hook_result = hook_result.map(|reply| {
            reply.and_then(|value| serde_json::from_value::<EvaluateHookReply>(value).ok())
        });
        match hook_result {
            Ok(Some(EvaluateHookReply { met, reason })) => Ok(Verdict { met, reason }),
            Ok(None) => match &self.fallback {
                Some(fallback) => fallback.evaluate(condition, transcript, cancel).await,
                None => Ok(Verdict {
                    met: false,
                    reason: MALFORMED_VERDICT_REASON.to_string(),
                }),
            },
            Err(HostError::Cancelled) => Err(HostError::Cancelled),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "hya/goal:evaluate hook failed; degrading to not-met so the run stays cappable"
                );
                match &self.fallback {
                    Some(fallback) => fallback.evaluate(condition, transcript, cancel).await,
                    None => Ok(Verdict {
                        met: false,
                        reason: PLUGIN_HOOK_ERROR_REASON.to_string(),
                    }),
                }
            }
        }
    }
}

/// Strip YAML frontmatter (`---\n...\n---\n`) from a `SKILL.md` document.
#[must_use]
pub fn skill_prompt_body(content: &str) -> &str {
    content
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n").map(|(_, body)| body.trim()))
        .unwrap_or(content)
}

/// Extract a non-empty trimmed string from `value` at a single key or nested
/// `goal.<key>` section.
#[must_use]
pub(crate) fn extract_config_string(value: &Value, key: &str) -> Option<String> {
    value
        .get("goal")
        .and_then(|section| section.get(key))
        .or_else(|| value.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Resolve the goal evaluator model following precedence:
/// explicit CLI/request override > bundle `config.yml` `goal.evaluator_model`
/// > worker/default model.
#[must_use]
pub fn resolve_evaluator_model<'a>(
    cli_flag: Option<&'a str>,
    config_value: Option<&'a str>,
    worker_current: Option<&'a str>,
) -> Option<&'a str> {
    let non_empty = |value: Option<&'a str>| value.map(str::trim).filter(|m| !m.is_empty());
    non_empty(cli_flag)
        .or_else(|| non_empty(config_value))
        .or_else(|| non_empty(worker_current))
}
