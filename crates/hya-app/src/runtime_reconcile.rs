use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use hya_bundle::PreparedResource;
use hya_core::runtime_registry::RuntimeSourceSkill;
use hya_core::{
    RuntimeRefreshError, RuntimeRegistry, RuntimeSource, RuntimeSourceExport, RuntimeSourceId,
    RuntimeSourceKind, RuntimeSourceOwner,
};
use hya_mcp::{McpServerConfig, McpStatus, PreparedMcpServer};
use hya_plugin::PreparedPlugin;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{PluginContributionSet, SkillContribution};
use hya_proto::ConfigGeneration;
use hya_tool::{SkillCatalogEntry, Tool, ToolPermission, parse_skill};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) type SourceId = RuntimeSourceId;

#[derive(Clone)]
pub(crate) enum DesiredSpec {
    Mcp(McpServerConfig),
    Plugin(PluginSpec),
}

#[derive(Clone)]
pub(crate) struct DesiredSource {
    id: SourceId,
    spec: DesiredSpec,
    spec_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AttemptTicket {
    revision: u64,
    source: SourceId,
}

#[derive(Clone)]
struct DesiredRecord {
    spec: DesiredSpec,
    spec_digest: [u8; 32],
}

#[derive(Clone)]
struct AttemptRecord {
    revision: u64,
    ticket: AttemptTicket,
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
    revision: u64,
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
    pub(crate) observed_revision: Option<u64>,
    pub(crate) observed_ticket_revision: Option<u64>,
}

#[derive(Default)]
struct ReconcileState {
    desired_revision: u64,
    desired: BTreeMap<SourceId, DesiredRecord>,
    attempt: BTreeMap<SourceId, AttemptRecord>,
    observed: BTreeMap<SourceId, ObservedRecord>,
}

pub(crate) struct RuntimeReconciler {
    registry: Arc<RuntimeRegistry>,
    state: Mutex<ReconcileState>,
}

pub(crate) struct RuntimeMcpControl {
    reconciler: Arc<RuntimeReconciler>,
}

type ControlFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) struct ReconcilePlan {
    revision: u64,
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
        let spec_digest = digest_mcp(&spec);
        Self {
            id,
            spec: DesiredSpec::Mcp(spec),
            spec_digest,
        }
    }

    #[must_use]
    pub(crate) fn plugin(id: SourceId, spec: PluginSpec) -> Self {
        let spec_digest = digest_plugin(&spec);
        Self {
            id,
            spec: DesiredSpec::Plugin(spec),
            spec_digest,
        }
    }

    #[must_use]
    pub(crate) fn id(&self) -> &SourceId {
        &self.id
    }

    fn into_record(self) -> (SourceId, DesiredRecord) {
        (
            self.id,
            DesiredRecord {
                spec: self.spec,
                spec_digest: self.spec_digest,
            },
        )
    }
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
            spec_digest: self.spec_digest,
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
        RuntimeSource::new(self.id, self.declaration_digest, self.owner, exports)
            .with_skills(self.skills)
            .with_resources(self.resources)
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

