use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context as _;
use hya_provider::{
    AuthPresence, CatalogDiscoveryRequest, CatalogFailure, HttpProvider, ModelCatalogSource,
    ProviderAuthState, ProviderCatalogResult, ProviderCatalogSnapshot, ProviderCatalogSource,
    ProviderCatalogState, ProviderDiscoveryOutcome, ProviderKind, ProviderModel, ProviderRouter,
    ReasoningEffort, discover_models,
};
use hya_tool::WebSearchConfig;
use serde_norway::Value;

use super::ResolvedConfig;
use super::edit::ConfigEditError;
use super::file::{EngineSettings, config_path, parse_config, resolve_mcp, resolve_permission};
use super::provider::{
    EffectiveModel, PROVIDER_KIND_LABELS, ParsedProvider, ProviderCredential, ProviderKindConfig,
    discovery_auth, merge_provider_models, resolve_provider_credential, resolve_providers,
    resolve_providers_filtered, route_for_models, status_auth, validate_base_url,
};

/// One provider deferred to background discovery refresh (no cached rows,
/// or a discovery-only provider with an empty `models:` list).
#[derive(Clone, Debug)]
pub struct PendingCatalogDiscovery {
    /// Declared provider id.
    pub provider_id: String,
    /// Protocol kind for discovery adapters.
    pub kind: ProviderKind,
    /// Catalog base URL from config.
    pub base_url: String,
    pub(crate) credential: ProviderCredential,
    pub(crate) provider: ParsedProvider,
}

fn failed_result(error: &CatalogFailure) -> ProviderCatalogResult {
    match error {
        CatalogFailure::Decode
        | CatalogFailure::Schema
        | CatalogFailure::BodyTooLarge
        | CatalogFailure::PaginationLimit => ProviderCatalogResult::Invalid,
        CatalogFailure::InvalidUrl
        | CatalogFailure::UnsafeUrl
        | CatalogFailure::Redirect
        | CatalogFailure::Transport
        | CatalogFailure::Timeout
        | CatalogFailure::HttpStatus { .. } => ProviderCatalogResult::Unavailable,
    }
}

pub(super) struct ProviderPlanResult {
    /// One route per effective protocol in the provider's model list (a single
    /// route unless entries override `kind`), all sharing the provider id,
    /// base URL, credential, and retry policy.
    pub(super) routes: Vec<HttpProvider>,
    pub(super) models: Vec<ProviderModel>,
    pub(super) state: ProviderCatalogState,
}

/// What a discovery run does to the provider's model cache rows.
#[derive(Clone, Debug)]
enum CacheAction {
    /// Replace the rows with a fresh remote list (possibly empty).
    Replace(Vec<crate::model_cache::CachedModel>),
    /// Keep the existing rows (transient failure: stale beats nothing).
    Keep,
}

/// Outcome of one remote model-list fetch for a provider.
#[derive(Clone, Debug)]
pub struct DiscoveryResult {
    auth: ProviderAuthState,
    result: ProviderCatalogResult,
    cache: CacheAction,
    error: Option<String>,
}

impl DiscoveryResult {
    /// Whether the list was fetched and parsed (possibly empty).
    #[must_use]
    pub fn ok(&self) -> bool {
        matches!(
            self.result,
            ProviderCatalogResult::Models | ProviderCatalogResult::Empty
        )
    }

