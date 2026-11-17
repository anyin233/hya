//! Artifact-backed preservation for the compaction ladder's spill rung.
//!
//! Eviction with nowhere to put the bytes is a drop: the model is told to re-run
//! the tool, which costs exactly what it cost the first time — and on a tool
//! whose output was expensive to produce, that is the whole saving given back.
//! Routing the body into the session's artifact store instead turns the ladder's
//! cheapest rung into its most recoverable one: the transcript keeps a handle,
//! and the bytes stay on disk at full fidelity for as long as the session does.

use std::path::{Path, PathBuf};

use hya_tool::handle::ArtifactStore;

use crate::compaction::EvictionSink;

/// Workspace-relative directory holding spilled tool output.
///
/// Must match the root [`hya_tool::handle::HandleRouter`] resolves against, or a
/// handle written here would name an artifact the `read` tool cannot find.
pub(crate) const ARTIFACT_DIR: &str = ".hya/tool-output";

/// Smallest body worth moving to disk.
///
/// The notice left behind is around a hundred characters, so spilling a body
/// near that size spends a file and a sidecar to make the transcript *larger*.
/// Below the floor the caller falls back to the lossy notice, which is the right
/// trade for an output small enough that re-running the tool is cheap.
const MIN_SPILL_BYTES: usize = 512;

/// Preserves evicted tool output in one session's artifact store.
pub(crate) struct ArtifactEvictionSink {
    store: ArtifactStore,
}

impl ArtifactEvictionSink {
    /// Spill into the artifact directory under `workdir`.
    pub(crate) fn new(workdir: &Path) -> Self {
        Self {
            store: ArtifactStore::new(artifact_root(workdir)),
        }
    }
}

/// Artifact directory for a session rooted at `workdir`.
pub(crate) fn artifact_root(workdir: &Path) -> PathBuf {
    workdir.join(ARTIFACT_DIR)
}

impl EvictionSink for ArtifactEvictionSink {
    fn spill(&self, tool: &str, body: &str) -> Option<String> {
        if body.len() < MIN_SPILL_BYTES {
            return None;
        }
        match self.store.store(tool, "text/plain", body.as_bytes()) {
            Ok(meta) => Some(format!("artifact://{}", meta.id)),
            // A spill that cannot reach disk degrades to the lossy notice rather
            // than failing the turn. This rung exists to keep the request inside
            // its window, and a full disk must not stand in the way of that.
            Err(error) => {
                tracing::warn!(tool, %error, "could not spill evicted tool output");
                None
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A body below the floor costs more to reference than to keep.
    #[test]
    fn small_bodies_are_not_spilled() {
        let dir = std::env::temp_dir().join(format!("hya-spill-small-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sink = ArtifactEvictionSink::new(&dir);

        assert!(sink.spill("bash", "short output").is_none());
        assert!(
            !artifact_root(&dir).exists(),
            "a refused spill must not create the artifact directory"
        );
    }

    /// A spilled body is addressable by the handle the notice carries.
    #[test]
    fn spilled_body_round_trips_through_its_handle() {
        let dir = std::env::temp_dir().join(format!("hya-spill-round-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let body = "x".repeat(MIN_SPILL_BYTES);
        let sink = ArtifactEvictionSink::new(&dir);

        let handle = sink.spill("bash", &body).unwrap();

        let id = handle.strip_prefix("artifact://").unwrap();
        let stored = std::fs::read_to_string(artifact_root(&dir).join(id)).unwrap();
        assert_eq!(stored, body);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