/// Validate and parse every prepared bundle Skill exactly once at the contribution seam.
fn prepared_bundle_skill_exports(
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
    if prepared_by_id.len() != contributions.skills.len() {
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
        let path = PathBuf::from(format!("bundle:{bundle_id}/{}", resource.source_path));
        let entry = parse_skill_contribution(contribution, path)?;
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
    let exports = plugin
        .tools()
        .into_iter()
        .map(|tool| {
            let declared_id = tool.name().to_string();
            PreparedExport::tool(declared_id, Vec::new(), tool, ToolPermission::Tool)
        })
        .collect();
    Ok(PreparedSource::new(id, declaration_digest, Arc::new(plugin), exports).with_skills(skills))
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
        self.apply_desired_locked(&mut state, next)
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
        next.insert(id, record);
        self.apply_desired_locked(&mut state, next)
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
        next.insert(id, record);
        self.apply_desired_locked(&mut state, next).map(Some)
    }

    fn apply_desired_locked(
        &self,
        state: &mut ReconcileState,
        next: BTreeMap<SourceId, DesiredRecord>,
    ) -> Result<ReconcilePlan, ReconcileError> {
        if desired_matches(&state.desired, &next) {
            return Ok(ReconcilePlan {
                revision: state.desired_revision,
                tickets: BTreeMap::new(),
                sources: Vec::new(),
            });
        }
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
        let changed =
            next.iter()
                .filter(|(_, record)| record.spec.enabled())
                .filter(|(id, record)| {
                    state.desired.get(*id).is_none_or(|old| {
                        !old.spec.enabled() || old.spec_digest != record.spec_digest
                    })
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
        let sources = changed
            .iter()
            .filter_map(|id| next.get(id).map(|record| record.as_source(id.clone())))
            .collect::<Vec<_>>();

        state.desired_revision = revision;
        state.desired = next;
        state.attempt.clear();
        let mut tickets = BTreeMap::new();
        for source in changed {
            let ticket = AttemptTicket {
                revision,
                source: source.clone(),
            };
            state.attempt.insert(
                source.clone(),
                AttemptRecord {
                    revision,
                    ticket: ticket.clone(),
                },
            );
            state.observed.insert(
                source.clone(),
                ObservedRecord {
                    revision,
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
                state.observed.insert(
                    source,
                    ObservedRecord {
                        revision,
                        ticket: None,
                        state: ObservedState::Removed,
                        declaration_digest: None,
                        typed_error: None,
                    },
                );
            }
        }

        Ok(ReconcilePlan {
            revision,
            tickets,
            sources,
        })
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
        if state.desired_revision != plan.revision {
            let current = state.desired_revision;
            return reject_after_unlock(
                state,
                results,
                ReconcileError::StaleAttempt {
                    attempted: plan.revision,
                    current,
                },
            );
        }

        let mut seen = BTreeSet::new();
        for result in &results {
            let source = result.source();
            if let Err(error) = validate_result_source(&state, plan, source) {
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

        let mut failures = BTreeMap::new();
        for result in &results {
            if let PreparedResult::Failed(failure) = result {
                failures.insert(failure.source.clone(), failure.error.clone());
            }
        }
        if let Some((source_id, error)) = failures.first_key_value() {
            let aborted = format!("REVISION_PREPARATION_FAILED: {source_id}: {error}");
            fail_attempt_locked(&mut state, plan, &failures, &aborted);
            let failure = ReconcileError::PreparationFailed {
                source_id: source_id.clone(),
                error: error.clone(),
            };
            return reject_after_unlock(state, results, failure);
        }

        let ready = results
            .into_iter()
            .filter_map(|result| match result {
                PreparedResult::Ready(source) => Some(source),
                PreparedResult::Failed(_) => None,
            })
            .collect::<Vec<_>>();
        let observations = ready
            .iter()
            .map(|source| (source.id.clone(), source.declaration_digest))
            .collect::<Vec<_>>();
        let runtime_sources = ready
            .into_iter()
            .map(PreparedSource::into_runtime_source)
            .collect::<Vec<_>>();
        let before = self.registry.effective_manifest();
        let publication = self
            .registry
            .refresh(|candidate| candidate.upsert_sources(runtime_sources.clone()));
        let generation = match publication {
            Ok(generation) => generation,
            Err(error) => {
                let typed_error = format!("RUNTIME_CANDIDATE_REJECTED: {error}");
                fail_attempt_locked(&mut state, plan, &BTreeMap::new(), &typed_error);
                return reject_after_unlock(state, runtime_sources, error.into());
            }
        };
        let published_generation = (generation != before.generation).then_some(generation);
        for (source, declaration_digest) in observations {
            let ticket = plan.tickets.get(&source).cloned();
            state.attempt.remove(&source);
            state.observed.insert(
                source,
                ObservedRecord {
                    revision: plan.revision,
                    ticket,
                    state: ObservedState::Ready,
                    declaration_digest: Some(declaration_digest),
                    typed_error: None,
                },
            );
        }
        let outcome = ReconcileOutcome {
            published_generation,
        };
        drop(state);
        drop(runtime_sources);
        Ok(outcome)
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
                    observed_revision: observed.map(|record| record.revision),
                    observed_ticket_revision: observed
                        .and_then(|record| record.ticket.as_ref())
                        .map(|ticket| ticket.revision),
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
        plan: ReconcilePlan,
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
        let mut results = Vec::new();
        while let Some(joined) = set.join_next_with_id().await {
            match joined {
                Ok((id, result)) => {
                    tasks.remove(&id);
                    results.push(result);
                }
                Err(error) => {
                    let Some(source) = tasks.remove(&error.id()) else {
                        return Err(ReconcileError::InvalidPrepared(format!(
                            "MCP preparation task {} had no source ticket",
                            error.id()
                        )));
                    };
                    results.push(PreparedResult::Failed(PreparedFailure::new(
                        source,
                        format!("MCP_PREPARE_TASK_FAILED: {error}"),
                    )));
                }
            }
        }
        self.reconciler.finish_revision(&plan, results)
    }

    fn mcp_status(&self) -> BTreeMap<String, McpStatus> {
        self.reconciler
            .status()
            .into_iter()
            .filter(|(source, _)| source.kind() == RuntimeSourceKind::Mcp)
            .map(|(source, status)| {
                let declaration_matches = status.observed_declaration_digest.is_some()
                    && status.observed_declaration_digest == status.effective_declaration_digest;
                let ticket_is_current = status.observed_revision.is_some()
                    && status.observed_revision == status.observed_ticket_revision;
                let value = match status.observed {
                    ObservedState::Connecting => McpStatus::Connecting,
                    ObservedState::Ready
                        if status.effective && declaration_matches && ticket_is_current =>
                    {
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
            self.reconcile_plan(plan)
                .await
                .map(|outcome| {
                    trace_publication(&outcome);
                })
                .map_err(|error| error.to_string())
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
            self.reconcile_plan(plan)
                .await
                .map(|outcome| {
                    trace_publication(&outcome);
                    true
                })
                .map_err(|error| error.to_string())
        })
    }

    fn resources(&self) -> ControlFuture<'_, BTreeMap<String, Value>> {
        Box::pin(async move { self.effective_resources() })
    }
}

pub(crate) async fn prepare_desired_source(source: DesiredSource) -> PreparedResult {
    let id = source.id().clone();
    match source.spec {
        DesiredSpec::Mcp(config) => {
            match hya_mcp::prepare(id.configured_id().to_string(), config).await {
                Ok(server) => PreparedResult::Ready(prepared_mcp_source(id, server)),
                Err(error) => PreparedResult::Failed(PreparedFailure::new(
                    id,
                    format!("MCP_START_FAILED: {error}"),
                )),
            }
        }
        DesiredSpec::Plugin(spec) => PreparedResult::Failed(PreparedFailure::new(
            id,
            format!("PLUGIN_DYNAMIC_RECONCILIATION_UNSUPPORTED: {}", spec.id),
        )),
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

fn desired_matches(
    left: &BTreeMap<SourceId, DesiredRecord>,
    right: &BTreeMap<SourceId, DesiredRecord>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, left)| {
            right
                .get(id)
                .is_some_and(|right| left.spec_digest == right.spec_digest)
        })
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
        return Err(ReconcileError::InvalidPrepared(format!(
            "missing active attempt for {source}"
        )));
    };
    if active.revision != plan.revision || active.ticket != *ticket {
        return Err(ReconcileError::InvalidPrepared(format!(
            "ticket mismatch for {source}"
        )));
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

fn fail_attempt_locked(
    state: &mut ReconcileState,
    plan: &ReconcilePlan,
    source_errors: &BTreeMap<SourceId, String>,
    fallback: &str,
) {
    for source in plan.tickets.keys() {
        state.attempt.remove(source);
        state.observed.insert(
            source.clone(),
            ObservedRecord {
                revision: plan.revision,
                ticket: plan.tickets.get(source).cloned(),
                state: ObservedState::Failed,
                declaration_digest: None,
                typed_error: Some(
                    source_errors
                        .get(source)
                        .map_or_else(|| fallback.to_string(), Clone::clone),
                ),
            },
        );
    }
}

fn digest_mcp(spec: &McpServerConfig) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"hya:mcp-spec:v1\0");
    update_strings(&mut digest, &spec.command);
    if let Some(env) = &spec.env {
        for (key, value) in env {
            update_bytes(&mut digest, key.as_bytes());
            update_bytes(&mut digest, value.as_bytes());
        }
    }
    update_bytes(&mut digest, &[u8::from(spec.enabled.unwrap_or(true))]);
    update_bytes(
        &mut digest,
        &spec.timeout_ms.unwrap_or_default().to_be_bytes(),
    );
    digest.finalize().into()
}

fn digest_plugin(spec: &PluginSpec) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"hya:plugin-spec:v1\0");
    update_bytes(&mut digest, spec.id.as_bytes());
    update_bytes(&mut digest, format!("{:?}", spec.kind).as_bytes());
    update_strings(&mut digest, &spec.command);
    update_bytes(
        &mut digest,
        &spec.timeout_ms.unwrap_or_default().to_be_bytes(),
    );
    for (key, value) in &spec.env {
        update_bytes(&mut digest, key.as_bytes());
        update_bytes(&mut digest, value.as_bytes());
    }
    for (hook, posture) in &spec.posture_overrides {
        update_bytes(&mut digest, hook.as_str().as_bytes());
        update_bytes(&mut digest, format!("{posture:?}").as_bytes());
    }
    digest.finalize().into()
}

fn update_strings(digest: &mut Sha256, values: &[String]) {
    for value in values {
        update_bytes(digest, value.as_bytes());
    }
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
        AgentRole, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, PreparedAgentBundle,
        PreparedInstallableBundle, ResourceView, SpawnLifecycle,
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
                posture_overrides: BTreeMap::new(),
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
                "new_tool",
                new_closes.clone(),
            )],
        )?;
        assert!(new_outcome.published_generation.is_some());

        let stale_closes = Arc::new(AtomicUsize::new(0));
        let stale = reconciler.finish_revision(
            &old_plan,
            vec![prepared(source, [1; 32], "old_tool", stale_closes.clone())],
        );
        assert!(matches!(stale, Err(ReconcileError::StaleAttempt { .. })));
        assert_eq!(stale_closes.load(Ordering::SeqCst), 1);
        assert_eq!(new_closes.load(Ordering::SeqCst), 0);

        let names = registry
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"new_tool".to_string()));
        assert!(!names.contains(&"old_tool".to_string()));
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
                "removed_tool",
                retained_closes.clone(),
            )],
        )?;

        let workdir = std::env::temp_dir().join("hya-reconcile-removal-binding");
        let old_binding = registry.bind_turn(&workdir)?;
        assert!(old_binding.resolve_tool("removed_tool").is_some());

        let failing_source = SourceId::mcp("failing");
        let replacement = reconciler.replace_desired(vec![desired(&failing_source, "fails")])?;
        let after_removal_generation = registry.effective_manifest().generation;
        assert!(old_binding.resolve_tool("removed_tool").is_some());
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
        assert!(after_failure.resolve_tool("removed_tool").is_none());
        assert!(old_binding.resolve_tool("removed_tool").is_some());
        drop(old_binding);
        assert_eq!(retained_closes.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn current_failure_keeps_generation_and_closes_partial_successes() -> anyhow::Result<()> {
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
                    "partial_tool",
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
        assert_eq!(partial_closes.load(Ordering::SeqCst), 1);
        let after_failure_generation = registry.effective_manifest().generation;
        assert_eq!(after_failure_generation, before_generation);
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
        assert!(after.resolve_tool("partial_tool").is_none());
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
                "read",
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
                .is_some_and(|error| error.contains("duplicate tool name: read"))
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
    fn mixed_mcp_plugin_revision_publishes_exactly_once_only_when_complete() -> anyhow::Result<()> {
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
                    "plugin_lookup",
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
                    "plugin_lookup",
                    new_plugin_closes.clone(),
                    ToolPermission::Tool,
                ),
            ],
        )?;
        let effective = registry.effective_manifest();
        assert_eq!(outcome.published_generation, Some(effective.generation));
        assert_eq!(
            effective.generation.get(),
            old_binding.generation().get() + 1
        );
        let names = registry
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"mcp__mixed__lookup".to_string()));
        assert!(names.contains(&"plugin_lookup".to_string()));
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
            .is_err(),
            "a missing prepared Skill declaration must fail closed"
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
            spawn_lifecycle: SpawnLifecycle::Transient,
            resource_view: ResourceView {
                allow: vec![prepared.stable_id.clone()],
                deny: Vec::new(),
                aliases: BTreeMap::new(),
                namespace: None,
            },
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
        };
        let bundle = PreparedAgentBundle {
            format_version: 2,
            identity: BundleIdentity {
                id: bundle_id.to_string(),
                version: "1.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            digest: "test-only".to_string(),
            agent,
            tools: Vec::new(),
            skills: vec![prepared.clone()],
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        };
        let bundles =
            BundleCatalog::from_prepared(&[PreparedInstallableBundle::Agent(Box::new(bundle))])?;
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
