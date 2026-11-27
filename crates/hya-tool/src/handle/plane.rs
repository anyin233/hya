//! Session-scoped access to spilled tool output and dynamic URI handlers.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use super::artifact::ArtifactStore;
use super::registry::{SchemeRegistry, SchemeRegistryError};
use super::router::DynamicUriHandler;

/// Workspace-relative directory holding spilled tool output.
pub(super) const ARTIFACT_DIR: &str = ".hya/tool-output";

/// Resolves the artifact store and registered dynamic URI scheme handlers from
/// the call's working directory.
#[derive(Clone, Default)]
pub struct ArtifactPlane {
    uri_handlers: Arc<BTreeMap<String, Arc<dyn DynamicUriHandler>>>,
}

impl std::fmt::Debug for ArtifactPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactPlane")
            .field("uri_schemes", &self.uri_handlers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ArtifactPlane {
    /// The store for a session rooted at `workdir`.
    #[must_use]
    pub fn store(&self, workdir: &Path) -> ArtifactStore {
        ArtifactStore::new(workdir.join(ARTIFACT_DIR))
    }

    /// Register a dynamic URI scheme handler for calls using this plane.
    ///
    /// # Errors
    /// Returns [`SchemeRegistryError::ProtectedScheme`] if `handler` attempts to
    /// claim an internal scheme (`artifact`, `skill`, `local`), or
    /// [`SchemeRegistryError::AlreadyOwned`] if the scheme is already registered.
    pub fn with_uri_handler(
        mut self,
        handler: Arc<dyn DynamicUriHandler>,
    ) -> Result<Self, SchemeRegistryError> {
        let scheme = handler.scheme().to_string();
        if SchemeRegistry::is_internal_scheme(&scheme) {
            return Err(SchemeRegistryError::ProtectedScheme(scheme));
        }
        let map = Arc::make_mut(&mut self.uri_handlers);
        if map.contains_key(&scheme) {
            return Err(SchemeRegistryError::AlreadyOwned {
                owner: format!("uri-handler:{scheme}"),
                scheme,
            });
        }
        map.insert(scheme, handler);
        Ok(self)
    }

    /// Registered dynamic URI scheme handlers carried by this plane.
    #[must_use]
    pub fn uri_handlers(&self) -> &Arc<BTreeMap<String, Arc<dyn DynamicUriHandler>>> {
        &self.uri_handlers
    }
}
