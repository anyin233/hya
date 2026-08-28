//! Source-located Workflow compiler failures.

/// A source position reported by the Workflow compiler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    line: usize,
    column: usize,
}

impl SourceLocation {
    /// One-based source line.
    #[must_use]
    pub const fn line(self) -> usize {
        self.line
    }

    /// One-based source column.
    #[must_use]
    pub const fn column(self) -> usize {
        self.column
    }
}

/// Stable compiler phase that rejected a Workflow source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowCompileErrorKind {
    /// Markdown fence or YAML frontmatter failure.
    Frontmatter,
    /// Restricted flowchart grammar failure.
    Graph,
    /// Cross-section or normalized-plan invariant failure.
    Validation,
}

/// A typed, source-located Workflow compilation failure.
#[derive(Debug, thiserror::Error)]
#[error("{source_name}:{line}:{column}: {message}")]
pub struct WorkflowCompileError {
    source_name: String,
    kind: WorkflowCompileErrorKind,
    line: usize,
    column: usize,
    message: String,
}

impl WorkflowCompileError {
    pub(crate) fn new(
        source: &str,
        line: usize,
        column: usize,
        message: impl Into<String>,
    ) -> Self {
        Self::with_kind(
            WorkflowCompileErrorKind::Validation,
            source,
            line,
            column,
            message,
        )
    }

    pub(crate) fn frontmatter(
        source: &str,
        line: usize,
        column: usize,
        message: impl Into<String>,
    ) -> Self {
        Self::with_kind(
            WorkflowCompileErrorKind::Frontmatter,
            source,
            line,
            column,
            message,
        )
    }

    pub(crate) fn graph(
        source: &str,
        line: usize,
        column: usize,
        message: impl Into<String>,
    ) -> Self {
        Self::with_kind(
            WorkflowCompileErrorKind::Graph,
            source,
            line,
            column,
            message,
        )
    }

    fn with_kind(
        kind: WorkflowCompileErrorKind,
        source: &str,
        line: usize,
        column: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source_name: source.to_string(),
            kind,
            line: line.max(1),
            column: column.max(1),
            message: message.into(),
        }
    }

    /// Compiler phase that rejected the source.
    #[must_use]
    pub const fn kind(&self) -> WorkflowCompileErrorKind {
        self.kind
    }

    /// Display identity supplied through [`crate::WorkflowSource`].
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source_name
    }

    /// One-based source position for the failure.
    #[must_use]
    pub const fn location(&self) -> SourceLocation {
        SourceLocation {
            line: self.line,
            column: self.column,
        }
    }

    /// Human-readable validation detail without the source prefix.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}
