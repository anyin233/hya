//! Catalog scope binding: which [`CatalogScope`] a session's turns bind, and
//! the engine-side cache limits on scope overlays.
//!
//! **Resolution.** A Project-kind session whose Project still exists binds
//! `Project { id, roots }` with the roots read fresh from the store at every
//! bind (the same lookup as the turn's workspace roots), so a roots edit
//! applies to the next turn. A temporary session, a session without a
//! Project, and a session whose Project was deleted bind `Directory(workdir)`
//! (inert tiers only). Child and resident sessions copy their parent's
//! Project and kind at creation, so they bind the parent's scope. A catalog
//! read for a directory binds the registered Project containing it, else
//! `Directory(dir)`; a read naming no directory binds `Global`.
//!
//! **Cache limits.** Every non-global bind records the scope's last-bind
//! time. On each bind (and on [`SessionEngine::sweep_catalog_scopes`]) the
//! engine drops overlays idle longer than
//! [`CatalogScopeCacheConfig::idle_ttl`], then the least recently bound ones
//! beyond [`CatalogScopeCacheConfig::max_scopes`]. A scope that a live
//! [`TurnBinding`] still retains (a turn in flight) is never evicted (turns
//! rebind every round, which refreshes its last-bind time); even if it were
//! dropped, bindings keep their snapshot and its sources alive, and the next
//! bind republishes the overlay.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hya_proto::{ProjectId, Projection, SessionId, SessionKind};
use tokio::sync::broadcast;

use super::SessionEngine;
use crate::catalog_scope::{CatalogScope, ScopeKey};
use crate::error::CoreError;
use crate::runtime_registry::TurnBinding;

/// Limits on cached scope overlays (Project and Directory scopes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogScopeCacheConfig {
    /// Most scopes kept; the least recently bound beyond it are dropped.
    pub max_scopes: usize,
    /// A scope not bound for this long is dropped.
    pub idle_ttl: Duration,
}

impl Default for CatalogScopeCacheConfig {
    fn default() -> Self {
        Self {
            max_scopes: 32,
            idle_ttl: Duration::from_secs(30 * 60),
        }
    }
}

/// Capacity of the scope-invalidation broadcast; a lagging subscriber
/// misses old invalidations and should treat the lag as "refresh all".
const INVALIDATION_CAPACITY: usize = 64;

/// Engine-shared scope bookkeeping: last-bind times and invalidation fan-out.
pub(crate) struct ScopeCache {
    config: Mutex<CatalogScopeCacheConfig>,
    last_bound: Mutex<HashMap<ScopeKey, Instant>>,
    invalidated: broadcast::Sender<ScopeKey>,
}

impl ScopeCache {
    pub(crate) fn new() -> Self {
        let (invalidated, _) = broadcast::channel(INVALIDATION_CAPACITY);
        Self {
            config: Mutex::new(CatalogScopeCacheConfig::default()),
            last_bound: Mutex::new(HashMap::new()),
            invalidated,
        }
    }

    pub(crate) fn set_config(&self, config: CatalogScopeCacheConfig) {
        *self
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config;
    }