    /// Stable wire label: `models`, `empty`, `auth_required`,
    /// `auth_rejected`, `unavailable`, `invalid`, or `unsupported`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match (self.auth, self.result) {
            (ProviderAuthState::AuthRequired, _) => "auth_required",
            (ProviderAuthState::AuthRejected, _) => "auth_rejected",
            (_, ProviderCatalogResult::Models) => "models",
            (_, ProviderCatalogResult::Empty) => "empty",
            (_, ProviderCatalogResult::Invalid) => "invalid",
            (_, ProviderCatalogResult::Unsupported) => "unsupported",
            (_, ProviderCatalogResult::Unavailable | ProviderCatalogResult::Offline) => {
                "unavailable"
            }
        }
    }

    /// Bounded, non-secret failure description.
    #[must_use]
    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Number of remote models fetched.
    #[must_use]
    pub fn model_count(&self) -> usize {
        match &self.cache {
            CacheAction::Replace(rows) => rows.len(),
            CacheAction::Keep => 0,
        }
    }

    /// Ids of the fetched models, in endpoint order (empty unless fetched).
    #[must_use]
    pub fn model_ids(&self) -> Vec<&str> {
        match &self.cache {
            CacheAction::Replace(rows) => rows.iter().map(|row| row.id.as_str()).collect(),
            CacheAction::Keep => Vec::new(),
        }
    }

    fn timed_out(auth: ProviderAuthState) -> Self {
        Self {
            auth,
            result: ProviderCatalogResult::Unavailable,
            cache: CacheAction::Keep,
            error: Some(CatalogFailure::Timeout.to_string()),
        }
    }
}

/// Fetch one provider's remote model list (bounded; no retry loop).
///
/// A 401/403 clears the provider's cached rows (they are not usable with this
/// credential); a transport, status, or decode failure keeps them.
async fn discover_provider(
    provider: &ParsedProvider,
    credential: &ProviderCredential,
) -> DiscoveryResult {
    let presence = |auth: AuthPresence| match auth {
        AuthPresence::Credentialed => ProviderAuthState::Credentialed,
        AuthPresence::Unauthenticated => ProviderAuthState::Unauthenticated,
    };
    let fetched_at = crate::model_cache::now_ms();
    match discover_models(CatalogDiscoveryRequest::new(
        provider.id.clone(),
        provider.kind,
        provider.base_url.clone(),
        discovery_auth(provider.kind, credential),
    ))
    .await
    {
        ProviderDiscoveryOutcome::Discovered { models, auth } => {
            let rows = models
                .iter()
                .filter(|model| provider.id != "hya" || model.id != "offline")
                .map(|model| crate::model_cache::CachedModel::from_discovered(model, fetched_at))
                .collect::<Vec<_>>();
            DiscoveryResult {
                auth: presence(auth),
                result: if rows.is_empty() {
                    ProviderCatalogResult::Empty
                } else {
                    ProviderCatalogResult::Models
                },
                cache: CacheAction::Replace(rows),
                error: None,
            }
        }
        ProviderDiscoveryOutcome::Empty { auth } => DiscoveryResult {
            auth: presence(auth),
            result: ProviderCatalogResult::Empty,
            cache: CacheAction::Replace(Vec::new()),
            error: None,
        },
        ProviderDiscoveryOutcome::AuthRequired => DiscoveryResult {
            auth: ProviderAuthState::AuthRequired,
            result: ProviderCatalogResult::Unavailable,
            cache: CacheAction::Replace(Vec::new()),
            error: Some("the model list requires an API key (HTTP 401/403)".to_string()),
        },
        ProviderDiscoveryOutcome::AuthRejected => DiscoveryResult {
            auth: ProviderAuthState::AuthRejected,
            result: ProviderCatalogResult::Unavailable,
            cache: CacheAction::Replace(Vec::new()),
            error: Some("the provider rejected the API key (HTTP 401/403)".to_string()),
        },
        ProviderDiscoveryOutcome::Unsupported { .. } => DiscoveryResult {
            auth: status_auth(credential),
            result: ProviderCatalogResult::Unsupported,
            cache: CacheAction::Keep,
            error: Some("no model-list adapter for this provider kind".to_string()),
        },
        ProviderDiscoveryOutcome::Failed { error } => DiscoveryResult {
            auth: status_auth(credential),
            result: failed_result(&error),
            cache: CacheAction::Keep,
            error: Some(error.to_string()),
        },
    }
}

