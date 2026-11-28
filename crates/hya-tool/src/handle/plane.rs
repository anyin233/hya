//! Session-scoped access to spilled tool output and dynamic URI scheme handlers.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use super::artifact::ArtifactStore;
use super::registry::{SchemeRegistry, SchemeRegistryError};
use super::router::DynamicUriHandler;

/// Workspace-relative directory holding spilled tool output.
pub(super) const ARTIFACT_DIR: &str = ".hya/tool-output";

/// Resolves the spilled tool-output artifact store from the call's working directory.
#[derive(Clone, Copy, Debug, Default)]
pub struct ArtifactPlane;

impl ArtifactPlane {
    /// The store for a session rooted at `workdir`.
    #[must_use]
    pub fn store(&self, workdir: &Path) -> ArtifactStore {
        ArtifactStore::new(workdir.join(ARTIFACT_DIR))
    }
}

/// Registered dynamic URI scheme handlers carried on [`crate::ToolCtx::schemas`].
#[derive(Clone, Default)]
pub struct UriSchemaPlane {
    uri_handlers: Arc<BTreeMap<String, Arc<dyn DynamicUriHandler>>>,
}

impl std::fmt::Debug for UriSchemaPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UriSchemaPlane")
            .field("uri_schemes", &self.uri_handlers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl UriSchemaPlane {
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

    /// Remove all registered dynamic URI scheme handlers from this plane.
    #[must_use]
    pub fn clear_uri_handlers(mut self) -> Self {
        self.uri_handlers = Arc::default();
        self
    }

    /// Whether no dynamic URI scheme handlers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.uri_handlers.is_empty()
    }

    /// Registered dynamic URI scheme handlers carried by this plane.
    #[must_use]
    pub fn uri_handlers(&self) -> &Arc<BTreeMap<String, Arc<dyn DynamicUriHandler>>> {
        &self.uri_handlers
    }
}