    fn config(&self) -> CatalogScopeCacheConfig {
        *self
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SessionEngine {
    /// The catalog scope `session`'s turns bind, for its recorded workdir.
    ///
    /// # Errors
    /// [`CoreError::Invalid`] when the session does not exist; store errors.
    pub async fn catalog_scope_for_session(
        &self,
        session: SessionId,
    ) -> Result<CatalogScope, CoreError> {
        let projection = self.read_projection_shared(session).await?;
        let workdir = session_workdir(session, &projection)?;
        Ok(self
            .scope_for_projection(session, &projection, &workdir)
            .await)
    }

    /// The catalog scope of a catalog read for `dir`: the registered
    /// (unarchived) Project containing it, else `Directory(dir)`; `Global`
    /// for no (or an empty) directory. A path the store cannot resolve (for
    /// example a relative one) is a plain directory.
    pub async fn catalog_scope_for_directory(&self, dir: Option<&Path>) -> CatalogScope {
        let Some(dir) = dir.filter(|dir| !dir.as_os_str().is_empty()) else {
            return CatalogScope::Global;
        };
        match self
            .store
            .resolve_project_by_path(&dir.to_string_lossy())
            .await
        {
            Ok(Some(project)) => CatalogScope::Project {
                id: project.id,
                roots: project.roots.iter().map(PathBuf::from).collect(),
            },
            Ok(None) => CatalogScope::Directory(dir.to_path_buf()),
            Err(error) => {
                tracing::debug!(
                    dir = %dir.display(),
                    %error,
                    "directory does not resolve to a project; binding it as a plain directory"
                );
                CatalogScope::Directory(dir.to_path_buf())
            }
        }
    }

    /// The scope of `session` (folded into `projection`) for a bind in
    /// `workdir`. Never fails: a deleted Project or a store failure binds
    /// `Directory(workdir)`.
    pub(crate) async fn scope_for_projection(
        &self,
        session: SessionId,
        projection: &Projection,
        workdir: &Path,
    ) -> CatalogScope {
        let directory = || CatalogScope::Directory(workdir.to_path_buf());
        let project = match (projection.session.kind, projection.session.project) {
            (SessionKind::Project, Some(project)) => project,
            _ => return directory(),
        };
        match self.store.get_project(project).await {
            Ok(Some(found)) => CatalogScope::Project {
                id: found.id,
                roots: found.roots.iter().map(PathBuf::from).collect(),
            },
            Ok(None) => directory(),
            Err(error) => {
                tracing::warn!(
                    %session,
                    %project,
                    %error,
                    "reading the session project failed; binding the workdir's catalog"
                );
                directory()
            }
        }
    }

    /// Refresh the base catalog and `scope`'s overlay (when an app refresher
    /// is configured), then bind `scope` for a turn in `workdir`.
    ///
    /// Skills are discovered for `workdir` plus, for a Project, every root
    /// in order (`hya_tool::discover_skills_for_roots_with_builtins`); an
    /// empty `workdir` discovers user skills only.
    ///
    /// # Errors
    /// Propagates catalog refresh or bind failures.
    pub async fn bind_scope_runtime(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
    ) -> Result<TurnBinding, CoreError> {
        if let Some(refresh) = &self.catalog_refresh {
            let _ = refresh.refresh_if_changed(self.runtime.as_ref()).await?;
            let _ = refresh.refresh_scope(self.runtime.as_ref(), scope).await?;
        }
        self.bind_scope_unrefreshed(scope, workdir)
    }

    /// [`Self::bind_scope_runtime`] whose refresh failures are only logged
    /// (bundle API and catalog listings must not fail because an unrelated
    /// bundle cannot start).
    pub(crate) async fn bind_scope_runtime_lenient(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
    ) -> Result<TurnBinding, CoreError> {
        if let Some(refresh) = &self.catalog_refresh {
            if let Err(error) = refresh.refresh_if_changed(self.runtime.as_ref()).await {
                tracing::warn!("runtime catalog refresh failed: {error:#}");
            }
            if let Err(error) = refresh.refresh_scope(self.runtime.as_ref(), scope).await {
                tracing::warn!(scope = %scope.key(), "catalog scope refresh failed: {error:#}");
            }
        }
        self.bind_scope_unrefreshed(scope, workdir)
    }

    /// The binding of `session`'s own scope for a catalog read in its
    /// recorded workdir, with lenient refresh.
    pub(crate) async fn session_scope_binding(
        &self,
        session: SessionId,
    ) -> Result<TurnBinding, CoreError> {
        let projection = self.read_projection_shared(session).await?;
        let workdir = session_workdir(session, &projection)?;
        let scope = self
            .scope_for_projection(session, &projection, &workdir)
            .await;
        self.bind_scope_runtime_lenient(&scope, &workdir).await
    }

    fn bind_scope_unrefreshed(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
    ) -> Result<TurnBinding, CoreError> {
        let binding = if workdir.as_os_str().is_empty() {
            self.runtime.bind_scoped_with_skills(
                scope,
                workdir,
                hya_tool::discover_user_skills_with_builtins,
            )?
        } else {
            self.runtime.bind_scoped_with_skills(scope, workdir, || {
                hya_tool::discover_skills_for_roots_with_builtins(workdir, scope_roots(scope))
            })?
        };
        self.touch_catalog_scope(scope.key());
        Ok(binding)
    }

    /// Drop Project `project`'s overlay (its roots, manifests, or existence
    /// changed) and tell subscribers of
    /// [`Self::subscribe_catalog_scope_invalidations`]. Existing bindings
    /// keep their snapshot; the next bind of the Project rebuilds it.
    pub fn invalidate_catalog_scope(&self, project: ProjectId) {
        let key = ScopeKey::Project(project);
        self.runtime.drop_scope(&key);
        self.scope_cache
            .last_bound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        // No subscriber is not an error.
        let _ = self.scope_cache.invalidated.send(key);
    }

    /// Receive the key of every scope dropped by
    /// [`Self::invalidate_catalog_scope`] (the server turns each into a
    /// `CatalogUpdated` notice). Cache evictions are not reported: the
    /// catalog does not change, it is only rebuilt on the next bind.
    #[must_use]
    pub fn subscribe_catalog_scope_invalidations(&self) -> broadcast::Receiver<ScopeKey> {
        self.scope_cache.invalidated.subscribe()
    }

    /// Replace the scope cache limits; applies from the next bind or sweep.
    pub fn set_catalog_scope_cache_config(&self, config: CatalogScopeCacheConfig) {
        self.scope_cache.set_config(config);
    }

    /// The current scope cache limits.
    #[must_use]
    pub fn catalog_scope_cache_config(&self) -> CatalogScopeCacheConfig {
        self.scope_cache.config()
    }

    /// Evict idle and over-capacity scope overlays now (binds do this too;
    /// the app may call it periodically so an idle process releases
    /// Project processes).
    pub fn sweep_catalog_scopes(&self) {
        self.evict_catalog_scopes(None);
    }

    fn touch_catalog_scope(&self, key: ScopeKey) {
        if key == ScopeKey::Global {
            return;
        }
        self.scope_cache
            .last_bound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone(), Instant::now());
        self.evict_catalog_scopes(Some(&key));
    }

    /// Drop scopes idle past the TTL, then the least recently bound beyond
    /// the cap; `keep` (the scope just bound) and scopes retained by a live
    /// binding are never dropped.
    fn evict_catalog_scopes(&self, keep: Option<&ScopeKey>) {
        let config = self.scope_cache.config();
        let now = Instant::now();
        let victims = {
            let mut last_bound = self
                .scope_cache
                .last_bound
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut candidates = Vec::new();
            let mut victims = Vec::new();
            for (key, bound) in last_bound.iter() {
                if Some(key) == keep || self.runtime.scope_in_use(key) {
                    continue;
                }
                if now.saturating_duration_since(*bound) >= config.idle_ttl {
                    victims.push(key.clone());
                } else {
                    candidates.push((*bound, key.clone()));
                }
            }
            let kept = last_bound.len() - victims.len();
            if kept > config.max_scopes {
                candidates.sort();
                victims.extend(
                    candidates
                        .into_iter()
                        .take(kept - config.max_scopes)
                        .map(|(_, key)| key),
                );
            }
            for key in &victims {
                last_bound.remove(key);
            }
            victims
        };
        for key in victims {
            tracing::debug!(scope = %key, "evicting cached catalog scope");
            self.runtime.drop_scope(&key);
        }
    }
}

/// Extra skill roots of `scope` beyond the workdir: a Project's roots.
fn scope_roots(scope: &CatalogScope) -> &[PathBuf] {
    match scope {
        CatalogScope::Project { roots, .. } => roots,
        CatalogScope::Global | CatalogScope::Directory(_) => &[],
    }
}

fn session_workdir(session: SessionId, projection: &Projection) -> Result<PathBuf, CoreError> {
    if projection.session.id != Some(session) {
        return Err(CoreError::Invalid(format!("session not found: {session}")));
    }
    projection
        .session
        .workdir
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| CoreError::Invalid(format!("session has no workdir: {session}")))
}
