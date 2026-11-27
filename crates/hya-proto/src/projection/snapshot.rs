use serde::{Deserialize, Serialize};

use super::{Projection, is_zero};

/// Version of the shared reducer's fold semantics and of the durable snapshot
/// encoding ([`Projection::encode_snapshot`]).
///
/// Stores persist folded projections as a pure cache keyed by this version:
/// a snapshot carrying any other version is ignored and rebuilt from the event
/// log. Bump it whenever [`Projection::apply`] can fold the same events into a
/// different projection, or when `Projection` (or anything it contains)
/// changes shape; the `reducer_fingerprint_pins_the_version` test fails until
/// the bump is recorded.
pub const PROJECTION_REDUCER_VERSION: u32 = 14;

/// Durable snapshot encoding: the wire projection plus replay-only reducer
/// state the wire form deliberately omits.
#[derive(Serialize)]
struct SnapshotRef<'a> {
    projection: &'a Projection,
    #[serde(skip_serializing_if = "is_zero")]
    todo_last_id: u64,
    #[serde(skip_serializing_if = "is_zero")]
    file_revision: u64,
    #[serde(skip_serializing_if = "str::is_empty")]
    reducer_signature: &'a str,
}

#[derive(Deserialize)]
struct SnapshotOwned {
    projection: Projection,
    #[serde(default)]
    todo_last_id: u64,
    #[serde(default)]
    file_revision: u64,
    #[serde(default)]
    reducer_signature: String,
}

impl Projection {
    /// Encode this projection as a durable snapshot without extension reducer
    /// signatures.
    ///
    /// # Errors
    /// Returns the JSON encoder error (not expected for reducer output).
    pub fn encode_snapshot(&self) -> Result<Vec<u8>, serde_json::Error> {
        self.encode_snapshot_with("")
    }

    /// Encode this projection as a durable snapshot tagged with
    /// `reducer_signature` (from [`super::ProjectionReducerRegistry::signature`]).
    ///
    /// Unlike the wire form, the snapshot keeps replay-only reducer state
    /// (`todo_last_id`, `file_revision`, and `reducer_signature`), so
    /// [`Projection::decode_snapshot_with`] followed by applying remaining
    /// events equals a full replay under the same reducer set.
    ///
    /// # Errors
    /// Returns the JSON encoder error (not expected for reducer output).
    pub fn encode_snapshot_with(&self, reducer_signature: &str) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&SnapshotRef {
            projection: self,
            todo_last_id: self.session.todo_last_id,
            file_revision: self.session.file_revision,
            reducer_signature,
        })
    }

    /// Decode a snapshot written by [`Projection::encode_snapshot`] under the
    /// same [`PROJECTION_REDUCER_VERSION`] and empty reducer signature.
    ///
    /// # Errors
    /// Returns a decoder error for malformed snapshots or mismatched reducer signatures.
    pub fn decode_snapshot(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        Self::decode_snapshot_with(bytes, "")
    }

    /// Decode a snapshot written by [`Projection::encode_snapshot_with`] and
    /// verify its `reducer_signature` matches `expected_signature`.
    ///
    /// # Errors
    /// Returns a decoder error for malformed snapshots or mismatched reducer signatures.
    pub fn decode_snapshot_with(
        bytes: &[u8],
        expected_signature: &str,
    ) -> Result<Self, serde_json::Error> {
        let SnapshotOwned {
            mut projection,
            todo_last_id,
            file_revision,
            reducer_signature,
        } = serde_json::from_slice(bytes)?;
        if reducer_signature != expected_signature {
            return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                "projection snapshot reducer signature mismatch: expected `{expected_signature}`, found `{reducer_signature}`"
            )));
        }
        projection.session.todo_last_id = todo_last_id;
        projection.session.file_revision = file_revision;
        Ok(projection)
    }
}