/// Apply a discovery result to the provider's cached rows, persisting a
/// replacement, and return the rows to plan with.
async fn apply_discovery(
    provider_id: &str,
    cached: Vec<crate::model_cache::CachedModel>,
    discovery: &DiscoveryResult,
) -> Vec<crate::model_cache::CachedModel> {
    match &discovery.cache {
        CacheAction::Replace(rows) => {
            crate::model_cache::store_provider_or_warn(provider_id, rows).await;
            rows.clone()
        }
        CacheAction::Keep => cached,
    }
}

/// Fetch the model list of a provider that is not saved yet, with `api_key`
/// as its credential, exactly as a saved provider would be fetched. Nothing
/// is written; pass the result to [`store_discovery`] once the provider is
/// saved.
///
/// # Errors
/// [`ConfigEditError::Invalid`] for an unknown `kind` label or a bad base URL.
pub async fn probe_provider(
    provider_id: &str,
    kind: &str,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<DiscoveryResult, ConfigEditError> {
    let kind: ProviderKindConfig = serde_norway::from_value(Value::String(kind.to_string()))
        .map_err(|_| {
            ConfigEditError::Invalid(format!(
                "unknown provider kind `{kind}` (expected one of {})",
                PROVIDER_KIND_LABELS.join(", ")
            ))
        })?;
    validate_base_url(base_url).map_err(ConfigEditError::Invalid)?;
    let provider = ParsedProvider {
        id: provider_id.to_string(),
        kind: kind.into(),
        base_url: base_url.to_string(),
        api_key: None,
        models: Vec::new(),
        retry: hya_provider::RetryConfig::default(),
        prompt_cache: None,
    };
    let credential = ProviderCredential {
        token: api_key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string),
        use_grok_session: false,
        use_codex_session: false,
        account_id: None,
        use_oauth_refresh: false,
    };
    Ok(discover_provider(&provider, &credential).await)
}

/// Persist a [`probe_provider`] result as `provider_id`'s model cache rows
/// (a failed fetch leaves the cache untouched).
pub async fn store_discovery(provider_id: &str, discovery: &DiscoveryResult) {
    apply_discovery(provider_id, Vec::new(), discovery).await;
}

/// Build one provider's route, catalog rows, and status from its cached rows
/// merged with its config entries.
pub(super) fn plan_for_provider(
    provider: &ParsedProvider,
    credential: &ProviderCredential,
    cached: &[crate::model_cache::CachedModel],
    discovery: Option<&DiscoveryResult>,
) -> anyhow::Result<ProviderPlanResult> {
    let merged = merge_provider_models(provider, cached);
    let auth = discovery.map_or_else(|| status_auth(credential), |discovery| discovery.auth);
    let source = if merged.is_empty() {
        ProviderCatalogSource::None
    } else if merged
        .iter()
        .any(|model| model.source != ModelCatalogSource::Configured)
    {
        ProviderCatalogSource::Discovered
    } else {
        ProviderCatalogSource::Configured
    };
    let result = if merged.is_empty() {
        discovery.map_or(ProviderCatalogResult::Empty, |discovery| discovery.result)
    } else {
        ProviderCatalogResult::Models
    };
    let state = ProviderCatalogState {
        provider_id: provider.id.clone(),
        kind: provider.kind,
        source,
        auth,
        result,
    };
    if merged.is_empty() {
        return Ok(ProviderPlanResult {
            routes: Vec::new(),
            models: Vec::new(),
            state,
        });
    }
    // Partition by effective protocol: the provider's own kind first, then
    // each override kind in first-appearance order. Every partition becomes
    // one route under the same provider id, base URL, credential, and retry
    // policy; `ProviderRouter::resolve` picks the route whose models claim
    // the requested ref, so the partitions' disjoint model sets route
    // themselves.
    let mut partitions: Vec<(ProviderKind, Vec<EffectiveModel>)> = Vec::new();
    for model in &merged {
        match partitions.iter_mut().find(|(kind, _)| *kind == model.kind) {
            Some((_, models)) => models.push(model.clone()),
            None => partitions.push((model.kind, vec![model.clone()])),
        }
    }
    partitions.sort_by_key(|(kind, _)| usize::from(*kind != provider.kind));
    let mut routes = Vec::with_capacity(partitions.len());
    let mut models = Vec::new();
    for (kind, partition) in partitions {
        let route = route_for_models(provider, credential, kind, &partition)?;
        models.extend(hya_provider::Provider::catalog(&route));
        routes.push(route);
    }
    Ok(ProviderPlanResult {
        routes,
        models,
        state,
    })
}

