//! Durable Workflow identities and catalog metadata types.

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Stable catalog origin for a compiled Workflow source.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowSourceId(String);

impl WorkflowSourceId {
    /// Construct a stable source identity from a catalog-owned value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the stable source identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkflowSourceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Canonical SHA-256 revision of normalized Workflow semantics.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkflowRevision([u8; 32]);

impl WorkflowRevision {
    /// Construct a revision from compiler digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return canonical digest bytes.
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

impl FromStr for WorkflowRevision {
    type Err = WorkflowRevisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 {
            return Err(WorkflowRevisionParseError(
                "Workflow revision must contain exactly 64 hexadecimal characters".to_string(),
            ));
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let high = hex_value(pair[0]).ok_or_else(|| {
                WorkflowRevisionParseError("Workflow revision contains non-hex data".to_string())
            })?;
            let low = hex_value(pair[1]).ok_or_else(|| {
                WorkflowRevisionParseError("Workflow revision contains non-hex data".to_string())
            })?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for WorkflowRevision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WorkflowRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Error returned by [`WorkflowRevision`] text parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowRevisionParseError(String);

impl std::fmt::Display for WorkflowRevisionParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WorkflowRevisionParseError {}

/// Decode one ASCII hexadecimal digit.
const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
/// One authored Workflow model candidate in an ordered fallback chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowModelCandidate {
    /// Base model identity without a Workflow variant suffix.
    pub id: String,
    /// Optional author-provided effort label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
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

/// Preferred authored model plus its ordered fallback candidates.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowModelAssignment {
    /// Preferred base model identity.
    pub id: String,
    /// Optional preferred effort label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Ordered fallback tail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<WorkflowModelCandidate>,
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

/// One Stage in a compiled Workflow info response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStageInfo {
    /// Compiled Stage id.
    pub id: String,
    /// Optional author title.
    pub title: Option<String>,
    /// Target Agent id.
    pub agent: String,
    /// Zero-based topological level.
    pub level: usize,
    /// Direct predecessor ids in compiled join order.
    pub predecessors: Vec<String>,
    /// Optional resident actor key.
    pub actor: Option<String>,
    /// `once` or `loop`.
    pub mode: String,
    /// Optional authored worker model assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_model: Option<WorkflowModelAssignment>,
    /// Optional authored verifier model assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_model: Option<WorkflowModelAssignment>,
}

/// Valid compiled Workflow metadata returned by an `info` command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowInfo {
    /// Stable source/name/revision identity.
    pub identity: WorkflowIdentity,
    /// Human-readable description.
    pub description: String,
    /// Required input names and descriptions.
    pub inputs: BTreeMap<String, String>,
    /// Failure policy wire value.
    pub on_failure: String,
    /// Compiled Stages in stable graph order.
    pub stages: Vec<WorkflowStageInfo>,
    /// Source path for diagnostics.
    pub path: String,
}

/// One discovery row. Invalid sources remain visible with an explicit error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSummary {
    /// Declared name, or source stem when compilation failed.
    pub name: String,
    /// Description when compilation succeeded.
    pub description: String,
    /// Stable source identity when compilation succeeded.
    pub source: Option<WorkflowSourceId>,
    /// Canonical revision when compilation succeeded.
    pub revision: Option<WorkflowRevision>,
    /// Compiled Stage ids, or empty for an invalid source.
    pub stages: Vec<String>,
    /// Source path for diagnostics.
    pub path: String,
    /// Compiler failure for an invalid source.
    pub error: Option<String>,
}

/// Exact compiled Workflow identity selected or executed by a Session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowIdentity {
    /// Stable catalog source identity.
    pub source: WorkflowSourceId,
    /// Author-declared Workflow name.
    pub name: String,
    /// Canonical compiler revision.
    pub revision: WorkflowRevision,
}
