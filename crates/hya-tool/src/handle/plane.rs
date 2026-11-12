//! Session-scoped access to spilled tool output.

use super::artifact::ArtifactStore;
use std::path::Path;

/// Workspace-relative directory holding spilled tool output.
pub(super) const ARTIFACT_DIR: &str = ".hya/tool-output";

/// Resolves the artifact store from the call's working directory.
#[derive(Clone, Debug, Default)]
pub struct ArtifactPlane {}

impl ArtifactPlane {
    /// The store for a session rooted at `workdir`.
    #[must_use]
    pub fn store(&self, workdir: &Path) -> ArtifactStore {
        ArtifactStore::new(workdir.join(ARTIFACT_DIR))
    }
}