/// Load Hya config and resolve every provider catalog before publishing.
///
/// A provider's effective model list is its model-cache rows
/// (`$XDG_CACHE_HOME/hya/model_cache.db`) merged per model id with its
/// config `models:` entries (config fields win). Providers with cached rows
/// or config entries start without waiting on discovery HTTP; a provider with
/// neither makes one bounded blocking request. Providers with no cached rows,
/// and discovery-only providers (empty `models:`), are queued on
/// [`ResolvedConfig::pending_discovery`] for background refresh.
pub async fn load() -> anyhow::Result<Option<ResolvedConfig>> {
    let Some(path) = config_path() else {
        return Ok(None);
    };
    let yaml =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    if yaml.trim().is_empty() {
        return Ok(None);
    }
    let file = parse_config(&yaml)?;
    let permission = resolve_permission(&file)?;
    let mcp = resolve_mcp(&file)?;
    let parsed = resolve_providers(&file)?;

    let mut cache = if parsed.is_empty() {
        BTreeMap::new()
    } else {
        crate::model_cache::read_all_or_empty().await
    };
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut tasks = tokio::task::JoinSet::new();
    let mut plans = Vec::new();
    let mut pending_discovery = Vec::new();
    for provider in parsed {
        let credential = resolve_provider_credential(&provider)?;
        let cached = cache.remove(&provider.id).unwrap_or_default();
        let pending = PendingCatalogDiscovery {
            provider_id: provider.id.clone(),
            kind: provider.kind,
            base_url: provider.base_url.clone(),
            credential: credential.clone(),
            provider: provider.clone(),
        };
        if !provider.models.is_empty() || !cached.is_empty() {
            if cached.is_empty() || provider.models.is_empty() {
                pending_discovery.push(pending);
            }
            plans.push(plan_for_provider(&provider, &credential, &cached, None)?);
            continue;
        }
        // Nothing cached and nothing configured: one bounded blocking
        // request so a discovery-only provider is usable on first start. A
        // queued refresh is kept only when this attempt fails.
        pending_discovery.push(pending);
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            let discovery = match tokio::time::timeout_at(deadline, async {
                let _permit = semaphore.acquire_owned().await.ok();
                discover_provider(&provider, &credential).await
            })
            .await
            {
                Ok(discovery) => discovery,
                Err(_) => DiscoveryResult::timed_out(status_auth(&credential)),
            };
            let rows = apply_discovery(&provider.id, Vec::new(), &discovery).await;
            plan_for_provider(&provider, &credential, &rows, Some(&discovery))
        });
    }
    while let Some(result) = tasks.join_next().await {
        let plan = result.map_err(|error| anyhow::anyhow!("catalog discovery task: {error}"))??;
        if !plan.routes.is_empty() && !plan.models.is_empty() {
            pending_discovery.retain(|entry| entry.provider_id != plan.state.provider_id);
        }
        plans.push(plan);
    }
    plans.sort_by(|left, right| left.state.provider_id.cmp(&right.state.provider_id));
    let mut router = ProviderRouter::new();
    let mut live_models = Vec::new();
    let mut states = Vec::new();
    for plan in plans {
        for route in plan.routes {
            router = router.with(Arc::new(route));
        }
        live_models.extend(plan.models);
        states.push(plan.state);
    }
    if live_models.is_empty() {
        router = router.with(Arc::new(hya_provider::DevProvider::new()));
    }
    let requested_default = file.default_model.clone().map(hya_proto::ModelRef::new);
    let catalog = Arc::new(ProviderCatalogSnapshot::build(
        live_models,
        states,
        requested_default,
    ));
    router = router.with_catalog_snapshot(Arc::clone(&catalog));
    // Validate labels at the file boundary before passing the snapshot.
    if let Some(value) = file.reasoning.as_deref()
        && ReasoningEffort::parse(value).is_none()
    {
        anyhow::bail!("invalid global reasoning effort `{value}`");
    }
    let websearch = file
        .tools
        .as_ref()
        .map_or_else(WebSearchConfig::default, |tools| tools.websearch.clone());
    Ok(Some(ResolvedConfig {
        router,
        default_model: catalog.default_model().to_string(),
        catalog,
        mcp,
        default_agent: file.default_agent.clone(),
        plugins: file.plugins.clone(),
        permission,
        websearch,
        pending_discovery,
        settings: EngineSettings { file },
    }))
}

