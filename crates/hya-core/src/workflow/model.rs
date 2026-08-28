//! Typed value model for user-authored workflow definitions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::WorkflowError;

/// Default iteration ceiling for a loop stage when the author omits one.
const DEFAULT_MAX_ITERATIONS: u32 = 8;

/// What happens to the rest of the graph when one stage member fails.
///
/// Declared per workflow (`on_member_failure`), which makes the join/partial-
/// failure contract part of the author's file rather than an engine default
/// nobody can see.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    /// Skip every stage that transitively depends on unfinished work.
    #[default]
    FailFast,
    /// Keep executing; declare failed upstreams in joined directives so the
    /// joining stage sees partial results and degradation explicitly.
    CollectAll,
}

impl std::fmt::Display for FailurePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FailFast => write!(f, "fail_fast"),
            Self::CollectAll => write!(f, "collect_all"),
        }
    }
}

impl std::str::FromStr for FailurePolicy {
    type Err = WorkflowError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_norway::from_str::<Self>(&format!("\"{}\"", value.trim()))
            .map_err(|_| WorkflowError::Parse(format!("unknown failure policy `{value}`")))
    }
}

/// How many times a stage's member runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StageMode {
    /// Exactly one member run (the normal case).
    #[default]
    Once,
    /// Iterate the member through [`crate::completion::IterationDriver`] until
    /// the independent verifier reports the `until` condition met or a cap
    /// fires. The stop decision always belongs to the verifier agent, never to
    /// the worker claiming success.
    Loop,
}

impl std::fmt::Display for StageMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Once => write!(f, "once"),
            Self::Loop => write!(f, "loop"),
        }
    }
}

/// Independent verifier binding for a loop stage.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifySpec {
    /// Agent id (resolved through the same caller authorization as workers)
    /// running in a FRESH child session per judgment, so verification state is
    /// independent of the worker transcript it judges.
    pub agent: String,
    /// Goal condition judged against the worker's latest output.
    pub until: String,
    /// Total worker-round ceiling including the first round already executed
    /// as part of the stage's level batch.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
}

fn default_max_iterations() -> u32 {
    DEFAULT_MAX_ITERATIONS
}

/// One node of the workflow DAG.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageDef {
    /// Unique stage id (kebab/snake identifier; referenced by `needs` and
    /// downstream templates as `{{id}}`).
    pub id: String,
    /// Agent id resolved through the caller's `can_spawn` authorization.
    pub agent: String,
    /// Prompt template for the member directive. Placeholders:
    /// `{{inputs.key}}` and `{{upstream_stage_id}}`.
    pub prompt: String,
    /// Upstream stage ids that must complete before this stage starts. Stages
    /// whose dependencies are satisfied at the same topological level execute
    /// as ONE parallel governed team batch (fan-out).
    #[serde(default)]
    pub needs: Vec<String>,
    /// Single-shot vs iterated-until-verified. Defaults to [`StageMode::Once`].
    #[serde(default)]
    pub mode: StageMode,
    /// Required when `mode: loop`; ignored otherwise (rejected as invalid).
    #[serde(default)]
    pub verify: Option<VerifySpec>,
}

/// One user-authored workflow: a named, reusable DAG of stages.
///
/// Loaded from markdown-frontmatter or YAML (see [`super::parse`]).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDef {
    /// Stable workflow name (used by discovery lookup).
    pub name: String,
    /// Human description shown by listings.
    pub description: String,
    /// Declared input keys with descriptions. Values are supplied per run via
    /// [`super::WorkflowRunContext::inputs`]; every declared key must be
    /// provided.
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    /// Join-side partial-failure contract. Defaults to fail-fast.
    #[serde(default)]
    pub on_member_failure: FailurePolicy,
    /// The stage graph (declaration order is the tie-breaker for deterministic
    /// fan-in rendering).
    pub stages: Vec<StageDef>,
}

impl WorkflowDef {
    /// Structural self-consistency checks shared by parsing and planning.
    ///
    /// Graph-level rules (dangling edges, cycles, placeholder closure) live in
    /// [`super::build_plan`].
    ///
    /// # Errors
    /// [`WorkflowError::Invalid`] naming the first violated rule.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.name.trim().is_empty() {
            return Err(self.invalid("name must not be empty"));
        }
        if self.stages.is_empty() {
            return Err(self.invalid("at least one stage is required"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for stage in &self.stages {
            if !valid_identifier(&stage.id) {
                return Err(self.invalid(format!(
                    "stage id `{}` must be a non-empty kebab/snake identifier",
                    stage.id
                )));
            }
            if !seen.insert(stage.id.as_str()) {
                return Err(self.invalid(format!("duplicate stage id `{}`", stage.id)));
            }
            if stage.agent.trim().is_empty() {
                return Err(self.invalid(format!("stage `{}` has no agent", stage.id)));
            }
            if stage.prompt.trim().is_empty() {
                return Err(self.invalid(format!("stage `{}` has an empty prompt", stage.id)));
            }
            match (stage.mode, &stage.verify) {
                (StageMode::Loop, None) => {
                    return Err(self.invalid(format!(
                        "stage `{}` sets mode: loop but declares no verify block",
                        stage.id
                    )));
                }
                (StageMode::Once, Some(_)) => {
                    return Err(self.invalid(format!(
                        "stage `{}` declares verify but mode is not loop",
                        stage.id
                    )));
                }
                (StageMode::Loop, Some(verify)) => {
                    if verify.agent.trim().is_empty() || verify.until.trim().is_empty() {
                        return Err(self.invalid(format!(
                            "stage `{}` verifier needs both agent and until",
                            stage.id
                        )));
                    }
                }
                (StageMode::Once, None) => {}
            }
        }
        Ok(())
    }

    fn invalid(&self, detail: impl Into<String>) -> WorkflowError {
        WorkflowError::Invalid {
            workflow: self.name.clone(),
            detail: detail.into(),
        }
    }
}

/// Whether `value` is a usable stage/input identifier (placeholder-safe).
fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}
