//! Runtime rendering for validated Workflow stages.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::model::CompiledWorkflow;

/// Maximum bytes of one direct predecessor output included in a downstream
/// directive.
pub const MAX_PREDECESSOR_OUTPUT_BYTES: usize = 4_000;

/// Terminal status carried with direct predecessor evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StageEvidenceStatus {
    /// The predecessor finished successfully.
    Done,
    /// The predecessor finished with an execution failure.
    Failed,
    /// The predecessor stopped because the run was cancelled.
    Cancelled,
    /// The predecessor did not start because fail-fast stopped the graph.
    Skipped,
}

impl StageEvidenceStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }
}

/// Borrowed terminal evidence for one compiled Stage index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StageEvidence<'a> {
    status: StageEvidenceStatus,
    output: &'a str,
}

impl<'a> StageEvidence<'a> {
    /// Bind a typed terminal status to its bounded-at-render output.
    #[must_use]
    pub const fn new(status: StageEvidenceStatus, output: &'a str) -> Self {
        Self { status, output }
    }

    /// Terminal predecessor status.
    #[must_use]
    pub const fn status(self) -> StageEvidenceStatus {
        self.status
    }

    /// Unbounded source output; rendering applies the public byte cap.
    #[must_use]
    pub const fn output(self) -> &'a str {
        self.output
    }
}

/// One Stage's final model inputs after validated interpolation and join
/// rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedStage {
    directive: String,
    system_context: String,
    verification_condition: Option<String>,
}

impl RenderedStage {
    /// User directive with required inputs and automatic predecessor evidence.
    #[must_use]
    pub fn directive(&self) -> &str {
        &self.directive
    }

    /// Deterministic append-only system-prompt layer for this activation.
    #[must_use]
    pub fn system_context(&self) -> &str {
        &self.system_context
    }

    /// Input-interpolated stop condition for loop Stages.
    #[must_use]
    pub fn verification_condition(&self) -> Option<&str> {
        self.verification_condition.as_deref()
    }
}

/// Runtime data rejected before Stage authorization or admission.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum WorkflowRenderError {
    /// A declared input has no value for this run.
    #[error("required Workflow input `{0}` was not provided")]
    MissingInput(String),
    /// A run supplied a key outside the compiled input declaration.
    #[error("Workflow input `{0}` is not declared")]
    UnknownInput(String),
    /// The caller passed an index outside the compiled plan.
    #[error("Workflow Stage index {0} is outside the compiled plan")]
    UnknownStageIndex(usize),
    /// The evidence vector does not align with the immutable plan.
    #[error("Workflow evidence has {actual} entries; expected {expected}")]
    EvidenceShape {
        /// Number of evidence entries supplied by the executor.
        actual: usize,
        /// Number of compiled Stages.
        expected: usize,
    },
    /// A direct predecessor has no terminal evidence yet.
    #[error("direct predecessor `{0}` has no terminal evidence")]
    MissingPredecessorEvidence(String),
}

impl CompiledWorkflow {
    /// Check that a run supplies exactly the compiled input namespace.
    ///
    /// # Errors
    /// [`WorkflowRenderError::MissingInput`] or
    /// [`WorkflowRenderError::UnknownInput`] for the first stable key.
    pub fn validate_inputs(
        &self,
        inputs: &BTreeMap<String, String>,
    ) -> Result<(), WorkflowRenderError> {
        if let Some(key) = self
            .definition
            .inputs
            .keys()
            .find(|key| !inputs.contains_key(*key))
        {
            return Err(WorkflowRenderError::MissingInput(key.clone()));
        }
        if let Some(key) = inputs
            .keys()
            .find(|key| !self.definition.inputs.contains_key(*key))
        {
            return Err(WorkflowRenderError::UnknownInput(key.clone()));
        }
        Ok(())
    }

    /// Render one compiled Stage using only its direct predecessor evidence.
    ///
    /// # Errors
    /// Returns [`WorkflowRenderError`] before producing partial prompt data when
    /// inputs, Stage index, evidence shape, or predecessor terminal state are
    /// invalid.
    pub fn render_stage(
        &self,
        stage_index: usize,
        inputs: &BTreeMap<String, String>,
        evidence: &[Option<StageEvidence<'_>>],
    ) -> Result<RenderedStage, WorkflowRenderError> {
        self.validate_inputs(inputs)?;
        let Some(stage) = self.plan.stages.get(stage_index) else {
            return Err(WorkflowRenderError::UnknownStageIndex(stage_index));
        };
        if evidence.len() != self.plan.stages.len() {
            return Err(WorkflowRenderError::EvidenceShape {
                actual: evidence.len(),
                expected: self.plan.stages.len(),
            });
        }

        let mut directive = interpolate_inputs(&stage.directive, inputs);
        if !stage.predecessor_indices.is_empty() {
            directive.push_str("\n\n<workflow-upstream>\n");
            for &predecessor_index in &stage.predecessor_indices {
                let predecessor = &self.plan.stages[predecessor_index];
                let Some(item) = evidence[predecessor_index] else {
                    return Err(WorkflowRenderError::MissingPredecessorEvidence(
                        predecessor.id.clone(),
                    ));
                };
                directive.push_str("<stage id=\"");
                directive.push_str(&predecessor.id);
                directive.push_str("\" agent=\"");
                directive.push_str(&predecessor.agent);
                directive.push_str("\" status=\"");
                directive.push_str(item.status.as_str());
                directive.push_str("\">\n");
                push_xml_text(
                    &mut directive,
                    truncate_utf8(item.output, MAX_PREDECESSOR_OUTPUT_BYTES),
                );
                directive.push_str("\n</stage>\n");
            }
            directive.push_str("</workflow-upstream>");
        }

        let system_context = format!(
            "<workflow-context>\nworkflow: {}\nstage: {}\nlevel: {}\n</workflow-context>",
            self.definition.name, stage.id, stage.level
        );
        let verification_condition = stage
            .verify
            .as_ref()
            .map(|verify| interpolate_inputs(&verify.until, inputs));
        Ok(RenderedStage {
            directive,
            system_context,
            verification_condition,
        })
    }
}

fn interpolate_inputs(template: &str, inputs: &BTreeMap<String, String>) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        rendered.push_str(&rest[..start]);
        let token = after[..end].trim();
        if let Some(key) = token.strip_prefix("input.")
            && let Some(value) = inputs.get(key)
        {
            rendered.push_str(value);
        }
        rest = &after[end + 2..];
    }
    rendered.push_str(rest);
    rendered
}

fn truncate_utf8(value: &str, limit: usize) -> &str {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn push_xml_text(target: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => target.push_str("&amp;"),
            '<' => target.push_str("&lt;"),
            '>' => target.push_str("&gt;"),
            _ => target.push(character),
        }
    }
}