/// Replace the routes, rows, and statuses of `replaced` providers in the
/// current router/catalog with `plans`, keeping every other provider.
fn splice_catalog(
    current: &ProviderCatalogSnapshot,
    current_router: &ProviderRouter,
    replaced: &BTreeSet<String>,
    plans: Vec<ProviderPlanResult>,
) -> (ProviderRouter, Arc<ProviderCatalogSnapshot>) {
    let mut states = current
        .providers()
        .iter()
        .filter(|state| {
            !replaced.contains(&state.provider_id) && state.source != ProviderCatalogSource::Offline
        })
        .cloned()
        .collect::<Vec<_>>();
    states.extend(plans.iter().map(|plan| plan.state.clone()));
    states.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
    let mut models = current
        .models()
        .iter()
        .filter(|model| {
            !replaced.contains(&model.provider_id) && model.source != ModelCatalogSource::Offline
        })
        .cloned()
        .collect::<Vec<_>>();
    for plan in &plans {
        models.extend(plan.models.iter().cloned());
    }
    let catalog = Arc::new(ProviderCatalogSnapshot::build(
        models,
        states,
        Some(current.default_model().clone()),
    ));
    let offline = catalog
        .models()
        .iter()
        .all(|model| model.source == ModelCatalogSource::Offline);
    // The built-in offline route (`hya`) is present only while the current
    // catalog has no live row; it is re-added below only while the new one
    // has none. A configured provider named `hya` is an ordinary route.
    let offline_route_present = current
        .models()
        .iter()
        .all(|model| model.source == ModelCatalogSource::Offline);
    let mut router = ProviderRouter::new();
    for provider in current_router.providers() {
        if replaced.contains(provider.id()) || (offline_route_present && provider.id() == "hya") {
            continue;
        }
        router = router.with(Arc::clone(provider));
    }
    for plan in plans {
        for route in plan.routes {
            router = router.with(Arc::new(route));
        }
    }
    if offline {
        router = router.with(Arc::new(hya_provider::DevProvider::new()));
    }
    router = router.with_catalog_snapshot(Arc::clone(&catalog));
    (router, catalog)
}

