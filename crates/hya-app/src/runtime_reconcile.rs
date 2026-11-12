use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use hya_bundle::{PreparedResource, PreparedSchema};
use hya_core::runtime_registry::{RuntimeSourceSkill, SourceSchema};
use hya_core::{
    RuntimeRefreshError, RuntimeRegistry, RuntimeSource, RuntimeSourceExport, RuntimeSourceId,
    RuntimeSourceKind, RuntimeSourceOwner,
};
use hya_mcp::{McpServerConfig, McpStatus, PreparedMcpServer};
use hya_plugin::PreparedPlugin;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{PluginContributionSet, SkillContribution};
use hya_proto::ConfigGeneration;
use hya_tool::{NamedTool, SkillCatalogEntry, Tool, ToolPermission, parse_skill};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) type SourceId = RuntimeSourceId;

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum DesiredSpec {
    Mcp(McpServerConfig),
    Plugin(PluginSpec),
}

#[derive(Clone)]
pub(crate) struct DesiredSource {
    id: SourceId,
    spec: DesiredSpec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AttemptTicket {
    revision: u64,
    source: SourceId,
}

#[derive(Clone)]
struct DesiredRecord {
    spec: DesiredSpec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ObservedState {
    Connecting,
    Ready,
    Failed,
    Removed,
}

#[derive(Clone)]
struct ObservedRecord {
    ticket: Option<AttemptTicket>,
    state: ObservedState,
    declaration_digest: Option<[u8; 32]>,
    typed_error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct SourceStatus {
    pub(crate) desired: bool,
    pub(crate) observed: ObservedState,
    pub(crate) typed_error: Option<String>,
    pub(crate) effective: bool,
    pub(crate) effective_generation: ConfigGeneration,
    pub(crate) observed_declaration_digest: Option<[u8; 32]>,
    pub(crate) effective_declaration_digest: Option<[u8; 32]>,
}

#[derive(Default)]
struct ReconcileState {
    desired_revision: u64,
    desired: BTreeMap<SourceId, DesiredRecord>,
    attempt: BTreeMap<SourceId, AttemptTicket>,
    observed: BTreeMap<SourceId, ObservedRecord>,
}

pub(crate) struct RuntimeReconciler {
    registry: Arc<RuntimeRegistry>,
    state: Mutex<ReconcileState>,
}

pub(crate) struct RuntimeMcpControl {
    reconciler: Arc<RuntimeReconciler>,
}

/// Refresh config plugins at the same bind/explicit-refresh boundary as bundles.
pub(crate) struct RuntimeExtensionsRefresh {
    catalogs: crate::ProjectScopeRefresh,
    reconciler: Arc<RuntimeReconciler>,
    config_file: PathBuf,
    applied: tokio::sync::Mutex<Option<String>>,
    pure: bool,
}

impl RuntimeExtensionsRefresh {
    pub(crate) fn new(
        catalogs: crate::ProjectScopeRefresh,
        reconciler: Arc<RuntimeReconciler>,
        pure: bool,
    ) -> Result<Self, hya_core::CoreError> {
        let config_file = crate::config::active_config_path();
        let applied = read_plugin_config(&config_file)?;
        Ok(Self {
            catalogs,
            reconciler,
            config_file,
            applied: tokio::sync::Mutex::new(applied),
            pure,
        })
    }
}

fn read_plugin_config(path: &Path) -> Result<Option<String>, hya_core::CoreError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(hya_core::CoreError::Invalid(format!(
            "read plugin config {}: {error}",
            path.display()
        ))),
    }
}

impl RuntimeExtensionsRefresh {
    async fn refresh_plugins(&self, explicit: bool) -> Result<(), hya_core::CoreError> {
        if self.pure {
            return Ok(());
        }
        let mut applied = self.applied.lock().await;
        let loaded = match read_plugin_config(&self.config_file) {
            Ok(next) => {
                if !explicit && *applied == next {
                    return Ok(());
                }
                // Remember failed inputs too; only edits or explicit refresh retry.
                *applied = next.clone();
                crate::config::parse_plugin_config(next.as_deref().unwrap_or("")).map_err(|error| {
                    hya_core::CoreError::Invalid(format!("load plugin config: {error:#}"))
                })
            }
            Err(error) => Err(error),
        };
        let (specs, config_error) = match loaded {
            Ok(specs) => (Some(specs), None),
            Err(error) if explicit => (None, Some(error)),
            Err(error) => return Err(error),
        };
        let plan = self
            .reconciler
            .replace_plugins(specs, explicit)
            .map_err(|error| hya_core::CoreError::Invalid(error.to_string()))?;
        let result = RuntimeMcpControl::new(self.reconciler.clone())
            .reconcile_plan(plan, tokio_util::sync::CancellationToken::new())
            .await
            .map(|_| ())
            .map_err(|error| hya_core::CoreError::Invalid(error.to_string()));
        if let Some(error) = config_error {
            return Err(error);
        }
        result
    }
}

#[async_trait::async_trait]
impl hya_core::RuntimeCatalogRefresh for RuntimeExtensionsRefresh {
    async fn refresh_if_changed(
        &self,
        runtime: &RuntimeRegistry,
    ) -> Result<bool, hya_core::CoreError> {
        let before = runtime.effective_manifest().generation;
        let catalogs = self.catalogs.refresh_if_changed(runtime).await;
        let plugins = self.refresh_plugins(false).await;
        catalogs?;
        plugins?;
        Ok(runtime.effective_manifest().generation != before)
    }

    async fn refresh_explicit(
        &self,
        runtime: &RuntimeRegistry,
    ) -> Result<bool, hya_core::CoreError> {
        let before = runtime.effective_manifest().generation;
        let catalogs = self.catalogs.refresh_explicit(runtime).await;
        let plugins = self.refresh_plugins(true).await;
        catalogs?;
        plugins?;
        Ok(runtime.effective_manifest().generation != before)
    }

    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &hya_core::CatalogScope,
    ) -> Result<bool, hya_core::CoreError> {
        self.catalogs.refresh_scope(runtime, scope).await
    }

    async fn refresh_scope_explicit(
        &self,
        runtime: &RuntimeRegistry,
        scope: &hya_core::CatalogScope,
    ) -> Result<bool, hya_core::CoreError> {
        self.catalogs.refresh_scope_explicit(runtime, scope).await
    }
}

type ControlFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) struct ReconcilePlan {
    tickets: BTreeMap<SourceId, AttemptTicket>,
    sources: Vec<DesiredSource>,
}

pub(crate) struct ReconcileOutcome {
    pub(crate) published_generation: Option<ConfigGeneration>,
}

pub(crate) struct PreparedExport {
    declared_id: String,
    aliases: Vec<String>,
    tool: Arc<dyn Tool>,
    permission: ToolPermission,
}

pub(crate) struct PreparedSource {
    id: SourceId,
    declaration_digest: [u8; 32],
    owner: Arc<dyn RuntimeSourceOwner>,
    exports: Vec<PreparedExport>,
    skills: Vec<RuntimeSourceSkill>,
    resources: BTreeMap<String, Value>,
    schemas: Vec<SourceSchema>,
    hooks: Option<Arc<dyn hya_core::hooks::HookDispatcher>>,
}

pub(crate) struct PreparedFailure {
    source: SourceId,
    error: String,
}

pub(crate) enum PreparedResult {
    Ready(PreparedSource),
    Failed(PreparedFailure),
}

#[derive(Debug, Error)]
pub(crate) enum ReconcileError {
    #[error("desired revision exhausted")]
    RevisionExhausted,
    #[error("duplicate desired source {0}")]
    DuplicateSource(SourceId),
    #[error("stale reconciliation attempt for revision {attempted}; current is {current}")]
    StaleAttempt { attempted: u64, current: u64 },
    #[error("invalid prepared result: {0}")]
    InvalidPrepared(String),
    #[error("source preparation failed for {source_id}: {error}")]
    PreparationFailed { source_id: SourceId, error: String },
    #[error(transparent)]
    Runtime(#[from] RuntimeRefreshError),
}

impl DesiredSource {
    #[must_use]
    pub(crate) fn mcp(id: SourceId, spec: McpServerConfig) -> Self {
        Self {
            id,
            spec: DesiredSpec::Mcp(normalize_mcp(spec)),
        }
    }

    #[must_use]
    pub(crate) fn plugin(id: SourceId, spec: PluginSpec) -> Self {
        Self {
            id,
            spec: DesiredSpec::Plugin(spec),
        }
    }

    #[must_use]
    pub(crate) fn id(&self) -> &SourceId {
        &self.id
    }

    fn into_record(self) -> (SourceId, DesiredRecord) {
        (self.id, DesiredRecord { spec: self.spec })
    }
}

fn normalize_mcp(mut spec: McpServerConfig) -> McpServerConfig {
    spec.enabled = Some(spec.enabled != Some(false));
    spec.env = Some(spec.env.unwrap_or_default());
    spec.timeout_ms = Some(
        spec.timeout_ms
            .unwrap_or(hya_mcp::client::DEFAULT_CALL_TIMEOUT.as_millis() as u64),
    );
    spec.transport = Some(match (spec.url.is_some(), spec.transport.as_deref()) {
        (false, _) => "stdio".to_string(),
        (true, None | Some("http" | "streamable_http")) => "http".to_string(),
        (true, Some(value)) => value.to_string(),
    });
    spec
}

impl DesiredSpec {
    fn enabled(&self) -> bool {
        match self {
            Self::Mcp(spec) => spec.enabled != Some(false),
            Self::Plugin(_) => true,
        }
    }
}

impl DesiredRecord {
    fn as_source(&self, id: SourceId) -> DesiredSource {
        DesiredSource {
            id,
            spec: self.spec.clone(),
        }
    }
}

impl PreparedExport {
    #[must_use]
    pub(crate) fn tool(
        declared_id: impl Into<String>,
        aliases: Vec<String>,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
    ) -> Self {
        Self {
            declared_id: declared_id.into(),
            aliases,
            tool,
            permission,
        }
    }
}

impl PreparedSource {
    #[must_use]
    pub(crate) fn new(
        id: SourceId,
        declaration_digest: [u8; 32],
        owner: Arc<dyn RuntimeSourceOwner>,
        exports: Vec<PreparedExport>,
    ) -> Self {
        Self {
            id,
            declaration_digest,
            owner,
            exports,
            skills: Vec::new(),
            resources: BTreeMap::new(),
            schemas: Vec::new(),
            hooks: None,
        }
    }
    /// Attach parsed Skill contributions to this prepared runtime source.
    #[must_use]
    pub(crate) fn with_skills(mut self, skills: Vec<RuntimeSourceSkill>) -> Self {
        self.skills = skills;
        self
    }

