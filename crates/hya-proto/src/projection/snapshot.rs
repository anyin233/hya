use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{Projection, is_zero};
use crate::ids::WorkflowRunId;

/// Version of the shared reducer's fold semantics and of the durable snapshot
/// encoding ([`Projection::encode_snapshot`]).
///
/// Stores persist folded projections as a pure cache keyed by this version:
/// a snapshot carrying any other version is ignored and rebuilt from the event
/// log. Bump it whenever [`Projection::apply`] can fold the same events into a
/// different projection, or when `Projection` (or anything it contains)
/// changes shape; the `reducer_fingerprint_pins_the_version` test fails until
/// the bump is recorded.
pub const PROJECTION_REDUCER_VERSION: u32 = 13;

/// Durable snapshot encoding: the wire projection plus replay-only reducer
/// state the wire form deliberately omits.
#[derive(Serialize)]
struct SnapshotRef<'a> {
    projection: &'a Projection,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    workflow_seen_runs: Vec<WorkflowRunId>,
    #[serde(skip_serializing_if = "is_zero")]
    todo_last_id: u64,
    #[serde(skip_serializing_if = "is_zero")]
    file_revision: u64,
}

#[derive(Deserialize)]
struct SnapshotOwned {
    projection: Projection,
    #[serde(default)]
    workflow_seen_runs: BTreeSet<WorkflowRunId>,
    #[serde(default)]
    todo_last_id: u64,
    #[serde(default)]
    file_revision: u64,
}

impl Projection {
    /// Encode this projection as a durable snapshot.
    ///
    /// Unlike the wire form, the snapshot keeps replay-only reducer state
    /// (Workflow run dedupe, todo id allocation and the file revision), so [`Projection::decode_snapshot`] followed by
    /// applying the remaining events equals a full replay.
    ///
    /// # Errors
    /// Returns the JSON encoder error (not expected for reducer output).
    pub fn encode_snapshot(&self) -> Result<Vec<u8>, serde_json::Error> {
        let workflow_seen_runs = self
            .session
            .workflow
            .as_ref()
            .map(|workflow| workflow.seen_runs.iter().copied().collect())
            .unwrap_or_default();
        serde_json::to_vec(&SnapshotRef {
            projection: self,
            workflow_seen_runs,
            todo_last_id: self.session.todo_last_id,
            file_revision: self.session.file_revision,
        })
    }

    /// Decode a snapshot written by [`Projection::encode_snapshot`] under the
    /// same [`PROJECTION_REDUCER_VERSION`].
    ///
    /// # Errors
    /// Returns a decoder error for malformed snapshots or out-of-range mail indices.
    pub fn decode_snapshot(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let SnapshotOwned {
            mut projection,
            workflow_seen_runs,
            todo_last_id,
            file_revision,
        } = serde_json::from_slice(bytes)?;
        projection.session.todo_last_id = todo_last_id;
        projection.session.file_revision = file_revision;
        if let Some(workflow) = projection.session.workflow.as_mut() {
            workflow.seen_runs = workflow_seen_runs;
        }
        if projection
            .team
            .inboxes
            .values()
            .flatten()
            .chain(
                projection
                    .team
                    .channels
                    .values()
                    .flat_map(|channel| &channel.log),
            )
            .any(|index| *index >= projection.team.mail.len())
        {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "projection snapshot contains an out-of-range mail index",
            ));
        }
        Ok(projection)
    }
}
