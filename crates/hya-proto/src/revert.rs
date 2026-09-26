//! File snapshot and revert value types (`FilesChanged`, `SessionReverted`,
//! `SessionUnreverted`, and `SessionProjection.revert`).
//!
//! File contents never ride on events: a stored state names a
//! content-addressed blob (`sha256` hex) that the store keeps per session.

use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, ToolCallId};
use crate::projection::MessageProjection;

/// State of one file at a point in time, as recorded for revert.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileState {
    /// The path did not exist (or was not a regular file).
    Absent,
    /// Regular file whose bytes are stored as a session blob.
    Stored {
        /// Lowercase hex `sha256` of the content (the blob key).
        hash: String,
        /// Content length in bytes.
        size: u64,
    },
    /// The content was not kept, so this state cannot be restored.
    Omitted {
        /// File size in bytes when known (0 otherwise).
        size: u64,
        /// Why: `too_large`, `session_cap`, `unreadable`, …
        reason: String,
    },
}

impl FileState {
    /// Whether a restore can write this state back.
    #[must_use]
    pub fn restorable(&self) -> bool {
        !matches!(self, FileState::Omitted { .. })
    }
}

/// One file a tool call changed, with its content before the change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Absolute path of the file.
    pub path: String,
    /// State before the call changed it.
    pub before: FileState,
}

/// A [`FileChange`] folded onto its message, with the call that made it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeRecord {
    /// Tool call that changed the file (`None` for non-call changes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<ToolCallId>,
    /// Absolute path of the file.
    pub path: String,
    /// State before the call changed it.
    pub before: FileState,
}

/// One file a revert or unrevert wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRestore {
    /// Absolute path of the file.
    pub path: String,
    /// State the operation wrote to disk (`omitted`: left untouched).
    pub restored: FileState,
    /// State on disk just before the operation (what an unrevert writes back).
    pub saved: FileState,
    /// Why writing `restored` failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A pending revert: messages hidden from the transcript until an unrevert
/// restores them or the next message commits (drops) them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RevertProjection {
    /// First hidden message (the reverted user message).
    pub message: MessageId,
    /// Hidden messages, in transcript order.
    pub hidden: Vec<MessageProjection>,
    /// Files the revert wrote; `saved` is the state before the first revert
    /// of this pending range.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileRestore>,
}