    /// Attach opaque JSON resources to this prepared runtime source.
    #[must_use]
    pub(crate) fn with_resources(mut self, resources: BTreeMap<String, Value>) -> Self {
        self.resources = resources;
        self
    }

    /// Attach external URI-scheme claims to this prepared runtime source.
    #[must_use]
    pub(crate) fn with_schemas(mut self, schemas: Vec<SourceSchema>) -> Self {
        self.schemas = schemas;
        self
    }

    /// Consume this validated candidate into the immutable runtime source shape.
    pub(crate) fn into_runtime_source(self) -> RuntimeSource {
        let exports = self
            .exports
            .into_iter()
            .map(|export| {
                let canonical_name = export.tool.name().to_string();
                RuntimeSourceExport::tool(
                    export.declared_id,
                    canonical_name,
                    export.aliases,
                    export.tool,
                    export.permission,
                )
            })
            .collect();
        let source = RuntimeSource::new(self.id, self.declaration_digest, self.owner, exports)
            .with_skills(self.skills)
            .with_resources(self.resources)
            .with_schemas(self.schemas);
        match self.hooks {
            Some(hooks) => source.with_hooks(hooks),
            None => source,
        }
    }
}

impl PreparedFailure {
    #[must_use]
    pub(crate) fn new(source: SourceId, error: impl Into<String>) -> Self {
        Self {
            source,
            error: error.into(),
        }
    }
}

/// Convert prepared bundle Skill resources through the shared contribution seam.
pub(crate) fn adapt_prepared_bundle_skills(
    bundle_id: &str,
    resources: &[PreparedResource],
    contributions: &PluginContributionSet,
) -> Result<Vec<SkillCatalogEntry>, ReconcileError> {
    Ok(
        prepared_bundle_skill_exports(bundle_id, resources, contributions)?
            .into_iter()
            .map(|skill| skill.entry().clone())
            .collect(),
    )
}

/// Prepare a static bundle source whose Skills are already validated and parsed.
pub(crate) fn prepared_static_bundle_source(
    bundle_id: &str,
    resources: &[PreparedResource],
    contributions: &PluginContributionSet,
) -> Result<PreparedSource, ReconcileError> {
    let skills = prepared_bundle_skill_exports(bundle_id, resources, contributions)?;
    let declaration_digest = digest_static_bundle_skills(bundle_id, &skills);
    Ok(PreparedSource::new(
        SourceId::bundle(bundle_id),
        declaration_digest,
        Arc::new(()),
        Vec::new(),
    )
    .with_skills(skills))
}

/// Resolve one prepared bundle's declared schema extensions into runtime
/// scheme claims.
///
/// Bundle tools are view-scoped (sidecar activation), so the claim names the
/// owning tool by its `bundle:{id}/tool/{local}` stable id — the spelling a
/// bundle agent's compiled view resolves the sidecar tool under — rather than
/// a registry export.
pub(crate) fn bundle_schema_claims(
    bundle_id: &str,
    schemas: &[PreparedSchema],
    tools: &[PreparedResource],
) -> Result<Vec<SourceSchema>, ReconcileError> {
    let mut claims = Vec::with_capacity(schemas.len());
    for schema in schemas {
        let Some(tool) = tools.iter().find(|tool| tool.local_id == schema.tool) else {
            return Err(ReconcileError::InvalidPrepared(format!(
                "bundle `{bundle_id}` schema `{}` references unknown tool `{}`",
                schema.scheme, schema.tool
            )));
        };
        claims.push(SourceSchema {
            scheme: schema.scheme.clone(),
            canonical_tool: tool.stable_id.clone(),
            writable: schema.writable,
        });
    }
    Ok(claims)
}

/// Validate and parse every prepared bundle Skill exactly once at the contribution seam.
pub(crate) fn prepared_bundle_skill_exports(
    bundle_id: &str,
    resources: &[PreparedResource],
    contributions: &PluginContributionSet,
) -> Result<Vec<RuntimeSourceSkill>, ReconcileError> {
    contributions.validate(bundle_id).map_err(|error| {
        ReconcileError::InvalidPrepared(format!("Skill contribution validation failed: {error}"))
    })?;

    let mut prepared_by_id = BTreeMap::new();
    for resource in resources {
        let expected_stable_id = format!("bundle:{bundle_id}/skill/{}", resource.local_id);
        if resource.stable_id != expected_stable_id {
            return Err(ReconcileError::InvalidPrepared(format!(
                "prepared Skill `{}` has unexpected stable id `{}`",
                resource.local_id, resource.stable_id
            )));
        }
        if prepared_by_id
            .insert(resource.local_id.as_str(), resource)
            .is_some()
        {
            return Err(ReconcileError::InvalidPrepared(format!(
                "duplicate prepared Skill `{}`",
                resource.local_id
            )));
        }
    }
    if !contributions.skills.is_empty() && prepared_by_id.len() != contributions.skills.len() {
        return Err(ReconcileError::InvalidPrepared(format!(
            "prepared Skill count {} does not match contribution count {}",
            prepared_by_id.len(),
            contributions.skills.len()
        )));
    }

    let mut contribution_by_id = BTreeMap::new();
    for contribution in &contributions.skills {
        if contribution_by_id
            .insert(contribution.id.as_str(), contribution)
            .is_some()
        {
            return Err(ReconcileError::InvalidPrepared(format!(
                "duplicate Skill contribution `{}`",
                contribution.id
            )));
        }
    }

    let mut exports = Vec::with_capacity(resources.len());
    for (local_id, resource) in prepared_by_id {
        if !contributions.skills.is_empty() {
            let Some(contribution) = contribution_by_id.get(local_id) else {
                return Err(ReconcileError::InvalidPrepared(format!(
                    "prepared Skill `{local_id}` is missing from contributions"
                )));
            };
            if contribution.content != resource.content || contribution.digest != resource.digest {
                return Err(ReconcileError::InvalidPrepared(format!(
                    "Skill contribution `{local_id}` differs from prepared id/content/digest"
                )));
            }
        }
        let contribution = SkillContribution {
            id: local_id.to_string(),
            content: resource.content.clone(),
            digest: resource.digest.clone(),
        };
        let path = PathBuf::from(format!("bundle:{bundle_id}/{}", resource.source_path));
        let entry = parse_skill_contribution(&contribution, path)?;
        exports.push(RuntimeSourceSkill::new(
            resource.stable_id.clone(),
            resource.local_id.clone(),
            resource.aliases.clone(),
            resource.digest.clone(),
            resource.content.clone(),
            entry,
        ));
    }
    Ok(exports)
}

/// Parse one shared Skill contribution into runtime metadata at the declaration seam.
fn parse_skill_contribution(
    contribution: &SkillContribution,
    path: PathBuf,
) -> Result<SkillCatalogEntry, ReconcileError> {
    let parsed = parse_skill(&contribution.content).ok_or_else(|| {
        ReconcileError::InvalidPrepared(format!(
            "Skill contribution `{}` has invalid SKILL.md frontmatter",
            contribution.id
        ))
    })?;
    if parsed.name != contribution.id {
        return Err(ReconcileError::InvalidPrepared(format!(
            "parsed Skill name `{}` does not match contribution id `{}`",
            parsed.name, contribution.id
        )));
    }
    let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    Ok(SkillCatalogEntry {
        name: parsed.name,
        description: parsed.description,
        content: parsed.content,
        allowed_tools: parsed.allowed_tools,
        model: parsed.model,
        path,
        dir,
        origin: hya_tool::SkillCatalogOrigin::Virtual,
    })
}

/// Build deterministic declaration identity bytes for a static bundle Skill source.
fn digest_static_bundle_skills(bundle_id: &str, skills: &[RuntimeSourceSkill]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"hya:static-bundle-skill-declaration:v1\0");
    update_bytes(&mut digest, bundle_id.as_bytes());
    for skill in skills {
        update_bytes(&mut digest, skill.stable_id().as_bytes());
        update_bytes(&mut digest, skill.local_id().as_bytes());
        update_bytes(&mut digest, skill.digest().as_bytes());
        update_bytes(&mut digest, skill.content().as_bytes());
    }
    digest.finalize().into()
}

/// Prepare one dynamic plugin source through the same Skill parser and source view.
pub(crate) fn prepared_plugin_source(
    plugin: PreparedPlugin,
) -> Result<PreparedSource, ReconcileError> {
    let id = SourceId::plugin(plugin.id());
    let contributions = plugin.contributions();
    contributions.validate(plugin.id()).map_err(|error| {
        ReconcileError::InvalidPrepared(format!("plugin contribution validation failed: {error}"))
    })?;
    let skills = contributions
        .skills
        .iter()
        .map(|contribution| {
            let path = PathBuf::from(format!(
                "plugin:{}/skills/{}/SKILL.md",
                plugin.id(),
                contribution.id
            ));
            let entry = parse_skill_contribution(contribution, path)?;
            Ok(RuntimeSourceSkill::new(
                format!("plugin:{}/skill/{}", plugin.id(), contribution.id),
                contribution.id.clone(),
                Vec::new(),
                contribution.digest.clone(),
                contribution.content.clone(),
                entry,
            ))
        })
        .collect::<Result<Vec<_>, ReconcileError>>()?;
    let mut digest = Sha256::new();
    digest.update(b"hya:plugin-declaration:v1\0");
    update_bytes(&mut digest, plugin.canonical_declaration());
    let declaration_digest = digest.finalize().into();
    // Contributed tools are composed into their qualified provider-facing
    // name by the host: `{plugin_id}__{declared local name}`. The inner tool
    // keeps the declared name for its `tool/call` wire requests; the
    // `NamedTool` wrapper exposes only the qualified spelling to the
    // registry, schemas, and permission plane.
    let exports = plugin
        .tools()
        .into_iter()
        .map(|tool| {
            let local_name = tool.name().to_string();
            let qualified = format!("{}__{local_name}", plugin.id());
            let exposed: Arc<dyn Tool> = Arc::new(NamedTool::new(qualified, tool));
            PreparedExport::tool(local_name, Vec::new(), exposed, ToolPermission::Tool)
        })
        .collect();
    Ok(PreparedSource::new(id, declaration_digest, Arc::new(plugin), exports).with_skills(skills))
}

/// The host is retained only by its published source, alongside its tools.
pub(crate) fn prepared_plugin_host(
    host: hya_plugin::PluginHost,
) -> Result<PreparedSource, ReconcileError> {
    let plugin = host.prepared_plugins().into_iter().next().ok_or_else(|| {
        ReconcileError::InvalidPrepared("plugin host has no prepared plugin".into())
    })?;
    let has_hooks = !plugin.contributions().hooks.is_empty();
    let mut source = prepared_plugin_source(plugin)?;
    if has_hooks {
        source.hooks = Some(Arc::new(host));
    }
    Ok(source)
}

impl From<PreparedSource> for PreparedResult {
    fn from(source: PreparedSource) -> Self {
        Self::Ready(source)
    }
}