/// Force network discovery for the queued providers, update the model
/// cache, and return rebuilt routes plus a replacement snapshot.
///
/// Callers swap the result onto the live [`hya_engine::SessionEngine`] and notify
/// the TUI (`catalog.updated`) after this future completes.
///
/// # Errors
/// Returns route-build failures.
pub async fn refresh_pending_catalogs(
    pending: Vec<PendingCatalogDiscovery>,
    current: &ProviderCatalogSnapshot,
    current_router: &ProviderRouter,
) -> anyhow::Result<(ProviderRouter, Arc<ProviderCatalogSnapshot>)> {
    let replaced = pending
        .iter()
        .map(|entry| entry.provider_id.clone())
        .collect::<BTreeSet<_>>();
    let mut cache = crate::model_cache::read_all_or_empty().await;
    let mut plans = Vec::with_capacity(pending.len());
    for entry in pending {
        let discovery = discover_provider(&entry.provider, &entry.credential).await;
        let cached = cache.remove(&entry.provider_id).unwrap_or_default();
        let rows = apply_discovery(&entry.provider_id, cached, &discovery).await;
        plans.push(plan_for_provider(
            &entry.provider,
            &entry.credential,
            &rows,
            Some(&discovery),
        )?);
    }
    Ok(splice_catalog(current, current_router, &replaced, plans))
}

/// When a live provider rebuild fetches the remote model list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoverMode {
    /// Always fetch (refresh, provider upsert).
    Always,
    /// Fetch only when the model cache has no rows for the provider (key save).
    IfUncached,
    /// Never fetch (key removal, config model edits).
    Never,
}

/// Result of rebuilding providers from the current `config.yaml`.
pub struct ProviderRebuild {
    /// Router with the rebuilt routes spliced in.
    pub router: ProviderRouter,
    /// Catalog with the rebuilt rows and statuses spliced in.
    pub catalog: Arc<ProviderCatalogSnapshot>,
    /// Rebuilt provider ids that are declared in `config.yaml`.
    pub configured: BTreeSet<String>,
    /// Fetch outcome per provider that fetched.
    pub discovery: BTreeMap<String, DiscoveryResult>,
}

/// Re-read `config.yaml`, credentials, and the model cache, then rebuild
/// `ids` (every declared provider when `None`) and splice them into the
/// current router/catalog. A requested id no longer declared in config loses
/// its route and rows.
///
/// # Errors
/// Returns config read/parse/validation or route-build failures.
pub async fn rebuild_providers(
    ids: Option<&BTreeSet<String>>,
    mode: DiscoverMode,
    current: &ProviderCatalogSnapshot,
    current_router: &ProviderRouter,
) -> anyhow::Result<ProviderRebuild> {
    let selected = match config_path() {
        Some(path) => {
            let yaml = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            if yaml.trim().is_empty() {
                Vec::new()
            } else {
                resolve_providers_filtered(&parse_config(&yaml)?, ids, true)?
            }
        }
        None => Vec::new(),
    };
    let configured = selected
        .iter()
        .map(|provider| provider.id.clone())
        .collect::<BTreeSet<_>>();
    let mut replaced = configured.clone();
    if let Some(ids) = ids {
        replaced.extend(ids.iter().cloned());
    }
    let mut cache = crate::model_cache::read_all_or_empty().await;
    let mut plans = Vec::with_capacity(selected.len());
    let mut discovery = BTreeMap::new();
    for provider in selected {
        let credential = resolve_provider_credential(&provider)?;
        let mut cached = cache.remove(&provider.id).unwrap_or_default();
        let fetch = match mode {
            DiscoverMode::Always => true,
            DiscoverMode::IfUncached => cached.is_empty(),
            DiscoverMode::Never => false,
        };
        let outcome = if fetch {
            let outcome = discover_provider(&provider, &credential).await;
            cached = apply_discovery(&provider.id, cached, &outcome).await;
            Some(outcome)
        } else {
            None
        };
        plans.push(plan_for_provider(
            &provider,
            &credential,
            &cached,
            outcome.as_ref(),
        )?);
        if let Some(outcome) = outcome {
            discovery.insert(provider.id.clone(), outcome);
        }
    }
    let (router, catalog) = splice_catalog(current, current_router, &replaced, plans);
    Ok(ProviderRebuild {
        router,
        catalog,
        configured,
        discovery,
    })
}
