//! Compiler for user-authored Workflow documents.
//!
//! [`compile`] is the only construction seam. It turns one Markdown source into
//! immutable metadata and a normalized plan; callers cannot construct an
//! unvalidated plan.

mod compiler;
mod error;
mod model;
mod render;

pub use error::{SourceLocation, WorkflowCompileError, WorkflowCompileErrorKind};
pub use model::{
    CompiledWorkflow, FailurePolicy, MAX_WORKFLOW_MODEL_ID_CHARS, MAX_WORKFLOW_REASONING_CHARS,
    StageMode, VerifySpec, WorkflowDefinition, WorkflowLevel, WorkflowModelAssignment,
    WorkflowModelCandidate, WorkflowPlan, WorkflowRevision, WorkflowStage,
};
pub use render::{
    MAX_PREDECESSOR_OUTPUT_BYTES, RenderedStage, StageEvidence, StageEvidenceStatus,
    WorkflowRenderError,
};

/// Borrowed Workflow document and its display identity.
#[derive(Clone, Copy, Debug)]
pub struct WorkflowSource<'a> {
    name: &'a str,
    text: &'a str,
}

impl<'a> WorkflowSource<'a> {
    /// Bind source text to the identity used in compilation errors.
    #[must_use]
    pub const fn new(name: &'a str, text: &'a str) -> Self {
        Self { name, text }
    }

    /// Display identity used in compilation errors.
    #[must_use]
    pub const fn name(self) -> &'a str {
        self.name
    }

    /// Complete Markdown source text.
    #[must_use]
    pub const fn text(self) -> &'a str {
        self.text
    }
}

/// Compile one Markdown Workflow through frontmatter validation, restricted
/// flowchart parsing, and deterministic topological normalization.
///
/// # Errors
///
/// Returns [`WorkflowCompileError`] when the source is malformed or the author
/// and graph declarations disagree.
pub fn compile(source: WorkflowSource<'_>) -> Result<CompiledWorkflow, WorkflowCompileError> {
    compiler::compile_source(source)
}