impl From<PreparedFailure> for PreparedResult {
    fn from(failure: PreparedFailure) -> Self {
        Self::Failed(failure)
    }
}

impl PreparedResult {
    fn source(&self) -> &SourceId {
        match self {
            Self::Ready(source) => &source.id,
            Self::Failed(failure) => &failure.source,
        }
    }
}

impl RuntimeReconciler {
    #[must_use]
    pub(crate) fn new(registry: Arc<RuntimeRegistry>) -> Self {
        Self {
            registry,
            state: Mutex::new(ReconcileState::default()),
        }
    }

    pub(crate) fn replace_desired(
        &self,
        desired: Vec<DesiredSource>,
    ) -> Result<ReconcilePlan, ReconcileError> {
        let mut next = BTreeMap::new();
        for source in desired {
            let (id, record) = source.into_record();
            if next.insert(id.clone(), record).is_some() {
                return Err(ReconcileError::DuplicateSource(id));
            }
        }

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.apply_desired_locked(&mut state, next, &BTreeSet::new())
    }

    fn replace_plugins(
        &self,
        specs: Option<Vec<PluginSpec>>,
        explicit: bool,
    ) -> Result<ReconcilePlan, ReconcileError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = state.desired.clone();
        // An unreadable declaration file preserves known source desires.
        if let Some(specs) = specs {
            next.retain(|id, _| id.kind() != RuntimeSourceKind::Plugin);
            for spec in specs {
                let (id, record) =
                    DesiredSource::plugin(SourceId::plugin(spec.id.clone()), spec).into_record();
                next.insert(id, record);
            }
        }
        let retry = if explicit {
            next.keys().cloned().collect()
        } else {
            BTreeSet::new()
        };
        self.apply_desired_locked(&mut state, next, &retry)
    }

    pub(crate) fn upsert_mcp(
        &self,
        name: String,
        config: McpServerConfig,
    ) -> Result<ReconcilePlan, ReconcileError> {
        let source = DesiredSource::mcp(SourceId::mcp(name), config);
        let (id, record) = source.into_record();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = state.desired.clone();
        next.insert(id.clone(), record);
        self.apply_desired_locked(&mut state, next, &BTreeSet::from([id]))
    }

    pub(crate) fn set_mcp_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Option<ReconcilePlan>, ReconcileError> {
        let id = SourceId::mcp(name);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(record) = state.desired.get(&id) else {
            return Ok(None);
        };
        let DesiredSpec::Mcp(mut config) = record.spec.clone() else {
            return Ok(None);
        };
        config.enabled = Some(enabled);
        let source = DesiredSource::mcp(id.clone(), config);
        let (_, record) = source.into_record();
        let mut next = state.desired.clone();
        next.insert(id.clone(), record);
        self.apply_desired_locked(&mut state, next, &BTreeSet::from([id]))
            .map(Some)
    }

    fn apply_desired_locked(
        &self,
        state: &mut ReconcileState,
        next: BTreeMap<SourceId, DesiredRecord>,
        retry: &BTreeSet<SourceId>,
    ) -> Result<ReconcilePlan, ReconcileError> {
        let revision = state
            .desired_revision
            .checked_add(1)
            .ok_or(ReconcileError::RevisionExhausted)?;
        let removed = state
            .desired
            .iter()
            .filter(|(_, record)| record.spec.enabled())
            .filter(|(id, _)| next.get(*id).is_none_or(|record| !record.spec.enabled()))
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        let changed = next
            .iter()
            .filter(|(_, record)| record.spec.enabled())
            .filter(|(id, record)| {
                state
                    .desired
                    .get(*id)
                    .is_none_or(|old| !old.spec.enabled() || old.spec != record.spec)
                    || (retry.contains(*id)
                        && !state.attempt.contains_key(*id)
                        && state
                            .observed
                            .get(*id)
                            .is_none_or(|observed| observed.state != ObservedState::Ready))
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let sources = changed
            .iter()
            .filter_map(|id| next.get(id).map(|record| record.as_source(id.clone())))
            .collect::<Vec<_>>();

        state.desired_revision = revision;
        state.desired = next;
        state.attempt.retain(|id, _| {
            state
                .desired
                .get(id)
                .is_some_and(|record| record.spec.enabled())
        });
        let mut tickets = BTreeMap::new();
        for source in changed {
            let ticket = AttemptTicket {
                revision,
                source: source.clone(),
            };
            state.attempt.insert(source.clone(), ticket.clone());
            state.observed.insert(
                source.clone(),
                ObservedRecord {
                    ticket: Some(ticket.clone()),
                    state: ObservedState::Connecting,
                    declaration_digest: None,
                    typed_error: None,
                },
            );
            tickets.insert(source, ticket);
        }

        if !removed.is_empty() {
            self.registry.refresh(|candidate| {
                candidate.remove_sources(&removed);
                Ok(())
            })?;
            for source in removed {
                state.attempt.remove(&source);
                state.observed.insert(
                    source,
                    ObservedRecord {
                        ticket: None,
                        state: ObservedState::Removed,
                        declaration_digest: None,
                        typed_error: None,
                    },
                );
            }
        }

        Ok(ReconcilePlan { tickets, sources })
    }

    pub(crate) fn finish_revision<I, T>(
        &self,
        plan: &ReconcilePlan,
        results: I,
    ) -> Result<ReconcileOutcome, ReconcileError>
    where
        I: IntoIterator<Item = T>,
        T: Into<PreparedResult>,
    {
        let results = results.into_iter().map(Into::into).collect::<Vec<_>>();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut seen = BTreeSet::new();
        for result in &results {
            let source = result.source();
            if !plan.tickets.contains_key(source) {
                let error = ReconcileError::InvalidPrepared(format!("unexpected source {source}"));
                return reject_after_unlock(state, results, error);
            }
            if !seen.insert(source.clone()) {
                let error =
                    ReconcileError::InvalidPrepared(format!("duplicate result for {source}"));
                return reject_after_unlock(state, results, error);
            }
        }
        if seen.len() != plan.tickets.len() {
            let error = ReconcileError::InvalidPrepared(format!(
                "expected {} results, got {}",
                plan.tickets.len(),
                seen.len()
            ));
            return reject_after_unlock(state, results, error);
        }

        let mut first_error = None;
        let mut published_generation = None;
        for result in results {
            let id = result.source().clone();
            if let Err(error) = validate_result_source(&state, plan, &id) {
                first_error.get_or_insert(error);
                continue;
            }
            let outcome = match result {
                PreparedResult::Failed(failure) => Err(ReconcileError::PreparationFailed {
                    source_id: id.clone(),
                    error: failure.error,
                }),
                PreparedResult::Ready(prepared) => {
                    let declaration_digest = prepared.declaration_digest;
                    let source = prepared.into_runtime_source();
                    let before = self.registry.effective_manifest().generation;
                    match self
                        .registry
                        .refresh(|candidate| candidate.upsert_sources(vec![source]))
                    {
                        Ok(generation) => {
                            if generation != before {
                                published_generation = Some(generation);
                            }
                            state.observed.insert(
                                id.clone(),
                                ObservedRecord {
                                    ticket: plan.tickets.get(&id).cloned(),
                                    state: ObservedState::Ready,
                                    declaration_digest: Some(declaration_digest),
                                    typed_error: None,
                                },
                            );
                            Ok(())
                        }
                        Err(error) => Err(ReconcileError::Runtime(error)),
                    }
                }
            };
            state.attempt.remove(&id);
            if let Err(error) = outcome {
                state.observed.insert(
                    id.clone(),
                    ObservedRecord {
                        ticket: plan.tickets.get(&id).cloned(),
                        state: ObservedState::Failed,
                        declaration_digest: None,
                        typed_error: Some(match &error {
                            ReconcileError::PreparationFailed { error, .. } => error.clone(),
                            _ => error.to_string(),
                        }),
                    },
                );
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(ReconcileOutcome {
                published_generation,
            }),
        }
    }

    #[must_use]
    pub(crate) fn status(&self) -> BTreeMap<SourceId, SourceStatus> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let effective = self.registry.effective_manifest();
        let mut sources = state.desired.keys().cloned().collect::<BTreeSet<_>>();
        sources.extend(state.observed.keys().cloned());
        sources.extend(effective.sources.keys().cloned());
        sources
            .into_iter()
            .map(|source| {
                let observed = state.observed.get(&source);
                let status = SourceStatus {
                    desired: state.desired.contains_key(&source),
                    observed: observed
                        .map_or(ObservedState::Removed, |record| record.state.clone()),
                    typed_error: observed.and_then(|record| record.typed_error.clone()),
                    effective: effective.sources.contains_key(&source),
                    effective_generation: effective.generation,
                    observed_declaration_digest: observed
                        .and_then(|record| record.declaration_digest),
                    effective_declaration_digest: effective
                        .sources
                        .get(&source)
                        .map(|manifest| manifest.declaration_digest),
                };
                (source, status)
            })
            .collect()
    }
}

impl RuntimeMcpControl {
    #[must_use]
    pub(crate) fn new(reconciler: Arc<RuntimeReconciler>) -> Self {
        Self { reconciler }
    }

    pub(crate) async fn reconcile_plan(
        &self,
        mut plan: ReconcilePlan,
        stop: tokio_util::sync::CancellationToken,
    ) -> Result<ReconcileOutcome, ReconcileError> {
        if plan.is_complete() {
            return Ok(ReconcileOutcome {
                published_generation: None,
            });
        }
        let mut set = tokio::task::JoinSet::new();
        let mut tasks = BTreeMap::new();
        for source in plan.sources().iter().cloned() {
            let id = source.id().clone();
            let handle = set.spawn(async move { prepare_desired_source(source).await });
            tasks.insert(handle.id(), id);
        }
        let mut first_error = None;
        let mut published_generation = None;
        loop {
            let joined = tokio::select! {
                biased;
                () = stop.cancelled() => {
                    set.abort_all();
                    while set.join_next().await.is_some() {}
                    return Err(ReconcileError::InvalidPrepared("runtime reconciliation cancelled".to_string()));
                }
                joined = set.join_next_with_id() => match joined {
                    Some(joined) => joined,
                    None => break,
                },
            };
            let result = match joined {
                Ok((id, result)) => {
                    tasks.remove(&id);
                    result
                }
                Err(error) => {
                    let Some(source) = tasks.remove(&error.id()) else {
                        return Err(ReconcileError::InvalidPrepared(format!(
                            "source preparation task {} had no source ticket",
                            error.id()
                        )));
                    };
                    PreparedResult::Failed(PreparedFailure::new(
                        source,
                        format!("SOURCE_PREPARE_TASK_FAILED: {error}"),
                    ))
                }
            };
            let id = result.source().clone();
            let ticket = plan.tickets.remove(&id).ok_or_else(|| {
                ReconcileError::InvalidPrepared(format!("missing planned ticket for {id}"))
            })?;
            let source_plan = ReconcilePlan {
                tickets: BTreeMap::from([(id, ticket)]),
                sources: Vec::new(),
            };
            match self.reconciler.finish_revision(&source_plan, [result]) {
                Ok(outcome) => {
                    if outcome.published_generation.is_some() {
                        published_generation = outcome.published_generation;
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(ReconcileOutcome {
                published_generation,
            }),
        }
    }

    fn mcp_status(&self) -> BTreeMap<String, McpStatus> {
        self.reconciler
            .status()
            .into_iter()
            .filter(|(source, _)| source.kind() == RuntimeSourceKind::Mcp)
            .map(|(source, status)| {
                let declaration_matches = status.observed_declaration_digest.is_some()
                    && status.observed_declaration_digest == status.effective_declaration_digest;
                let value = match status.observed {
                    ObservedState::Connecting => McpStatus::Connecting,
                    ObservedState::Ready if status.effective && declaration_matches => {
                        McpStatus::Connected
                    }
                    ObservedState::Failed => McpStatus::Failed {
                        error: status
                            .typed_error
                            .unwrap_or_else(|| "MCP reconciliation failed".to_string()),
                    },
                    ObservedState::Removed if status.desired => McpStatus::Disabled,
                    ObservedState::Ready | ObservedState::Removed => McpStatus::Failed {
                        error: format!(
                            "MCP source is not effective in generation {}",
                            status.effective_generation.get()
                        ),
                    },
                };
                (source.configured_id().to_string(), value)
            })
            .collect()
    }

    fn effective_tools(&self) -> BTreeMap<String, Vec<String>> {
        self.reconciler
            .registry
            .effective_manifest()
            .sources
            .into_iter()
            .filter(|(source, _)| source.kind() == RuntimeSourceKind::Mcp)
            .map(|(source, manifest)| (source.configured_id().to_string(), manifest.exports))
            .collect()
    }

    fn effective_resources(&self) -> BTreeMap<String, Value> {
        self.reconciler
            .registry
            .effective_manifest()
            .sources
            .into_iter()
            .filter(|(source, _)| source.kind() == RuntimeSourceKind::Mcp)
            .flat_map(|(_, source)| {
                source
                    .resources
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

impl hya_server::McpControl for RuntimeMcpControl {
    fn status(&self) -> ControlFuture<'_, BTreeMap<String, McpStatus>> {
        Box::pin(async move { self.mcp_status() })
    }

    fn upsert(
        &self,
        name: String,
        config: McpServerConfig,
    ) -> ControlFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let plan = self
                .reconciler
                .upsert_mcp(name, config)
                .map_err(|error| error.to_string())?;
            match self
                .reconcile_plan(plan, tokio_util::sync::CancellationToken::new())
                .await
            {
                Ok(outcome) => {
                    trace_publication(&outcome);
                    Ok(())
                }
                // The server is stored and its status is FAILED with this
                // error; the add itself succeeded.
                Err(ReconcileError::PreparationFailed { .. }) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn set_enabled(&self, name: String, enabled: bool) -> ControlFuture<'_, Result<bool, String>> {
        Box::pin(async move {
            let Some(plan) = self
                .reconciler
                .set_mcp_enabled(&name, enabled)
                .map_err(|error| error.to_string())?
            else {
                return Ok(false);
            };
            match self
                .reconcile_plan(plan, tokio_util::sync::CancellationToken::new())
                .await
            {
                Ok(outcome) => {
                    trace_publication(&outcome);
                    Ok(true)
                }
                // A connect failure is the server's FAILED status.
                Err(ReconcileError::PreparationFailed { .. }) => Ok(true),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn resources(&self) -> ControlFuture<'_, BTreeMap<String, Value>> {
        Box::pin(async move { self.effective_resources() })
    }

    fn tools(&self) -> ControlFuture<'_, BTreeMap<String, Vec<String>>> {
        Box::pin(async move { self.effective_tools() })
    }
}

pub(crate) async fn prepare_desired_source(source: DesiredSource) -> PreparedResult {
    let id = source.id().clone();
    match source.spec {
        DesiredSpec::Mcp(mut config) => {
            for value in config.env.iter_mut().flat_map(|env| env.values_mut()) {
                match crate::config::resolve_secret(value) {
                    Ok(resolved) => *value = resolved,
                    Err(error) => {
                        return PreparedFailure::new(id, format!("MCP_ENV_FAILED: {error:#}"))
                            .into();
                    }
                }
            }
            match hya_mcp::prepare(id.configured_id().to_string(), config).await {
                Ok(server) => PreparedResult::Ready(prepared_mcp_source(id, server)),
                Err(error) => PreparedResult::Failed(PreparedFailure::new(
                    id,
                    format!("MCP_START_FAILED: {error}"),
                )),
            }
        }
        DesiredSpec::Plugin(spec) => {
            let (host, failures) = hya_plugin::PluginHost::connect_all_observed(
                vec![spec],
                crate::runtime::host_info(),
            )
            .await;
            let prepared = match failures.into_values().next() {
                Some(error) => Err(format!("PLUGIN_START_FAILED: {error}")),
                None => prepared_plugin_host(host).map_err(|error| error.to_string()),
            };
            match prepared {
                Ok(source) => source.into(),
                Err(error) => PreparedFailure::new(id, error).into(),
            }
        }
    }
}

fn prepared_mcp_source(id: SourceId, server: PreparedMcpServer) -> PreparedSource {
    let owner = Arc::new(server);
    let tools = owner.tools();
    let resources = owner.resources();
    let declaration_digest = digest_mcp_declaration(&tools, &resources);
    let exports = tools
        .into_iter()
        .map(|tool| {
            let declared_id = tool.name().to_string();
            PreparedExport::tool(declared_id, Vec::new(), tool, ToolPermission::Mcp)
        })
        .collect();
    PreparedSource::new(id, declaration_digest, owner, exports).with_resources(resources)
}

fn digest_mcp_declaration(
    tools: &[Arc<dyn Tool>],
    resources: &BTreeMap<String, Value>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"hya:mcp-declaration:v1\0");
    for tool in tools {
        if let Ok(schema) = serde_json::to_vec(&tool.schema()) {
            update_bytes(&mut digest, &schema);
        }
    }
    if let Ok(encoded) = serde_json::to_vec(resources) {
        update_bytes(&mut digest, &encoded);
    }
    digest.finalize().into()
}

fn trace_publication(outcome: &ReconcileOutcome) {
    if let Some(generation) = outcome.published_generation {
        tracing::debug!(
            generation = generation.get(),
            "published reconciled runtime sources"
        );
    }
}

impl ReconcilePlan {
    #[must_use]
    pub(crate) fn sources(&self) -> &[DesiredSource] {
        &self.sources
    }

    #[must_use]
    pub(crate) fn is_complete(&self) -> bool {
        self.tickets.is_empty()
    }
}

fn validate_result_source(
    state: &ReconcileState,
    plan: &ReconcilePlan,
    source: &SourceId,
) -> Result<(), ReconcileError> {
    let Some(ticket) = plan.tickets.get(source) else {
        return Err(ReconcileError::InvalidPrepared(format!(
            "unexpected source {source}"
        )));
    };
    let Some(active) = state.attempt.get(source) else {
        if let Some(current) = state
            .observed
            .get(source)
            .and_then(|observed| observed.ticket.as_ref())
            && current.revision > ticket.revision
        {
            return Err(ReconcileError::StaleAttempt {
                attempted: ticket.revision,
                current: current.revision,
            });
        }
        if !state
            .desired
            .get(source)
            .is_some_and(|record| record.spec.enabled())
        {
            return Err(ReconcileError::StaleAttempt {
                attempted: ticket.revision,
                current: state.desired_revision,
            });
        }
        return Err(ReconcileError::InvalidPrepared(format!(
            "missing active attempt for {source}"
        )));
    };
    if active != ticket {
        return Err(ReconcileError::StaleAttempt {
            attempted: ticket.revision,
            current: active.revision,
        });
    }
    Ok(())
}

fn reject_after_unlock<T>(
    state: MutexGuard<'_, ReconcileState>,
    staged: T,
    error: ReconcileError,
) -> Result<ReconcileOutcome, ReconcileError> {
    drop(state);
    drop(staged);
    Err(error)
}

fn update_bytes(digest: &mut Sha256, value: &[u8]) {
    digest.update(value.len().to_be_bytes());
    digest.update(value);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use hya_bundle::{
        AgentRole, Bundle, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, ResourceView,
    };
    use hya_core::{AgentCatalog, RuntimeRegistry};
    use hya_mcp::McpServerConfig;
    use hya_plugin::config::PluginSpec;
    use hya_plugin::messages::PluginKindWire;
    use hya_proto::{AgentName, ToolName, ToolSchema};
    use hya_tool::{Tool, ToolCtx, ToolError, ToolPermission, ToolRegistry};
    use serde_json::{Value, json};
    use sha2::{Digest as _, Sha256};

    use super::{
        DesiredSource, ObservedState, PreparedExport, PreparedFailure, PreparedResult,
        PreparedSource, ReconcileError, RuntimeReconciler, SourceId,
    };

    struct RefreshHarness {
        root: std::path::PathBuf,
        engine: hya_core::SessionEngine,
        reconciler: Arc<RuntimeReconciler>,
        refresh: Arc<super::RuntimeExtensionsRefresh>,
    }

    impl RefreshHarness {
        async fn new() -> anyhow::Result<Self> {
            let root =
                std::env::temp_dir().join(format!("hya-refresh-{}", hya_proto::SessionId::new()));
            std::fs::create_dir_all(&root)?;
            std::fs::write(root.join("config.yaml"), "plugins: {}\n")?;
            let registry = Arc::new(RuntimeRegistry::new(
                ToolRegistry::builtins(),
                crate::runtime::builtin_agent_catalog()?,
            ));
            let reconciler = Arc::new(RuntimeReconciler::new(registry.clone()));
            let refresh = Arc::new(super::RuntimeExtensionsRefresh {
                catalogs: crate::ProjectScopeRefresh::new(Arc::new(
                    crate::InstalledBundleRefresh::new(root.join("registry.db")),
                )),
                reconciler: reconciler.clone(),
                config_file: root.join("config.yaml"),
                applied: tokio::sync::Mutex::new(Some("plugins: {}\n".into())),
                pure: false,
            });
            let (permission, _) =
                hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
            let engine = hya_core::SessionEngine::new(
                hya_store::SessionStore::connect_memory().await?,
                Arc::new(hya_provider::ProviderRouter::new()),
                registry,
                permission,
                hya_core::EventBus::default(),
            )
            .with_catalog_refresh(refresh.clone());
            Ok(Self {
                root,
                engine,
                reconciler,
                refresh,
            })
        }

        fn config(&self, ids: &[&str]) -> anyhow::Result<()> {
            let plugins = ids.iter().map(|id| (id.to_string(), json!({"kind":"rust", "command":["python3", self.root.join(format!("{id}.py"))], "timeout_ms":1000}))).collect::<BTreeMap<_,_>>();
            std::fs::write(
                self.root.join("config.yaml"),
                serde_json::to_string(&json!({"plugins":plugins}))?,
            )?;
            Ok(())
        }

        fn script(&self, id: &str, works: bool) -> anyhow::Result<()> {
            let log = serde_json::to_string(&self.root.join(format!("{id}.starts")))?;
            let body = if works {
                r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' in r:
  result={'protocol_version':1,'plugin':{'id':'ID','version':'1.0.0','kind':'rust'},'hooks':[{'name':'message.user.before'}],'tools':[{'name':'echo','inputSchema':{'type':'object'}}]} if r.get('method')=='initialize' else {}
  print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#.replace("ID", id)
            } else {
                "raise SystemExit(1)\n".into()
            };
            std::fs::write(
                self.root.join(format!("{id}.py")),
                format!(
                    "import os\nwith open({log}, 'a') as f: f.write(str(os.getpid())+'\\n')\n{body}"
                ),
            )?;
            Ok(())
        }

        fn starts(&self, id: &str) -> usize {
            std::fs::read_to_string(self.root.join(format!("{id}.starts")))
                .unwrap_or_default()
                .lines()
                .count()
        }

        async fn bind(&self) -> anyhow::Result<hya_core::TurnBinding> {
            Ok(self
                .engine
                .bind_scope_runtime(
                    &hya_core::CatalogScope::Directory(self.root.clone()),
                    &self.root,
                )
                .await?)
        }
    }

    impl Drop for RefreshHarness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn hot_reload_failed_config_does_not_block_or_repeat_on_bind() -> anyhow::Result<()> {
        let h = RefreshHarness::new().await?;
        h.script("bad", false)?;
        h.script("good", true)?;
        h.config(&["bad", "good"])?;
        let binding = h.bind().await?;
        assert!(binding.resolve_tool("good__echo").is_some());
        for _ in 0..3 {
            h.bind().await?;
        }
        assert_eq!(h.starts("bad"), 1);
        std::fs::write(h.root.join("config.yaml"), "plugins: [invalid")?;
        assert!(h.bind().await?.resolve_tool("good__echo").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn hot_reload_explicit_refresh_retries_identical_failed_config() -> anyhow::Result<()> {
        let h = RefreshHarness::new().await?;
        h.script("retry", false)?;
        h.config(&["retry"])?;
        // RuntimeExtensionsRefresh snapshots the file before startup reconciliation.
        *h.refresh.applied.lock().await = super::read_plugin_config(&h.root.join("config.yaml"))?;
        let specs = crate::config::parse_plugin_config(&std::fs::read_to_string(
            h.root.join("config.yaml"),
        )?)?;
        let plan = h.reconciler.replace_desired(
            specs
                .into_iter()
                .map(|spec| DesiredSource::plugin(SourceId::plugin(spec.id.clone()), spec))
                .collect(),
        )?;
        assert!(
            super::RuntimeMcpControl::new(h.reconciler.clone())
                .reconcile_plan(plan, tokio_util::sync::CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(h.starts("retry"), 1);
        h.script("retry", true)?;
        let refreshed = h
            .engine
            .refresh_bundles(&hya_core::CatalogScope::Directory(h.root.clone()), &h.root)
            .await?;
        assert!(refreshed.errors.is_empty(), "{:?}", refreshed.errors);
        assert!(refreshed.binding.resolve_tool("retry__echo").is_some());
        assert_eq!(h.starts("retry"), 2);
        Ok(())
    }

    #[tokio::test]
    async fn hot_reload_config_edit_does_not_retry_unrelated_failed_mcp() -> anyhow::Result<()> {
        use hya_server::McpControl as _;
        let h = RefreshHarness::new().await?;
        let log = h.root.join("mcp.starts");
        super::RuntimeMcpControl::new(h.reconciler.clone())
            .upsert(
                "bad".into(),
                McpServerConfig {
                    command: vec![
                        "python3".into(),
                        "-c".into(),
                        format!(
                            "with open({}, 'a') as f: f.write('start\\n')",
                            serde_json::to_string(&log)?
                        ),
                    ],
                    ..Default::default()
                },
            )
            .await
            .map_err(anyhow::Error::msg)?;
        std::fs::write(
            h.root.join("config.yaml"),
            "plugins: {}\n# unrelated provider edit\n",
        )?;
        h.bind().await?;
        assert_eq!(std::fs::read_to_string(&log)?.lines().count(), 1);
        std::fs::write(h.root.join("config.yaml"), "plugins: [invalid")?;
        let refreshed = h
            .engine
            .refresh_bundles(&hya_core::CatalogScope::Directory(h.root.clone()), &h.root)
            .await?;
        assert!(!refreshed.errors.is_empty());
        assert_eq!(
            std::fs::read_to_string(&log)?.lines().count(),
            2,
            "a malformed plugin config must not block explicit retry of the known MCP declaration"
        );
        Ok(())
    }

    #[tokio::test]
    async fn hot_reload_connect_compares_mcp_defaults_semantically() -> anyhow::Result<()> {
        use hya_server::McpControl as _;
        let h = RefreshHarness::new().await?;
        let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 result={'protocolVersion':'2024-11-05','capabilities':{},'serverInfo':{'name':'test','version':'1'}} if r['method']=='initialize' else {'tools':[]} if r['method']=='tools/list' else {}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
        let control = super::RuntimeMcpControl::new(h.reconciler.clone());
        let config = McpServerConfig {
            command: vec!["python3".into(), "-c".into(), script.into()],
            ..Default::default()
        };
        control
            .upsert("test".into(), config.clone())
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            control.status().await["test"],
            hya_mcp::McpStatus::Connected
        );
        let generation = h.engine.runtime_registry().effective_manifest().generation;
        control
            .set_enabled("test".into(), true)
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            h.engine.runtime_registry().effective_manifest().generation,
            generation
        );
        control
            .upsert(
                "test".into(),
                McpServerConfig {
                    enabled: Some(true),
                    env: Some(BTreeMap::new()),
                    transport: Some("stdio".into()),
                    timeout_ms: Some(30_000),
                    ..config
                },
            )
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            h.engine.runtime_registry().effective_manifest().generation,
            generation
        );
        Ok(())
    }

    #[tokio::test]
    async fn hot_reload_global_events_survive_scope_eviction() -> anyhow::Result<()> {
        let h = RefreshHarness::new().await?;
        let events = h.root.join("events.log");
        let script = format!(
            r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 with open({}, 'a') as f: f.write(json.dumps(r)+'\n')
 if 'id' in r:
  result={{'protocol_version':1,'plugin':{{'id':'global','version':'1.0.0','kind':'rust'}},'hooks':[{{'name':'event'}},{{'name':'session.start'}},{{'name':'session.end'}}]}} if r.get('method')=='initialize' else {{}}
  print(json.dumps({{'jsonrpc':'2.0','id':r['id'],'result':result}}),flush=True)
"#,
            serde_json::to_string(&events)?
        );
        std::fs::write(h.root.join("global.py"), script)?;
        h.config(&["global"])?;
        h.bind().await?;
        let session = h
            .engine
            .create(hya_core::CreateSession {
                parent: None,
                agent: hya_proto::AgentName::new("hya-main"),
                model: hya_proto::ModelRef::new("hya/offline"),
                workdir: h.root.display().to_string(),
                project: None,
                kind: hya_proto::SessionKind::Temporary,
            })
            .await?;
        h.engine
            .set_catalog_scope_cache_config(hya_core::CatalogScopeCacheConfig {
                max_scopes: 0,
                idle_ttl: std::time::Duration::ZERO,
            });
        h.engine.sweep_catalog_scopes();
        h.engine.set_title(session, "after eviction".into()).await?;
        h.engine.archive_session(session).await?;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let log = std::fs::read_to_string(&events).unwrap_or_default();
                if log.contains("session.end") && log.contains("after eviction") {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;
        let log = std::fs::read_to_string(events)?;
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("session_created"))
                .count(),
            1,
            "SessionCreated must reach global event hooks exactly once: {log}"
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("hook/session.start"))
                .count(),
            1
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("hook/session.end"))
                .count(),
            1
        );
        Ok(())
    }

    struct HookedSidecar;

    #[async_trait]
    impl hya_core::BoundSidecarFactory for HookedSidecar {
        async fn start(
            &self,
            _: hya_core::SidecarStart,
        ) -> Result<Box<dyn hya_core::SidecarHandle>, hya_core::CoreError> {
            Ok(Box::new(HookedSidecar))
        }
    }

    #[async_trait]
    impl hya_core::SidecarHandle for HookedSidecar {
        async fn ready(&mut self) -> Result<(), hya_core::CoreError> {
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<(), hya_core::CoreError> {
            Ok(())
        }

        fn hook_dispatcher(&self) -> Option<Arc<dyn hya_core::HookDispatcher>> {
            Some(Arc::new(hya_core::hooks::NoopHookHost))
        }
    }

    async fn assert_global_events_with_child_sidecar(resident: bool) -> anyhow::Result<()> {
        use hya_core::{AgentSpec, CreateSession, MemberSpec, ResidentSupervisor, SessionEngine};
        use hya_proto::{Event, FinishCause, MemberId, ModelRef, Role, SessionKind};
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        let h = RefreshHarness::new().await?;
        let events = h.root.join("events.log");
        std::fs::write(
            h.root.join("global.py"),
            format!(
                r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method')=='event':
  with open({}, 'a') as f: f.write(json.dumps(r['params']['envelope'])+'\n')
 if 'id' in r:
  result={{'protocol_version':1,'plugin':{{'id':'global','version':'1.0.0','kind':'rust'}},'hooks':[{{'name':'event'}}]}} if r.get('method')=='initialize' else {{}}
  print(json.dumps({{'jsonrpc':'2.0','id':r['id'],'result':result}}),flush=True)
"#,
                serde_json::to_string(&events)?
            ),
        )?;
        h.config(&["global"])?;
        h.bind().await?;
        let prepared = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
            "child",
            vec![hya_bundle::SourceFile::new(
                "bundle.hya.md",
                "---\nkind: Bundle\nidentity: { id: acme/child, version: 1.0.0, publisher: acme }\nagents: [{ id: hooked-child, role: subagent }]\n---\nChild prompt.\n",
            )],
        ))?;
        let runtime = h.engine.runtime_registry().clone();
        let catalog = Arc::new(AgentCatalog::new(Arc::new(
            BundleCatalog::from_verified_catalogs(&[&prepared])?,
        ))?);
        runtime.refresh(|candidate| {
            candidate.replace_catalog(catalog);
            Ok(())
        })?;
        let (permission, _) = hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
        let engine = Arc::new(SessionEngine::new(
            hya_store::SessionStore::connect_memory().await?,
            Arc::new(
                hya_provider::ProviderRouter::new()
                    .with(Arc::new(hya_provider::FakeProvider::scripted(Vec::new()))),
            ),
            runtime,
            permission,
            hya_core::EventBus::default(),
        ));
        let mut live = engine.bus().subscribe();
        let root_agent = AgentSpec {
            name: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            system_prompt: String::new(),
            workdir: h.root.clone(),
            reasoning: None,
        };
        let root = engine
            .create(CreateSession {
                parent: None,
                agent: root_agent.name.clone(),
                model: root_agent.model.clone(),
                workdir: h.root.display().to_string(),
                project: None,
                kind: SessionKind::Temporary,
            })
            .await?;
        engine.admit_user_prompt(root, "root prompt".into()).await?;
        engine
            .run_turn(root, &root_agent, CancellationToken::new())
            .await?;
        let child_agent = AgentSpec {
            name: AgentName::new("hooked-child"),
            ..root_agent
        };
        let binding = engine.bind_runtime(&h.root)?;
        let agents = engine.agent_roster_for_binding(&binding, "hooked-child")?;
        let resources = engine.agent_resource_policy_for_binding(&binding, "hooked-child")?;
        let child = if resident {
            let supervisor = ResidentSupervisor::start(engine.clone());
            let (child, handle) = supervisor
                .spawn_resident(
                    root,
                    child_agent,
                    (binding, agents, resources, Some(Arc::new(HookedSidecar))),
                    "child prompt".into(),
                    None,
                    None,
                )
                .await?;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let projection = engine.read_projection(child).await?;
                    if projection
                        .session
                        .messages
                        .iter()
                        .any(|m| m.role == Role::Assistant)
                        && !engine.turn_active(child)
                    {
                        return Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            engine
                .mail_send(
                    root,
                    hya_proto::MailEndpoint::Handle(handle),
                    hya_proto::MailKind::Message,
                    "next child prompt".into(),
                )
                .await?;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let projection = engine.read_projection(child).await?;
                    if projection
                        .session
                        .messages
                        .iter()
                        .filter(|m| m.role == Role::Assistant)
                        .count()
                        >= 2
                        && !engine.turn_active(child)
                    {
                        return Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            supervisor
                .drain(FinishCause::Shutdown, Duration::from_secs(2))
                .await;
            child
        } else {
            let results = hya_core::run_team(
                engine.clone(),
                root,
                vec![MemberSpec {
                    id: MemberId::new(),
                    agent: child_agent,
                    binding,
                    agents,
                    resources: Some(resources),
                    guidance: None,
                    directive: "child prompt".into(),
                    tool_call: None,
                    description: String::new(),
                    session: None,
                    sidecar_factory: Some(Arc::new(HookedSidecar)),
                }],
                CancellationToken::new(),
            )
            .await;
            assert_eq!(
                results[0].status,
                hya_core::MemberStatus::Done,
                "{results:?}"
            );
            results[0].session.parse()?
        };
        // A final notification is a queue barrier for this plugin's event worker.
        engine.set_title(root, "event barrier".into()).await?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(&events)
                    .unwrap_or_default()
                    .contains("event barrier")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        let delivered = std::fs::read_to_string(&events)?
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let mut expected = Vec::new();
        while let Ok(envelope) = live.try_recv() {
            expected.push(envelope);
        }
        assert!(
            expected.iter().any(
                |e| matches!(e.event, Event::SessionCreated { session, .. } if session == root)
            )
        );
        assert!(expected.iter().any(|e| matches!(e.event, Event::MessageStarted { session, role: Role::User, .. } if session == child)));
        assert!(expected.iter().any(|e| matches!(e.event, Event::MessageStarted { session, role: Role::Assistant, .. } if session == child)));
        for envelope in expected {
            let value = serde_json::to_value(&envelope)?;
            assert_eq!(
                delivered.iter().filter(|e| **e == value).count(),
                1,
                "every envelope exactly once: {value}; delivered={delivered:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn hot_reload_global_events_include_subagent_sidecar_preturn() -> anyhow::Result<()> {
        assert_global_events_with_child_sidecar(false).await
    }

    #[tokio::test]
    async fn hot_reload_global_events_include_resident_sidecar_preturn() -> anyhow::Result<()> {
        assert_global_events_with_child_sidecar(true).await
    }

    #[tokio::test]
    async fn hot_reload_removed_global_plugin_stops_despite_idle_sessions() -> anyhow::Result<()> {
        let h = RefreshHarness::new().await?;
        h.script("global", true)?;
        h.config(&["global"])?;
        h.bind().await?;
        let bundle = h.root.join(".hya/bundles/local");
        std::fs::create_dir_all(&bundle)?;
        std::fs::write(
            bundle.join("bundle.hya.md"),
            "---\nkind: Bundle\nidentity: { id: acme/local, version: 1.0.0, publisher: test }\nagents: [{ id: local, role: main }]\n---\nLocal.\n",
        )?;
        let project = h
            .engine
            .store()
            .create_project("idle", &[h.root.display().to_string()])
            .await?;
        let session = h
            .engine
            .create(hya_core::CreateSession {
                parent: None,
                agent: hya_proto::AgentName::new("hya-main"),
                model: hya_proto::ModelRef::new("hya/offline"),
                workdir: h.root.display().to_string(),
                project: Some(project.id),
                kind: hya_proto::SessionKind::Project,
            })
            .await?;
        let retained = h.engine.bind_session_runtime(session, &h.root).await?;
        assert!(
            h.engine
                .runtime_registry()
                .scope_overlay(&hya_core::ScopeKey::Project(project.id))
                .is_some()
        );
        let pid = std::fs::read_to_string(h.root.join("global.starts"))?
            .trim()
            .to_string();
        h.config(&[])?;
        assert!(
            h.engine
                .bind_global_runtime()
                .await?
                .resolve_tool("global__echo")
                .is_none()
        );
        assert!(retained.resolve_tool("global__echo").is_some());
        assert!(
            h.engine
                .runtime_registry()
                .scope_in_use(&hya_core::ScopeKey::Project(project.id))
        );
        drop(retained);
        assert!(
            !h.engine
                .runtime_registry()
                .scope_in_use(&hya_core::ScopeKey::Project(project.id))
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let output = std::process::Command::new("ps")
                    .args(["-o", "stat=", "-p", &pid])
                    .output()?;
                let state = String::from_utf8_lossy(&output.stdout);
                if state.trim().is_empty() || state.trim().starts_with('Z') {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok::<_, std::io::Error>(())
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn hot_reload_http_refresh_reports_partial_publication_and_retries() -> anyhow::Result<()>
    {
        use axum::body::Body;
        use http_body_util::BodyExt as _;
        use tower::ServiceExt as _;
        let h = RefreshHarness::new().await?;
        h.script("bad", false)?;
        h.script("good", true)?;
        h.config(&["bad", "good"])?;
        let engine = Arc::new(h.engine.clone());
        let (_, model) = crate::runtime::offline_router(None);
        let state = hya_server::AppState::new(
            engine,
            Arc::new(crate::runtime::agent_with_model(&model, None)),
        );
        let mut notices = state.subscribe_catalog_updates();
        let app = hya_server::router(state);
        let request = || {
            axum::http::Request::post("/v1/bundles:refresh")
                .header("content-type", "application/json")
                .body(Body::from("{\"directory\":\"\"}"))
        };
        let response = app.clone().oneshot(request()?).await?;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await?.to_bytes())?;
        assert!(
            body["errors"]
                .as_array()
                .is_some_and(|errors| !errors.is_empty()),
            "{body}"
        );
        let generation: u64 =
            serde_json::from_value::<String>(body["generation"].clone())?.parse()?;
        assert!(generation > 1, "{body}");
        assert!(
            notices.try_recv().is_ok(),
            "partial publication must notify catalog clients"
        );
        assert!(h.bind().await?.resolve_tool("good__echo").is_some());
        assert_eq!(h.starts("bad"), 1);
        h.script("bad", true)?;
        let response = app.oneshot(request()?).await?;
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await?.to_bytes())?;
        assert!(
            body.get("errors").is_none(),
            "protojson omits empty errors: {body}"
        );
        assert!(h.bind().await?.resolve_tool("bad__echo").is_some());
        Ok(())
    }

    #[tokio::test]
    async fn hot_reload_enabling_disabled_mcp_resolves_its_secret() -> anyhow::Result<()> {
        use hya_server::McpControl as _;
        let h = RefreshHarness::new().await?;
        let script = r#"import json,os,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 result={'protocolVersion':'2024-11-05','capabilities':{},'serverInfo':{'name':'test','version':'1'}} if r['method']=='initialize' else {'tools':[{'name':'echo','description':os.environ['TOKEN'],'inputSchema':{'type':'object'}}]} if r['method']=='tools/list' else {}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
        let control = super::RuntimeMcpControl::new(h.reconciler.clone());
        control
            .upsert(
                "secret".into(),
                McpServerConfig {
                    enabled: Some(false),
                    command: vec!["python3".into(), "-c".into(), script.into()],
                    env: Some(BTreeMap::from([("TOKEN".into(), "{env:PATH}".into())])),
                    ..Default::default()
                },
            )
            .await
            .map_err(anyhow::Error::msg)?;
        control
            .set_enabled("secret".into(), true)
            .await
            .map_err(anyhow::Error::msg)?;
        let binding = h.bind().await?;
        let tool = binding
            .resolve_tool("mcp__secret__echo")
            .ok_or_else(|| anyhow::anyhow!("connected MCP tool must publish"))?;
        assert_eq!(tool.tool.schema().description, std::env::var("PATH")?);
        Ok(())
    }

    struct MarkerTool(&'static str);

    #[async_trait]
    impl Tool for MarkerTool {
        fn name(&self) -> &str {
            self.0
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new(self.0),
                description: format!("{} marker", self.0),
                input_schema: json!({ "type": "object" }),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(json!({ "source": self.0 }))
        }
    }

    struct CloseCounter(Arc<AtomicUsize>);

    impl Drop for CloseCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn desired(source: &SourceId, revision_marker: &str) -> DesiredSource {
        DesiredSource::mcp(
            source.clone(),
            McpServerConfig {
                command: vec![revision_marker.to_string()],
                ..McpServerConfig::default()
            },
        )
    }

    fn desired_plugin(source: &SourceId, revision_marker: &str) -> DesiredSource {
        DesiredSource::plugin(
            source.clone(),
            PluginSpec {
                id: source.configured_id().to_string(),
                kind: PluginKindWire::Rust,
                command: vec![revision_marker.to_string()],
                timeout_ms: None,
                env: BTreeMap::new(),
                plugin_dir: None,
                launch: Default::default(),
            },
        )
    }

    fn prepared(
        source: SourceId,
        declaration_digest: [u8; 32],
        tool_name: &'static str,
        closes: Arc<AtomicUsize>,
    ) -> PreparedSource {
        prepared_with_permission(
            source,
            declaration_digest,
            tool_name,
            closes,
            ToolPermission::Mcp,
        )
    }

    fn prepared_with_permission(
        source: SourceId,
        declaration_digest: [u8; 32],
        tool_name: &'static str,
        closes: Arc<AtomicUsize>,
        permission: ToolPermission,
    ) -> PreparedSource {
        PreparedSource::new(
            source,
            declaration_digest,
            Arc::new(CloseCounter(closes)),
            vec![PreparedExport::tool(
                tool_name,
                Vec::new(),
                Arc::new(MarkerTool(tool_name)),
                permission,
            )],
        )
    }

    /// `POST /v1/mcp` semantics: a server that cannot start is stored and
    /// reported FAILED with its error (the add succeeds), and a disabled
    /// server is stored without connecting.
    #[test]
    fn hot_reload_unrelated_source_does_not_invalidate_pending_preparation() -> anyhow::Result<()> {
        let registry = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry.clone());
        let a = SourceId::mcp("a");
        let first = reconciler.replace_desired(vec![desired(&a, "a")])?;
        let second = reconciler.upsert_mcp(
            "b".into(),
            McpServerConfig {
                command: vec!["b".into()],
                ..Default::default()
            },
        )?;
        assert_eq!(second.sources().len(), 1);
        reconciler.finish_revision(
            &first,
            vec![prepared(
                a.clone(),
                [1; 32],
                "mcp__a__tool",
                Arc::new(AtomicUsize::new(0)),
            )],
        )?;
        assert!(reconciler.status()[&a].effective);
        Ok(())
    }

    #[test]
    fn hot_reload_identical_failed_source_can_retry() -> anyhow::Result<()> {
        let registry = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry);
        let a = SourceId::mcp("a");
        let first = reconciler.replace_desired(vec![desired(&a, "a")])?;
        assert!(
            reconciler
                .finish_revision(&first, vec![PreparedFailure::new(a.clone(), "failed")])
                .is_err()
        );
        let retry = reconciler.upsert_mcp(
            "a".into(),
            McpServerConfig {
                command: vec!["a".into()],
                ..Default::default()
            },
        )?;
        assert_eq!(retry.sources().len(), 1);
        reconciler.finish_revision(
            &retry,
            vec![prepared(
                a.clone(),
                [2; 32],
                "mcp__a__retry",
                Arc::new(AtomicUsize::new(0)),
            )],
        )?;
        assert_eq!(reconciler.status()[&a].observed, ObservedState::Ready);
        Ok(())
    }

    #[test]
    fn hot_reload_mcp_compares_remote_endpoint_and_transport() -> anyhow::Result<()> {
        let registry = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry);
        let config = McpServerConfig {
            url: Some("http://localhost/first".into()),
            ..Default::default()
        };
        reconciler.upsert_mcp("remote".into(), config.clone())?;
        let mut changed = config;
        changed.url = Some("http://localhost/second".into());
        assert_eq!(
            reconciler
                .upsert_mcp("remote".into(), changed.clone())?
                .sources()
                .len(),
            1
        );
        changed.transport = Some("sse".into());
        assert_eq!(
            reconciler
                .upsert_mcp("remote".into(), changed)?
                .sources()
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn mcp_add_keeps_failed_servers_and_never_connects_disabled() -> anyhow::Result<()> {
        use hya_mcp::McpStatus;
        use hya_server::McpControl as _;

        let registry = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let control = super::RuntimeMcpControl::new(Arc::new(RuntimeReconciler::new(registry)));
        let broken = McpServerConfig {
            command: vec!["/nonexistent/hya-mcp-missing-binary".to_string()],
            ..McpServerConfig::default()
        };
        control
            .upsert("broken".to_string(), broken.clone())
            .await
            .map_err(anyhow::Error::msg)?;
        let status = control.status().await;
        assert!(
            matches!(status.get("broken"), Some(McpStatus::Failed { error }) if !error.is_empty()),
            "{status:?}"
        );

        control
            .upsert(
                "off".to_string(),
                McpServerConfig {
                    enabled: Some(false),
                    ..broken
                },
            )
            .await
            .map_err(anyhow::Error::msg)?;
        let status = control.status().await;
        assert_eq!(status.get("off"), Some(&McpStatus::Disabled), "{status:?}");

        // Enabling the disabled server tries to connect; its failure is a
        // FAILED status, not an error.
        assert!(
            control
                .set_enabled("off".to_string(), true)
                .await
                .map_err(anyhow::Error::msg)?
        );
        let status = control.status().await;
        assert!(
            matches!(status.get("off"), Some(McpStatus::Failed { .. })),
            "{status:?}"
        );
        Ok(())
    }

    #[test]
    fn stale_success_is_closed_and_cannot_publish_over_newer_ticket() -> anyhow::Result<()> {
        let registry = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry.clone());
        let source = SourceId::mcp("alpha");
        let old_plan = reconciler.replace_desired(vec![desired(&source, "old")])?;
        let new_plan = reconciler.replace_desired(vec![desired(&source, "new")])?;

        let new_closes = Arc::new(AtomicUsize::new(0));
        let new_outcome = reconciler.finish_revision(
            &new_plan,
            vec![prepared(
                source.clone(),
                [2; 32],
                "mcp__alpha__new_tool",
                new_closes.clone(),
            )],
        )?;
        assert!(new_outcome.published_generation.is_some());

        let stale_closes = Arc::new(AtomicUsize::new(0));
        let stale = reconciler.finish_revision(
            &old_plan,
            vec![prepared(
                source,
                [1; 32],
                "mcp__alpha__old_tool",
                stale_closes.clone(),
            )],
        );
        assert!(matches!(stale, Err(ReconcileError::StaleAttempt { .. })));
        assert_eq!(stale_closes.load(Ordering::SeqCst), 1);
        assert_eq!(new_closes.load(Ordering::SeqCst), 0);

        let names = registry
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"mcp__alpha__new_tool".to_string()));
        assert!(!names.contains(&"mcp__alpha__old_tool".to_string()));
        Ok(())
    }

    #[test]
    fn explicit_removal_publishes_drop_only_despite_unrelated_connect_failure() -> anyhow::Result<()>
    {
        let registry = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry.clone());
        let removed_source = SourceId::mcp("removed");
        let initial = reconciler.replace_desired(vec![desired(&removed_source, "initial")])?;
        let retained_closes = Arc::new(AtomicUsize::new(0));
        reconciler.finish_revision(
            &initial,
            vec![prepared(
                removed_source,
                [3; 32],
                "mcp__removed__tool",
                retained_closes.clone(),
            )],
        )?;

        let workdir = std::env::temp_dir().join("hya-reconcile-removal-binding");
        let old_binding = registry.bind_turn(&workdir)?;
        assert!(old_binding.resolve_tool("mcp__removed__tool").is_some());

        let failing_source = SourceId::mcp("failing");
        let replacement = reconciler.replace_desired(vec![desired(&failing_source, "fails")])?;
        let after_removal_generation = registry.effective_manifest().generation;
        assert!(old_binding.resolve_tool("mcp__removed__tool").is_some());
        assert_eq!(retained_closes.load(Ordering::SeqCst), 0);

        let failed = reconciler.finish_revision(
            &replacement,
            vec![PreparedFailure::new(failing_source, "MCP_HANDSHAKE_FAILED")],
        );
        assert!(matches!(
            failed,
            Err(ReconcileError::PreparationFailed { .. })
        ));
        let after_failure_generation = registry.effective_manifest().generation;
        assert_eq!(after_failure_generation, after_removal_generation);
        let after_failure = registry.bind_turn(&workdir)?;
        assert!(after_failure.resolve_tool("mcp__removed__tool").is_none());
        assert!(old_binding.resolve_tool("mcp__removed__tool").is_some());
        drop(old_binding);
        assert_eq!(retained_closes.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn source_failure_preserves_independent_successes() -> anyhow::Result<()> {
        let registry = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry.clone());
        let ready_source = SourceId::mcp("ready");
        let failed_source = SourceId::mcp("failed");
        let plan = reconciler.replace_desired(vec![
            desired(&ready_source, "ready"),
            desired(&failed_source, "failed"),
        ])?;
        let workdir = std::env::temp_dir().join("hya-reconcile-partial-failure");
        let before_generation = registry.effective_manifest().generation;
        let partial_closes = Arc::new(AtomicUsize::new(0));

        let failed = reconciler.finish_revision(
            &plan,
            vec![
                PreparedResult::from(prepared(
                    ready_source,
                    [4; 32],
                    "mcp__ready__partial_tool",
                    partial_closes.clone(),
                )),
                PreparedResult::from(PreparedFailure::new(
                    failed_source.clone(),
                    "PLUGIN_INITIALIZE_FAILED",
                )),
            ],
        );

        assert!(matches!(
            failed,
            Err(ReconcileError::PreparationFailed { .. })
        ));
        assert_eq!(partial_closes.load(Ordering::SeqCst), 0);
        let after_failure_generation = registry.effective_manifest().generation;
        assert_eq!(after_failure_generation.get(), before_generation.get() + 1);
        let status = reconciler.status();
        let observed = &status[&failed_source];
        assert_eq!(observed.observed, ObservedState::Failed);
        assert_eq!(
            observed.typed_error.as_deref(),
            Some("PLUGIN_INITIALIZE_FAILED")
        );
        assert!(!observed.effective);
        assert_eq!(observed.effective_generation, after_failure_generation);
        let after = registry.bind_turn(&workdir)?;
        assert!(after.resolve_tool("mcp__ready__partial_tool").is_some());
        Ok(())
    }

    #[test]
    fn candidate_rejection_records_failure_and_invalidates_attempt() -> anyhow::Result<()> {
        let registry = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry.clone());
        let source = SourceId::plugin("collision");
        let plan = reconciler.replace_desired(vec![desired_plugin(&source, "collision")])?;
        let first_closes = Arc::new(AtomicUsize::new(0));
        let before = registry.effective_manifest().generation;

        let rejected = reconciler.finish_revision(
            &plan,
            vec![prepared_with_permission(
                source.clone(),
                [20; 32],
                "mcp__reserved__tool",
                first_closes.clone(),
                ToolPermission::Tool,
            )],
        );

        assert!(matches!(rejected, Err(ReconcileError::Runtime(_))));
        assert_eq!(registry.effective_manifest().generation, before);
        assert_eq!(first_closes.load(Ordering::SeqCst), 1);
        let status = reconciler.status();
        let observed = &status[&source];
        assert_eq!(observed.observed, ObservedState::Failed);
        assert!(
            observed
                .typed_error
                .as_deref()
                .is_some_and(|error| error.contains("mcp__reserved__tool"))
        );

        let retry_closes = Arc::new(AtomicUsize::new(0));
        let retry = reconciler.finish_revision(
            &plan,
            vec![prepared_with_permission(
                source,
                [21; 32],
                "retry_after_failure",
                retry_closes.clone(),
                ToolPermission::Tool,
            )],
        );
        assert!(matches!(retry, Err(ReconcileError::InvalidPrepared(_))));
        assert_eq!(retry_closes.load(Ordering::SeqCst), 1);
        assert_eq!(registry.effective_manifest().generation, before);
        Ok(())
    }

    #[test]
    fn mixed_mcp_plugin_plan_publishes_each_complete_source() -> anyhow::Result<()> {
        let registry = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry.clone());
        let mcp = SourceId::mcp("mixed-mcp");
        let plugin = SourceId::plugin("mixed-plugin");
        let initial = reconciler.replace_desired(vec![
            desired(&mcp, "mcp-v1"),
            desired_plugin(&plugin, "plugin-v1"),
        ])?;
        let old_mcp_closes = Arc::new(AtomicUsize::new(0));
        let old_plugin_closes = Arc::new(AtomicUsize::new(0));
        reconciler.finish_revision(
            &initial,
            vec![
                prepared(
                    mcp.clone(),
                    [10; 32],
                    "mcp__mixed__lookup",
                    old_mcp_closes.clone(),
                ),
                prepared_with_permission(
                    plugin.clone(),
                    [11; 32],
                    "mixed-plugin__plugin_lookup",
                    old_plugin_closes.clone(),
                    ToolPermission::Tool,
                ),
            ],
        )?;
        let workdir = std::env::temp_dir().join("hya-reconcile-mixed-revision");
        let old_binding = registry.bind_turn(&workdir)?;

        let replacement = reconciler.replace_desired(vec![
            desired(&mcp, "mcp-v2"),
            desired_plugin(&plugin, "plugin-v2"),
        ])?;
        let partial_closes = Arc::new(AtomicUsize::new(0));
        let incomplete = reconciler.finish_revision(
            &replacement,
            vec![prepared(
                mcp.clone(),
                [12; 32],
                "mcp__mixed__lookup",
                partial_closes.clone(),
            )],
        );
        assert!(matches!(
            incomplete,
            Err(ReconcileError::InvalidPrepared(_))
        ));
        assert_eq!(partial_closes.load(Ordering::SeqCst), 1);
        assert_eq!(
            registry.effective_manifest().generation,
            old_binding.generation()
        );

        let new_mcp_closes = Arc::new(AtomicUsize::new(0));
        let new_plugin_closes = Arc::new(AtomicUsize::new(0));
        let outcome = reconciler.finish_revision(
            &replacement,
            vec![
                prepared(
                    mcp.clone(),
                    [12; 32],
                    "mcp__mixed__lookup",
                    new_mcp_closes.clone(),
                ),
                prepared_with_permission(
                    plugin.clone(),
                    [13; 32],
                    "mixed-plugin__plugin_lookup",
                    new_plugin_closes.clone(),
                    ToolPermission::Tool,
                ),
            ],
        )?;
        let effective = registry.effective_manifest();
        assert_eq!(outcome.published_generation, Some(effective.generation));
        assert_eq!(
            effective.generation.get(),
            old_binding.generation().get() + 2
        );
        let names = registry
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"mcp__mixed__lookup".to_string()));
        assert!(names.contains(&"mixed-plugin__plugin_lookup".to_string()));
        assert_eq!(old_mcp_closes.load(Ordering::SeqCst), 0);
        assert_eq!(old_plugin_closes.load(Ordering::SeqCst), 0);
        assert_eq!(new_mcp_closes.load(Ordering::SeqCst), 0);
        assert_eq!(new_plugin_closes.load(Ordering::SeqCst), 0);
        let status = reconciler.status();
        assert_eq!(status[&mcp].observed_declaration_digest, Some([12; 32]));
        assert_eq!(status[&mcp].effective_declaration_digest, Some([12; 32]));
        assert_eq!(status[&plugin].observed_declaration_digest, Some([13; 32]));
        assert_eq!(status[&plugin].effective_declaration_digest, Some([13; 32]));
        Ok(())
    }
    /// Prepared static Skills must use the shared contribution declaration and preserve parsed metadata.
    #[test]
    fn prepared_static_bundle_skills_require_exact_contributions_and_preserve_metadata()
    -> anyhow::Result<()> {
        use hya_bundle::PreparedResource;
        use hya_plugin::messages::{PluginContributionSet, SkillContribution};

        let bundle_id = "hya/static-skill";
        let source_path = "resources/skills/reviewer.md";
        let content = "---\nname: reviewer\ndescription: reviews code\nallowed-tools: [read, grep]\nmodel: anthropic/claude-sonnet-4-6\n---\nBODY TEXT\n";
        let digest = format!("{:x}", Sha256::digest(content.as_bytes()));
        let prepared = PreparedResource {
            local_id: "reviewer".to_string(),
            stable_id: format!("bundle:{bundle_id}/skill/reviewer"),
            source_path: source_path.to_string(),
            digest: digest.clone(),
            content: content.to_string(),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let valid = PluginContributionSet {
            skills: vec![SkillContribution {
                id: "reviewer".to_string(),
                content: content.to_string(),
                digest: digest.clone(),
            }],
            ..PluginContributionSet::default()
        };

        let parsed = super::adapt_prepared_bundle_skills(
            bundle_id,
            std::slice::from_ref(&prepared),
            &valid,
        )?;
        let [entry] = parsed.as_slice() else {
            panic!("matching prepared Skill contribution must publish one entry");
        };
        assert_eq!(entry.name, "reviewer");
        assert_eq!(entry.description, "reviews code");
        assert_eq!(entry.content, "BODY TEXT\n");
        assert_eq!(entry.allowed_tools, ["read", "grep"]);
        assert_eq!(entry.model.as_deref(), Some("anthropic/claude-sonnet-4-6"));
        assert_eq!(
            entry.path,
            std::path::PathBuf::from(format!("bundle:{bundle_id}/{source_path}"))
        );
        assert_eq!(
            entry.dir,
            std::path::PathBuf::from(format!("bundle:{bundle_id}/resources/skills"))
        );

        let mut wrong_id = valid.clone();
        wrong_id.skills[0].id = "other".to_string();
        assert!(
            super::adapt_prepared_bundle_skills(
                bundle_id,
                std::slice::from_ref(&prepared),
                &wrong_id,
            )
            .is_err(),
            "a contribution id different from the prepared local id must fail closed"
        );

        let mut wrong_content = valid.clone();
        wrong_content.skills[0].content.push_str("tampered");
        wrong_content.skills[0].digest = format!(
            "{:x}",
            Sha256::digest(wrong_content.skills[0].content.as_bytes())
        );
        assert!(
            super::adapt_prepared_bundle_skills(
                bundle_id,
                std::slice::from_ref(&prepared),
                &wrong_content,
            )
            .is_err(),
            "content different from the prepared bytes must fail closed"
        );

        let mut wrong_digest = valid.clone();
        wrong_digest.skills[0].digest = "0".repeat(64);
        assert!(
            super::adapt_prepared_bundle_skills(
                bundle_id,
                std::slice::from_ref(&prepared),
                &wrong_digest,
            )
            .is_err(),
            "a digest different from the prepared digest must fail closed"
        );

        let missing = PluginContributionSet::default();
        assert!(
            super::adapt_prepared_bundle_skills(
                bundle_id,
                std::slice::from_ref(&prepared),
                &missing,
            )
            .is_ok(),
            "static Skills remain available without process declarations"
        );

        let mut extra = valid.clone();
        extra.skills.push(SkillContribution {
            id: "extra".to_string(),
            content: "---\nname: extra\ndescription: extra\n---\nextra\n".to_string(),
            digest: format!(
                "{:x}",
                Sha256::digest(b"---\nname: extra\ndescription: extra\n---\nextra\n")
            ),
        });
        assert!(
            super::adapt_prepared_bundle_skills(
                bundle_id,
                std::slice::from_ref(&prepared),
                &extra,
            )
            .is_err(),
            "an extra undeclared Skill must fail closed"
        );

        let mut duplicate = valid.clone();
        duplicate.skills.push(valid.skills[0].clone());
        assert!(
            super::adapt_prepared_bundle_skills(
                bundle_id,
                std::slice::from_ref(&prepared),
                &duplicate,
            )
            .is_err(),
            "duplicate Skill declarations must fail closed"
        );

        let agent = PreparedAgent {
            id: AgentName::new("static-agent"),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("static agent".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view: ResourceView {
                allow: vec![prepared.stable_id.clone()],
                deny: Vec::new(),
                aliases: BTreeMap::new(),
                namespace: None,
            },
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
        };
        let bundle = Bundle {
            check: None,
            tui: None,
            format_version: 4,
            identity: BundleIdentity {
                id: bundle_id.to_string(),
                version: "1.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
            digest: "test-only".to_string(),
            agents: vec![agent],
            tools: Vec::new(),
            skills: vec![prepared.clone()],
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),

            workflow: None,
            channels: vec![],
            schemas: vec![],
            process: None,
            apis: vec![],
            permission_modes: vec![],
        };
        let bundles = BundleCatalog::from_prepared(&[bundle])?;
        let catalog = Arc::new(AgentCatalog::new(Arc::new(bundles))?);
        let registry = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
        let source_id = SourceId::bundle(bundle_id);
        let source = super::prepared_static_bundle_source(bundle_id, &[prepared], &valid)?
            .into_runtime_source();
        let published = registry.refresh(|candidate| {
            candidate
                .replace_sources_of_kind(hya_core::RuntimeSourceKind::Bundle, vec![source.clone()])
        })?;
        assert_eq!(published, registry.effective_manifest().generation);
        let source_manifest = registry
            .effective_manifest()
            .sources
            .remove(&source_id)
            .ok_or_else(|| anyhow::anyhow!("published static Skill source is missing"))?;
        let [published] = source_manifest.skill_entries.as_slice() else {
            panic!("published static Skill source must retain one metadata entry");
        };
        assert_eq!(published.name, "reviewer");
        assert_eq!(published.description, "reviews code");
        assert_eq!(published.content, "BODY TEXT\n");
        assert_eq!(published.allowed_tools, ["read", "grep"]);
        assert_eq!(
            published.model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(
            published.path,
            std::path::PathBuf::from(format!("bundle:{bundle_id}/{source_path}"))
        );
        Ok(())
    }
}
