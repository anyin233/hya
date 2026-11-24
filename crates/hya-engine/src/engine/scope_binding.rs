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
//! bind republishes the overlay. Captured session hook chains never pin an
//! evicted or invalidated scope: they are released with it (see
//! [`super::session_hooks`]).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hya_proto::{
    AgentName, Event, ModelRef, ProjectId, Projection, SessionId, SessionKind, ToolSchema,
};
use tokio::sync::broadcast;

use super::SessionEngine;
use crate::catalog_scope::{CatalogScope, ScopeKey};
use crate::error::CoreError;
use crate::runtime_registry::TurnBinding;
use crate::{RuntimeCandidate, RuntimeRefreshError, RuntimeRegistry};

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
    /// Returns bind failures; preparation failures retain the published sources.
    pub async fn bind_scope_runtime(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
    ) -> Result<TurnBinding, CoreError> {
        self.bind_scope_runtime_for(scope, workdir, None).await
    }

    /// Explicitly refresh the base and `scope`, retrying failed sources even
    /// with unchanged declarations, then bind the result. Failures retain
    /// their source's previous publication and appear in `errors`;
    /// independent successes can still publish and set `changed`.
    ///
    /// # Errors
    /// The bind of the (new or retained) generation fails.
    pub async fn refresh_bundles(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
    ) -> Result<BundleRefresh, CoreError> {
        let before = self.runtime.publication_generation();
        let mut errors = Vec::new();
        if let Some(refresh) = &self.catalog_refresh {
            if let Err(error) = refresh.refresh_explicit(self.runtime.as_ref()).await {
                errors.push(error);
            }
            if let Err(error) = refresh
                .refresh_scope_explicit(self.runtime.as_ref(), scope)
                .await
            {
                errors.push(error);
            }
        }
        let binding = self.bind_scope_unrefreshed(scope, workdir)?;
        self.follow_scope_session_hooks(&binding, None).await;
        Ok(BundleRefresh {
            binding,
            changed: self.runtime.publication_generation() != before,
            errors,
        })
    }

    /// [`Self::bind_scope_runtime`] on behalf of session `own`: after the
    /// bind, `own`'s captured hook chain follows the new binding and other
    /// sessions of the scope release retired processes
    /// ([`super::session_hooks`]).
    pub(crate) async fn bind_scope_runtime_for(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
        own: Option<SessionId>,
    ) -> Result<TurnBinding, CoreError> {
        if let Some(refresh) = &self.catalog_refresh {
            if let Err(error) = refresh.refresh_if_changed(self.runtime.as_ref()).await {
                tracing::warn!(%error, "runtime catalog refresh failed; retaining published sources");
            }
            if let Err(error) = refresh.refresh_scope(self.runtime.as_ref(), scope).await {
                tracing::warn!(scope = %scope.key(), %error, "scope refresh failed; retaining published sources");
            }
        }
        let binding = self.bind_scope_unrefreshed(scope, workdir)?;
        self.follow_scope_session_hooks(&binding, own).await;
        Ok(binding)
    }

    /// [`Self::bind_scope_runtime`] whose refresh failures are only logged
    /// (bundle API and catalog listings must not fail because an unrelated
    /// bundle cannot start).
    pub(crate) async fn bind_scope_runtime_lenient(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
    ) -> Result<TurnBinding, CoreError> {
        self.bind_scope_runtime_for(scope, workdir, None).await
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

    /// The skills `session`'s current agent can load with the `skill` tool
    /// (`ListSkills` with `session`): the directory skills of its recorded
    /// workdir and Project roots, the built-in ones, and the bundle and
    /// plugin skills (`SkillCatalogOrigin::Virtual`) its resource view
    /// selects, from a lenient bind of the session's scope. A bundle agent
    /// sees only what its view selects. A session without a recorded agent
    /// lists the binding's directory skills.
    ///
    /// # Errors
    /// `CoreError::Invalid("session not found")` for an unknown session,
    /// plus bind and resource-view failures.
    pub async fn session_skill_catalog(
        &self,
        session: SessionId,
    ) -> Result<Vec<hya_tool::SkillCatalogEntry>, CoreError> {
        let projection = self.read_projection_shared(session).await?;
        if projection.session.id.is_none() {
            return Err(CoreError::Invalid("session not found".to_owned()));
        }
        let agent = projection.session.agent.clone();
        drop(projection);
        let binding = self.session_scope_binding(session).await?;
        let Some(agent) = agent else {
            return Ok(binding.skills().to_vec());
        };
        let policy = binding
            .agent_resource_policy(agent.as_str())
            .map_err(CoreError::bundle)?;
        let resources = binding
            .compile_agent_resources(&policy)
            .map_err(CoreError::bundle)?;
        Ok(resources.skills().to_vec())
    }

    fn bind_scope_unrefreshed(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
    ) -> Result<TurnBinding, CoreError> {
        let binding = if workdir.as_os_str().is_empty() {
            self.runtime
                .bind_scoped_with_skills(
                    scope,
                    workdir,
                    hya_tool::discover_user_skills_with_builtins,
                )
                .map_err(CoreError::runtime_refresh)?
        } else {
            self.runtime
                .bind_scoped_with_skills(scope, workdir, || {
                    hya_tool::discover_skills_for_roots_with_builtins(workdir, scope_roots(scope))
                })
                .map_err(CoreError::runtime_refresh)?
        };
        self.touch_catalog_scope(scope.key());
        Ok(binding)
    }

    /// Drop Project `project`'s overlay (its roots, manifests, or existence
    /// changed) and tell subscribers of
    /// [`Self::subscribe_catalog_scope_invalidations`]. Existing bindings
    /// keep their snapshot; the next bind of the Project rebuilds it.
    /// Sessions' captured hook chains from the Project are released now and
    /// recaptured at each session's next bind.
    pub fn invalidate_catalog_scope(&self, project: ProjectId) {
        let key = ScopeKey::Project(project);
        self.runtime.drop_scope(&key);
        self.release_scope_session_hooks(&key);
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
            self.release_scope_session_hooks(&key);
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

/// The outcome of [`SessionEngine::refresh_bundles`].
pub struct BundleRefresh {
    /// The scope after refresh, including retained sources and independent
    /// successful replacements.
    pub binding: TurnBinding,
    /// Whether the refresh published a new base or scope generation.
    pub changed: bool,
    /// Refresh failures; each kept its source's previous publication.
    pub errors: Vec<CoreError>,
}

impl SessionEngine {
    /// Tool schemas from the current effective runtime snapshot.
    #[must_use]
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.runtime.tool_schemas()
    }

    /// Shared runtime registry handle.
    #[must_use]
    pub fn runtime_registry(&self) -> Arc<RuntimeRegistry> {
        self.runtime.clone()
    }

    /// Semantic fingerprint for a bound turn (tools + permissions + sources).
    #[must_use]
    pub fn runtime_semantic_fingerprint_v1(&self, binding: &TurnBinding) -> Option<[u8; 32]> {
        binding.semantic_fingerprint_v1(&self.permission)
    }

    /// Bind a turn against the current registry without refreshing catalogs.
    ///
    /// # Errors
    /// Returns [`CoreError::RuntimeRefresh`] when binding fails.
    pub fn bind_runtime(&self, workdir: &std::path::Path) -> Result<TurnBinding, CoreError> {
        self.runtime
            .bind_turn(workdir)
            .map_err(CoreError::runtime_refresh)
    }

    /// Optionally refresh external catalogs, then bind the scope of
    /// `workdir` ([`Self::catalog_scope_for_directory`]: its registered
    /// Project, else the directory) for a turn or catalog read there.
    ///
    /// # Errors
    /// Propagates catalog refresh or bind failures.
    pub async fn bind_root_runtime(
        &self,
        workdir: &std::path::Path,
    ) -> Result<TurnBinding, CoreError> {
        let scope = self.catalog_scope_for_directory(Some(workdir)).await;
        self.bind_scope_runtime(&scope, workdir).await
    }

    /// Optionally refresh external catalogs, then bind a project-less view
    /// (user skills and builtins) for listings that name no directory.
    ///
    /// # Errors
    /// Propagates catalog refresh or bind failures.
    pub async fn bind_global_runtime(&self) -> Result<TurnBinding, CoreError> {
        self.bind_scope_runtime(&crate::catalog_scope::CatalogScope::Global, Path::new(""))
            .await
    }

    /// Bind a fresh runtime for a Session in its catalog scope
    /// ([`Self::catalog_scope_for_session`], roots read fresh) and apply its
    /// root-tree model overrides. Catalog refresh and skill discovery happen
    /// as in [`Self::bind_scope_runtime`]; temporary models are filtered
    /// against currently available providers without mutating their durable
    /// projection.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] when `session` (or its lineage root) is
    /// absent, plus runtime refresh/provider errors from the fresh bind.
    pub async fn bind_session_runtime(
        &self,
        session: SessionId,
        workdir: &Path,
    ) -> Result<TurnBinding, CoreError> {
        let projection = self.read_projection_shared(session).await?;
        if projection.session.id != Some(session) {
            return Err(CoreError::Invalid(format!("session not found: {session}")));
        }
        let (root, _) = self.session_lineage(session).await?;
        let root_projection = self.read_projection_shared(root).await?;
        if root_projection.session.id != Some(root) {
            return Err(CoreError::Invalid(format!(
                "session root not found: {root}"
            )));
        }
        let scope = self
            .scope_for_projection(session, &projection, workdir)
            .await;
        let overrides = root_projection.session.agent_model_overrides.clone();
        drop(root_projection);
        drop(projection);
        let binding = self
            .bind_scope_runtime_for(&scope, workdir, Some(session))
            .await?;
        let overrides = overrides
            .into_iter()
            .filter(|(_, model)| self.provider_router().resolve(model).is_some())
            .collect::<BTreeMap<_, _>>();
        Ok(binding.with_session_agent_models(overrides))
    }

    /// Set or clear one temporary model override on the root Session tree.
    ///
    /// The event is always appended to the lineage root so descendants resolve
    /// the same durable map. Existing bindings remain immutable and therefore
    /// continue using the model captured before this mutation.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] when `session` (or its lineage root) is
    /// absent, plus store errors from event emission.
    pub async fn set_agent_model_override(
        &self,
        session: SessionId,
        agent: AgentName,
        model: Option<ModelRef>,
    ) -> Result<(), CoreError> {
        let projection = self.read_projection_shared(session).await?;
        if projection.session.id != Some(session) {
            return Err(CoreError::Invalid(format!("session not found: {session}")));
        }
        drop(projection);
        let (root, _) = self.session_lineage(session).await?;
        let root_projection = self.read_projection_shared(root).await?;
        if root_projection.session.id != Some(root) {
            return Err(CoreError::Invalid(format!(
                "session root not found: {root}"
            )));
        }
        drop(root_projection);
        self.emit(
            root,
            Event::SessionAgentModelOverrideSet {
                session: root,
                agent,
                model,
            },
        )
        .await
    }

    /// Publish a new runtime candidate via the registry builder callback.
    ///
    /// # Errors
    /// Returns [`RuntimeRefreshError`] when the candidate is rejected.
    pub fn refresh_runtime(
        &self,
        build: impl FnOnce(&mut RuntimeCandidate) -> Result<(), RuntimeRefreshError>,
    ) -> Result<hya_proto::ConfigGeneration, RuntimeRefreshError> {
        self.runtime.refresh(build)
    }

    /// Refresh the runtime catalog (when an app refresher is configured) so
    /// bundle API lookups see newly installed bundles. A failed refresh keeps
    /// the live generation and is only logged: a bundle endpoint must not fail
    /// because an unrelated bundle cannot start.
    pub(super) async fn refresh_catalog_for_bundle_api(&self) {
        if let Some(refresh) = &self.catalog_refresh
            && let Err(error) = refresh.refresh_if_changed(self.runtime.as_ref()).await
        {
            tracing::warn!("runtime catalog refresh before a bundle API call failed: {error:#}");
        }
    }

    /// Every base-published bundle's declared API endpoints (the global
    /// view: Project bundles are listed only by
    /// [`Self::session_bundle_apis`]).
    pub async fn bundle_apis(&self) -> Vec<crate::bundle_apis::PublishedBundleApis> {
        self.refresh_catalog_for_bundle_api().await;
        self.runtime.published_bundle_apis()
    }

    /// Every bundle's declared API endpoints in `session`'s catalog scope.
    ///
    /// # Errors
    /// [`CoreError::Invalid`] for an unknown session; bind failures.
    pub async fn session_bundle_apis(
        &self,
        session: SessionId,
    ) -> Result<Vec<crate::bundle_apis::PublishedBundleApis>, CoreError> {
        Ok(self
            .session_scope_binding(session)
            .await?
            .published_bundle_apis())
    }

    /// Serve one bundle API call.
    ///
    /// Checks the session of a session-scoped call, resolves the bundle in
    /// the live published generation (a session-scoped call: in the
    /// session's catalog scope, so a Project session reaches its Project
    /// bundles; a global call: in the base only), routes the path against its declared
    /// templates, and forwards the request to its process, which reads
    /// through a request-scoped read-only capability (bound to the session for
    /// session scope, to no session for global scope).
    ///
    /// # Errors
    /// [`crate::BundleApiError`] for an unknown session, bundle, or endpoint,
    /// a disallowed method, a malformed call, a process failure, or a store
    /// failure.
    pub async fn invoke_bundle_api(
        &self,
        call: crate::bundle_apis::BundleApiCall,
    ) -> Result<crate::bundle_apis::BundleApiOutcome, crate::BundleApiError> {
        if let Some(session) = call.session
            && !self
                .store
                .session_exists(session)
                .await
                .map_err(CoreError::from)?
        {
            return Err(crate::BundleApiError::SessionNotFound(session));
        }
        let apis = match call.session {
            Some(session) => self
                .session_scope_binding(session)
                .await?
                .bundle_apis(&call.bundle),
            None => {
                self.refresh_catalog_for_bundle_api().await;
                self.runtime.bundle_apis(&call.bundle)
            }
        };
        let apis = apis.ok_or_else(|| crate::BundleApiError::NotFound {
            bundle: call.bundle.clone(),
            scope: call.scope(),
            path: call.path.clone(),
        })?;
        apis.invoke(call).await
    }
}

/// Optional app-owned hook to refresh the runtime catalog before a root bind.
///
/// **Contract:** Called from [`SessionEngine::bind_scope_runtime`] (and so
/// every root, session, and catalog bind): first
/// [`Self::refresh_if_changed`] for the base, then [`Self::refresh_scope`]
/// for the scope being bound. Return `Ok(true)` when something was
/// published, `Ok(false)` when nothing changed. Errors abort a turn bind
/// (catalog reads only log them). Implementors own MCP/plugin discovery
/// I/O; the engine only rebinds after a successful refresh.
#[async_trait]
pub trait RuntimeCatalogRefresh: Send + Sync {
    /// Refresh `runtime` if external sources changed.
    ///
    /// # Errors
    /// Propagate discovery or publication failures as [`CoreError`].
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError>;

    /// Explicit refresh retries sources whose last preparation failed, even
    /// when their declaration has not changed.
    async fn refresh_explicit(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        self.refresh_if_changed(runtime).await
    }

    /// Explicit variant of [`Self::refresh_scope`], including failed sources.
    async fn refresh_scope_explicit(
        &self,
        runtime: &RuntimeRegistry,
        scope: &crate::catalog_scope::CatalogScope,
    ) -> Result<bool, CoreError> {
        self.refresh_scope(runtime, scope).await
    }

    /// Bring `scope`'s overlay up to date before it is bound: publish (via
    /// [`RuntimeRegistry::publish_scope`] under `scope.key()`) or drop the
    /// overlay when the scope's inputs changed. Called for every scope,
    /// `Global` and `Directory` included; an implementor that loads no tier
    /// for a scope returns `Ok(false)`. The engine may drop an idle overlay
    /// at any time (cache limits); the next call must then republish it.
    ///
    /// # Errors
    /// Report source failures while retaining each source’s last publication.
    /// Binds log these failures and use the retained catalog.
    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &crate::catalog_scope::CatalogScope,
    ) -> Result<bool, CoreError> {
        let _ = (runtime, scope);
        Ok(false)
    }
}
