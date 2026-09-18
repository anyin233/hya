//! Session-scoped access to spilled tool output.
//!
//! The plane carries only the user's hook chain. An artifact's root directory is
//! derived from the call's own working directory at the moment it is needed, so
//! a plane can never address a different session than the [`ToolCtx`] it
//! travelled in — there is no second copy of the root to fall out of step.
//!
//! [`ToolCtx`]: crate::ToolCtx

use std::path::Path;
use std::sync::Arc;

use super::artifact::{ArtifactHook, ArtifactStore};

/// Workspace-relative directory holding spilled tool output.
///
/// Shared with [`super::router`] so a handle written by one is resolvable by the
/// other.
pub(super) const ARTIFACT_DIR: &str = ".hya/tool-output";

/// The user's `artifact://` post-processing chain, carried to every tool call.
///
/// Empty by default: an agent with no configured hooks retrieves exactly the
/// bytes its tools produced.
#[derive(Clone, Default)]
pub struct ArtifactPlane {
    hooks: Arc<[Arc<dyn ArtifactHook>]>,
}

impl std::fmt::Debug for ArtifactPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactPlane")
            .field(
                "hooks",
                &self
                    .hooks
                    .iter()
                    .map(|hook| hook.name())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ArtifactPlane {
    /// Carry `hooks`, applied in the order given.
    ///
    /// Order is the caller's to choose because hooks compose: "strip build noise"
    /// followed by "extract the failing assertion" is a different result from the
    /// reverse.
    #[must_use]
    pub fn new(hooks: Vec<Arc<dyn ArtifactHook>>) -> Self {
        Self {
            hooks: hooks.into(),
        }
    }

    /// Names of the registered hooks, in chain order.
    #[must_use]
    pub fn hook_names(&self) -> Vec<&str> {
        self.hooks.iter().map(|hook| hook.name()).collect()
    }

    /// The store for a session rooted at `workdir`, carrying this chain.
    #[must_use]
    pub fn store(&self, workdir: &Path) -> ArtifactStore {
        self.hooks.iter().fold(
            ArtifactStore::new(workdir.join(ARTIFACT_DIR)),
            |store, hook| store.with_hook(Arc::clone(hook)),
        )
    }
}
