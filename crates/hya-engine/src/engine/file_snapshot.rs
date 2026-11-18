//! Shared kernel capture and snapshot helpers used by the engine.

pub(crate) use hya_core::kernel::file_snapshot::{
    BlobBudget, FileCapture, git_output, observation,
};
pub use hya_core::kernel::file_snapshot::{
    MAX_DIRTY_BYTES, MAX_DIRTY_FILES, MAX_FILE_BYTES, MAX_SESSION_BLOB_BYTES,
};
