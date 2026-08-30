//! Immutable output model produced by the Workflow compiler.

use std::collections::BTreeMap;

use serde::Deserialize;
/// Maximum Unicode scalar values in a Workflow route model id.
pub const MAX_WORKFLOW_MODEL_ID_CHARS: usize = 256;
/// Maximum Unicode scalar values in an authored Workflow reasoning label.
pub const MAX_WORKFLOW_REASONING_CHARS: usize = 64;

/// What a Workflow does after any Stage fails.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    /// Stop after every activation in the admitted level settles.
    #[default]
    FailFast,
    /// Continue eligible Stages, while retaining failed predecessor evidence.
    CollectAll,
}

impl std::fmt::Display for FailurePolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::FailFast => "fail_fast",
            Self::CollectAll => "collect_all",
        })
    }
}

/// Validated Workflow metadata shared by all execution surfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowDefinition {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) inputs: BTreeMap<String, String>,
    pub(crate) on_failure: FailurePolicy,
}

impl WorkflowDefinition {
    /// Stable author-declared Workflow name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Human-readable Workflow description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Required runtime input names and their descriptions.
    #[must_use]
    pub fn inputs(&self) -> &BTreeMap<String, String> {
        &self.inputs
    }

    /// Author-selected failed-Stage policy.
    #[must_use]
    pub const fn on_failure(&self) -> FailurePolicy {
        self.on_failure
    }
}

/// Number of times a Stage activation runs.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StageMode {
    /// Run the Stage once.
    #[default]
    Once,
    /// Re-enter the Stage through the governed iteration driver until verified.
    Loop,
}

impl std::fmt::Display for StageMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Once => "once",
            Self::Loop => "loop",
        })
    }
}

/// One model candidate in a Workflow assignment's ordered fallback chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowModelCandidate {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) reasoning: Option<String>,
}

impl WorkflowModelCandidate {
    /// Base model reference without a Workflow variant suffix.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Author-provided reasoning effort label, before runtime capability parsing.
    #[must_use]
    pub fn reasoning(&self) -> Option<&str> {
        self.reasoning.as_deref()
    }
}

/// Preferred model and ordered fallback candidates for one Workflow role.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowModelAssignment {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) reasoning: Option<String>,
    #[serde(default)]
    pub(crate) fallback: Vec<WorkflowModelCandidate>,
}

impl WorkflowModelAssignment {
    /// Preferred base model reference without a Workflow variant suffix.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Preferred author-provided reasoning effort label.
    #[must_use]
    pub fn reasoning(&self) -> Option<&str> {
        self.reasoning.as_deref()
    }

    /// Ordered fallback tail after the preferred model.
    #[must_use]
    pub fn fallback(&self) -> &[WorkflowModelCandidate] {
        &self.fallback
    }
}

/// Independent verifier contract for one loop Stage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VerifySpec {
    pub(crate) agent: String,
    pub(crate) until: String,
    #[serde(default = "default_max_iterations")]
    pub(crate) max_iterations: u32,
    #[serde(default)]
    pub(crate) model: Option<WorkflowModelAssignment>,
}

impl VerifySpec {
    /// Agent id used as independent stop authority.
    #[must_use]
    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// Condition judged against the latest worker evidence.
    #[must_use]
    pub fn until(&self) -> &str {
        &self.until
    }

    /// Total worker-round ceiling, including the first activation.
    #[must_use]
    pub const fn max_iterations(&self) -> u32 {
        self.max_iterations
    }

    /// Optional model route used by the independent verifier.
    #[must_use]
    pub const fn model(&self) -> Option<&WorkflowModelAssignment> {
        self.model.as_ref()
    }
}

fn default_max_iterations() -> u32 {
    8
}

/// One validated Stage in first graph-occurrence order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowStage {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) agent: String,
    pub(crate) directive: String,
    pub(crate) level: usize,
    pub(crate) mode: StageMode,
    pub(crate) verify: Option<VerifySpec>,
    pub(crate) model: Option<WorkflowModelAssignment>,
    pub(crate) actor: Option<String>,
    pub(crate) predecessor_indices: Vec<usize>,
}

impl WorkflowStage {
    /// Workflow-local graph identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Optional human-readable Stage title.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Target Agent identifier.
    #[must_use]
    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// Author directive before input and predecessor evidence rendering.
    #[must_use]
    pub fn directive(&self) -> &str {
        &self.directive
    }

    /// Direct predecessors in compiled join order.
    #[must_use]
    pub fn predecessor_indices(&self) -> &[usize] {
        &self.predecessor_indices
    }

    /// Zero-based topological execution level.
    #[must_use]
    pub const fn level(&self) -> usize {
        self.level
    }

    /// Single-shot or independently verified loop execution.
    #[must_use]
    pub const fn mode(&self) -> StageMode {
        self.mode
    }

    /// Independent verifier declaration for a loop Stage.
    #[must_use]
    pub const fn verify(&self) -> Option<&VerifySpec> {
        self.verify.as_ref()
    }

    /// Optional request-local model route used by the worker activation.
    #[must_use]
    pub const fn model(&self) -> Option<&WorkflowModelAssignment> {
        self.model.as_ref()
    }
    /// Workflow-local resident identity, when explicitly declared.
    #[must_use]
    pub fn actor(&self) -> Option<&str> {
        self.actor.as_deref()
    }
}

/// One parallel topological level of a compiled Workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowLevel {
    pub(crate) stage_indices: Vec<usize>,
}

impl WorkflowLevel {
    /// Stage indices into [`WorkflowPlan::stages`] in stable graph order.
    #[must_use]
    pub fn stage_indices(&self) -> &[usize] {
        &self.stage_indices
    }
}

/// Validated, deterministic Stage graph ready for runtime execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowPlan {
    pub(crate) stages: Vec<WorkflowStage>,
    pub(crate) levels: Vec<WorkflowLevel>,
}

impl WorkflowPlan {
    /// Stages in first graph-occurrence order.
    #[must_use]
    pub fn stages(&self) -> &[WorkflowStage] {
        &self.stages
    }

    /// Parallel topological levels in execution order.
    #[must_use]
    pub fn levels(&self) -> &[WorkflowLevel] {
        &self.levels
    }

    /// Resolve a Workflow-local Stage id to its stable plan index.
    #[must_use]
    pub fn stage_index(&self, id: &str) -> Option<usize> {
        self.stages.iter().position(|stage| stage.id == id)
    }
}

/// Domain-separated digest of one normalized compiled Workflow.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkflowRevision(pub(crate) [u8; 32]);

impl WorkflowRevision {
    /// Raw SHA-256 revision bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl std::fmt::Display for WorkflowRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// One fully validated Workflow and its normalized plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledWorkflow {
    pub(crate) definition: WorkflowDefinition,
    pub(crate) plan: WorkflowPlan,
    pub(crate) revision: WorkflowRevision,
}

impl CompiledWorkflow {
    /// Validated author metadata.
    #[must_use]
    pub const fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }

    /// Normalized, unconstructible Stage plan.
    #[must_use]
    pub const fn plan(&self) -> &WorkflowPlan {
        &self.plan
    }

    /// Canonical revision of all normalized Workflow semantics.
    #[must_use]
    pub const fn revision(&self) -> WorkflowRevision {
        self.revision
    }
}
