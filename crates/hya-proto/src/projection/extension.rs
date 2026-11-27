//! Pluggable extension projection reducers for [`crate::Event::Extension`].

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use crate::ids::SessionId;
use crate::projection::Projection;

/// Pluggable reducer for [`crate::Event::Extension`] events owned by a bundle.
///
/// Loaded bundles register an implementation for their bundle id so extension
/// events fold into structured state in [`Projection::extensions`] (or core
/// projection fields where required by a bundle contract).
pub trait ProjectionReducer: Send + Sync {
    /// Canonical bundle identifier this reducer owns (matches
    /// [`crate::Event::Extension::bundle`]).
    fn bundle_id(&self) -> &str;

    /// Version of this bundle's projection state schema/fold semantics.
    ///
    /// Included in snapshot signatures so upgrading a bundle's reducer
    /// invalidates stale cached snapshots.
    fn version(&self) -> u32 {
        1
    }

    /// Fold one [`crate::Event::Extension`] event into `projection`.
    fn fold_extension(
        &self,
        projection: &mut Projection,
        session: SessionId,
        kind: &str,
        payload: &serde_json::Value,
    );
}

/// Immutable-friendly registry of bundle [`ProjectionReducer`]s.
#[derive(Clone, Default)]
pub struct ProjectionReducerRegistry {
    reducers: BTreeMap<String, Arc<dyn ProjectionReducer>>,
}

impl ProjectionReducerRegistry {
    /// Create an empty reducer registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a reducer for its [`ProjectionReducer::bundle_id`].
    pub fn register(
        &mut self,
        reducer: Arc<dyn ProjectionReducer>,
    ) -> Option<Arc<dyn ProjectionReducer>> {
        let key = reducer.bundle_id().to_owned();
        self.reducers.insert(key, reducer)
    }

    /// Builder-style helper to register a reducer.
    #[must_use]
    pub fn with_reducer(mut self, reducer: Arc<dyn ProjectionReducer>) -> Self {
        self.register(reducer);
        self
    }

    /// Look up the reducer registered for `bundle`.
    #[must_use]
    pub fn get(&self, bundle: &str) -> Option<&dyn ProjectionReducer> {
        self.reducers.get(bundle).map(AsRef::as_ref)
    }

    /// Whether no extension reducers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reducers.is_empty()
    }

    /// Deterministic signature of registered `(bundle_id, version)` pairs.
    ///
    /// Empty string when no reducers are registered; used to validate cached
    /// snapshots when loaded bundle reducers change across runs.
    #[must_use]
    pub fn signature(&self) -> String {
        if self.reducers.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for (idx, (bundle, reducer)) in self.reducers.iter().enumerate() {
            if idx > 0 {
                out.push(';');
            }
            let _ = write!(out, "{bundle}@{}", reducer.version());
        }
        out
    }
}

impl std::fmt::Debug for ProjectionReducerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectionReducerRegistry")
            .field("signature", &self.signature())
            .finish()
    }
}
