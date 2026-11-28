use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use hya_engine::runtime_registry::{RuntimeSourceSkill, SourceSchema};
use hya_engine::{
    RuntimeRefreshError, RuntimeRegistry, RuntimeSource, RuntimeSourceExport, RuntimeSourceId,
    RuntimeSourceKind, RuntimeSourceOwner,
};
use hya_mcp::McpServerConfig;
use hya_plugin::config::PluginSpec;
use hya_proto::ConfigGeneration;
use hya_tool::{Tool, ToolPermission};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(test)]
use crate::bundle_runtime::source::{adapt_prepared_bundle_skills, prepared_static_bundle_source};
use crate::runtime_mcp_source::normalize_mcp;

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
    io_tasks: Option<tokio_util::task::TaskTracker>,
    pub(crate) registry: Arc<RuntimeRegistry>,
    state: Mutex<ReconcileState>,
}

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
    pub(crate) hooks: Option<Arc<dyn hya_engine::NativeHook>>,
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
            io_tasks: None,
        }
    }

    pub(crate) fn with_io_tasks(mut self, tasks: tokio_util::task::TaskTracker) -> Self {
        self.io_tasks = Some(tasks);
        self
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
            let handle = set.spawn(hya_child_io::io_tasks::scope(
                self.io_tasks.clone(),
                async move { prepare_desired_source(source).await },
            ));
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
            match self.finish_revision(&source_plan, [result]) {
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

    pub(crate) fn replace_plugins(
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

async fn prepare_desired_source(source: DesiredSource) -> PreparedResult {
    let id = source.id().clone();
    match source.spec {
        DesiredSpec::Mcp(config) => crate::runtime_mcp_source::prepare(id, config).await,
        DesiredSpec::Plugin(spec) => crate::plugin_adapter::source::prepare(id, spec).await,
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

pub(crate) fn update_bytes(digest: &mut Sha256, value: &[u8]) {
    digest.update(value.len().to_be_bytes());
    digest.update(value);
}

#[cfg(test)]
mod tests {
    use crate::test_env::StableEnvGuard;

    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use hya_bundle::{
        AgentRole, Bundle, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, ResourceView,
    };
    use hya_engine::{AgentCatalog, RuntimeRegistry};
    use hya_mcp::McpServerConfig;
    use hya_plugin::config::PluginSpec;
    use hya_plugin::messages::PluginKindWire;
    use hya_proto::{AgentName, ToolName, ToolSchema};
    use hya_tool::{Tool, ToolCtx, ToolError, ToolPermission};
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
    impl hya_engine::RuntimeSourceOwner for CloseCounter {}

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

    #[tokio::test]
    async fn cancelled_plan_and_ticket_fencing_protect_effective_snapshot() -> anyhow::Result<()> {
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry.clone());

        // 1. Cancelled plan does not publish or advance observed state.
        let before = registry.effective_manifest().generation;
        let cancelled = SourceId::mcp("cancelled");
        let cplan = reconciler.upsert_mcp(
            cancelled.configured_id().into(),
            McpServerConfig {
                command: vec!["/nonexistent/hya-mcp-missing-binary".into()],
                ..Default::default()
            },
        )?;
        let stop = tokio_util::sync::CancellationToken::new();
        stop.cancel();
        assert!(matches!(
            reconciler.reconcile_plan(cplan, stop).await,
            Err(ReconcileError::InvalidPrepared(ref e)) if e == "runtime reconciliation cancelled"
        ));
        assert_eq!(
            reconciler.status()[&cancelled].observed,
            ObservedState::Connecting
        );
        assert_eq!(registry.effective_manifest().generation, before);

        // 2. Unrelated upsert preserves pending ticket; remote endpoint/transport diffs replan.
        let a = SourceId::mcp("a");
        let p1 = reconciler.replace_desired(vec![desired(&a, "a")])?;
        let p2 = reconciler.upsert_mcp(
            "remote".into(),
            McpServerConfig {
                url: Some("http://localhost/1".into()),
                ..Default::default()
            },
        )?;
        assert_eq!(p2.sources().len(), 1);
        assert_eq!(
            reconciler
                .upsert_mcp(
                    "remote".into(),
                    McpServerConfig {
                        url: Some("http://localhost/2".into()),
                        transport: Some("sse".into()),
                        ..Default::default()
                    }
                )?
                .sources()
                .len(),
            1
        );
        reconciler.finish_revision(
            &p1,
            vec![prepared(
                a.clone(),
                [1; 32],
                "mcp__a__tool",
                Arc::new(AtomicUsize::new(0)),
            )],
        )?;
        assert!(reconciler.status()[&a].effective);

        // 3. Stale attempt is closed without overwriting newer ticket.
        let alpha = SourceId::mcp("alpha");
        let old_plan = reconciler.replace_desired(vec![desired(&alpha, "old")])?;
        let new_plan = reconciler.replace_desired(vec![desired(&alpha, "new")])?;
        let new_closes = Arc::new(AtomicUsize::new(0));
        assert!(
            reconciler
                .finish_revision(
                    &new_plan,
                    vec![prepared(
                        alpha.clone(),
                        [2; 32],
                        "mcp__alpha__new",
                        new_closes.clone()
                    )]
                )?
                .published_generation
                .is_some()
        );
        let stale_closes = Arc::new(AtomicUsize::new(0));
        assert!(matches!(
            reconciler.finish_revision(
                &old_plan,
                vec![prepared(
                    alpha,
                    [1; 32],
                    "mcp__alpha__old",
                    stale_closes.clone()
                )]
            ),
            Err(ReconcileError::StaleAttempt { .. })
        ));
        assert_eq!(
            (
                stale_closes.load(Ordering::SeqCst),
                new_closes.load(Ordering::SeqCst)
            ),
            (1, 0)
        );
        let names: Vec<_> = registry
            .tool_schemas()
            .into_iter()
            .map(|s| s.name.as_str().to_string())
            .collect();
        assert!(
            names.contains(&"mcp__alpha__new".into()) && !names.contains(&"mcp__alpha__old".into())
        );
        Ok(())
    }

    #[test]
    fn removal_partial_failure_retry_and_candidate_rejection() -> anyhow::Result<()> {
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry.clone());
        let workdir = std::env::temp_dir().join("hya-reconcile-test");

        // 1. Removal drops immediately while old binding retains owner until dropped, even when new source fails.
        let removed = SourceId::mcp("removed");
        let init = reconciler.replace_desired(vec![desired(&removed, "init")])?;
        let retained_closes = Arc::new(AtomicUsize::new(0));
        reconciler.finish_revision(
            &init,
            vec![prepared(
                removed,
                [3; 32],
                "mcp__removed__tool",
                retained_closes.clone(),
            )],
        )?;
        let old_binding = registry.bind_turn(&workdir)?;
        let failing = SourceId::mcp("failing");
        let repl = reconciler.replace_desired(vec![desired(&failing, "fails")])?;
        let gen_after_drop = registry.effective_manifest().generation;
        assert!(matches!(
            reconciler.finish_revision(
                &repl,
                vec![PreparedFailure::new(
                    failing.clone(),
                    "MCP_HANDSHAKE_FAILED"
                )]
            ),
            Err(ReconcileError::PreparationFailed { .. })
        ));
        assert_eq!(registry.effective_manifest().generation, gen_after_drop);
        assert!(
            registry
                .bind_turn(&workdir)?
                .resolve_tool("mcp__removed__tool")
                .is_none()
        );
        assert_eq!(retained_closes.load(Ordering::SeqCst), 0);
        drop(old_binding);
        assert_eq!(retained_closes.load(Ordering::SeqCst), 1);

        // 2. Identical failed source can retry on explicit upsert.
        let retry = reconciler.upsert_mcp(
            "failing".into(),
            McpServerConfig {
                command: vec!["fails".into()],
                ..Default::default()
            },
        )?;
        assert_eq!(retry.sources().len(), 1);
        reconciler.finish_revision(
            &retry,
            vec![prepared(
                failing.clone(),
                [4; 32],
                "mcp__failing__ok",
                Arc::new(AtomicUsize::new(0)),
            )],
        )?;
        assert_eq!(reconciler.status()[&failing].observed, ObservedState::Ready);

        // 3. Partial failure publishes ready peers; runtime candidate rejection closes owner and invalidates ticket.
        let ready = SourceId::mcp("ready");
        let bad = SourceId::mcp("bad");
        let plan = reconciler.replace_desired(vec![desired(&ready, "r"), desired(&bad, "b")])?;
        let ready_closes = Arc::new(AtomicUsize::new(0));
        assert!(matches!(
            reconciler.finish_revision(
                &plan,
                vec![
                    PreparedResult::from(prepared(
                        ready,
                        [5; 32],
                        "mcp__ready__t",
                        ready_closes.clone()
                    )),
                    PreparedResult::from(PreparedFailure::new(bad.clone(), "ERR")),
                ]
            ),
            Err(ReconcileError::PreparationFailed { .. })
        ));
        assert_eq!(ready_closes.load(Ordering::SeqCst), 0);
        assert!(
            registry
                .bind_turn(&workdir)?
                .resolve_tool("mcp__ready__t")
                .is_some()
        );
        assert_eq!(
            reconciler.status()[&bad].typed_error.as_deref(),
            Some("ERR")
        );

        let col = SourceId::plugin("collision");
        let cplan = reconciler.replace_desired(vec![desired_plugin(&col, "c")])?;
        let c1 = Arc::new(AtomicUsize::new(0));
        assert!(matches!(
            reconciler.finish_revision(
                &cplan,
                vec![prepared_with_permission(
                    col.clone(),
                    [20; 32],
                    "mcp__reserved__tool",
                    c1.clone(),
                    ToolPermission::Tool
                )]
            ),
            Err(ReconcileError::Runtime(_))
        ));
        assert_eq!(c1.load(Ordering::SeqCst), 1);
        assert!(matches!(
            reconciler.finish_revision(
                &cplan,
                vec![prepared_with_permission(
                    col,
                    [21; 32],
                    "retry_ok",
                    Arc::new(AtomicUsize::new(0)),
                    ToolPermission::Tool
                )]
            ),
            Err(ReconcileError::InvalidPrepared(_))
        ));
        Ok(())
    }

    #[test]
    fn mixed_mcp_plugin_plan_publishes_complete_sources_and_tracks_digests() -> anyhow::Result<()> {
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = RuntimeReconciler::new(registry.clone());
        let mcp = SourceId::mcp("mixed-mcp");
        let plugin = SourceId::plugin("mixed-plugin");
        let initial =
            reconciler.replace_desired(vec![desired(&mcp, "m1"), desired_plugin(&plugin, "p1")])?;
        reconciler.finish_revision(
            &initial,
            vec![
                prepared(
                    mcp.clone(),
                    [10; 32],
                    "mcp__mixed__lookup",
                    Arc::new(AtomicUsize::new(0)),
                ),
                prepared_with_permission(
                    plugin.clone(),
                    [11; 32],
                    "mixed-plugin__lookup",
                    Arc::new(AtomicUsize::new(0)),
                    ToolPermission::Tool,
                ),
            ],
        )?;
        let old_binding = registry.bind_turn(&std::env::temp_dir())?;

        // Incomplete result list is rejected and closes staged owner without publishing.
        let repl =
            reconciler.replace_desired(vec![desired(&mcp, "m2"), desired_plugin(&plugin, "p2")])?;
        let partial_closes = Arc::new(AtomicUsize::new(0));
        assert!(matches!(
            reconciler.finish_revision(
                &repl,
                vec![prepared(
                    mcp.clone(),
                    [12; 32],
                    "mcp__mixed__lookup",
                    partial_closes.clone()
                )]
            ),
            Err(ReconcileError::InvalidPrepared(_))
        ));
        assert_eq!(partial_closes.load(Ordering::SeqCst), 1);
        assert_eq!(
            registry.effective_manifest().generation,
            old_binding.generation()
        );

        // Complete result list publishes both and records declaration digests.
        let outcome = reconciler.finish_revision(
            &repl,
            vec![
                prepared(
                    mcp.clone(),
                    [12; 32],
                    "mcp__mixed__lookup",
                    Arc::new(AtomicUsize::new(0)),
                ),
                prepared_with_permission(
                    plugin.clone(),
                    [13; 32],
                    "mixed-plugin__lookup",
                    Arc::new(AtomicUsize::new(0)),
                    ToolPermission::Tool,
                ),
            ],
        )?;
        assert_eq!(
            outcome.published_generation,
            Some(registry.effective_manifest().generation)
        );
        let status = reconciler.status();
        assert_eq!(status[&mcp].effective_declaration_digest, Some([12; 32]));
        assert_eq!(status[&plugin].effective_declaration_digest, Some([13; 32]));
        Ok(())
    }

    #[test]
    fn prepared_static_bundle_skills_require_exact_contributions_and_preserve_metadata()
    -> anyhow::Result<()> {
        let _env = StableEnvGuard::new();
        use hya_bundle::PreparedResource;
        use hya_plugin::messages::{PluginContributionSet, SkillContribution};

        let bundle_id = "hya/static-skill";
        let source_path = "resources/skills/reviewer.md";
        let content = "---\nname: reviewer\ndescription: reviews code\nallowed-tools: [read, grep]\nmodel: anthropic/claude-sonnet-4-6\n---\nBODY TEXT\n";
        let digest = format!("{:x}", Sha256::digest(content.as_bytes()));
        let prepared = PreparedResource {
            local_id: "reviewer".into(),
            stable_id: format!("bundle:{bundle_id}/skill/reviewer"),
            source_path: source_path.into(),
            digest: digest.clone(),
            content: content.into(),
            binary_base64: None,
            aliases: Vec::new(),
            overrides: None,
        };
        let valid = PluginContributionSet {
            skills: vec![SkillContribution {
                id: "reviewer".into(),
                content: content.into(),
                digest,
            }],
            ..Default::default()
        };

        let entries = super::adapt_prepared_bundle_skills(
            bundle_id,
            std::slice::from_ref(&prepared),
            &valid,
        )?;
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(
            (
                entry.name.as_str(),
                entry.content.as_str(),
                entry.model.as_deref()
            ),
            (
                "reviewer",
                "BODY TEXT\n",
                Some("anthropic/claude-sonnet-4-6")
            )
        );
        assert_eq!(entry.allowed_tools, ["read", "grep"]);
        assert!(
            super::adapt_prepared_bundle_skills(
                bundle_id,
                std::slice::from_ref(&prepared),
                &PluginContributionSet::default()
            )
            .is_ok()
        );

        // Mismatched id, content, digest, extra, or duplicate all fail closed.
        let mut wrong_id = valid.clone();
        wrong_id.skills[0].id = "other".into();
        let mut wrong_content = valid.clone();
        wrong_content.skills[0].content.push_str("tampered");
        wrong_content.skills[0].digest = format!(
            "{:x}",
            Sha256::digest(wrong_content.skills[0].content.as_bytes())
        );
        let mut wrong_digest = valid.clone();
        wrong_digest.skills[0].digest = "0".repeat(64);
        let mut extra = valid.clone();
        extra.skills.push(SkillContribution {
            id: "extra".into(),
            content: "---\nname: extra\ndescription: e\n---\ne\n".into(),
            digest: format!(
                "{:x}",
                Sha256::digest(b"---\nname: extra\ndescription: e\n---\ne\n")
            ),
        });
        let mut dup = valid.clone();
        dup.skills.push(valid.skills[0].clone());
        for bad in [&wrong_id, &wrong_content, &wrong_digest, &extra, &dup] {
            assert!(
                super::adapt_prepared_bundle_skills(
                    bundle_id,
                    std::slice::from_ref(&prepared),
                    bad
                )
                .is_err()
            );
        }

        // Static bundle source publishes parsed skill metadata into RuntimeRegistry.
        let bundle = Bundle {
            check: None,
            tui: None,
            format_version: 4,
            identity: BundleIdentity {
                id: bundle_id.into(),
                version: "1.0.0".into(),
                publisher: "hya-tests".into(),
            },
            namespace: None,
            overrides: None,
            digest: "test-only".into(),
            agents: vec![PreparedAgent {
                private: false,
                id: AgentName::new("static-agent"),
                description: None,
                role: AgentRole::Main,
                color: None,
                prompt: Some("static".into()),
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
                advertise: Vec::new(),
                handoff: hya_bundle::HandoffPolicy::Model,
            }],
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
            commands: Vec::new(),
            runtime: None,
            permission_modes: vec![],
        };
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
            Arc::new(AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(
                &[bundle],
            )?))?),
        ));
        let source = super::prepared_static_bundle_source(bundle_id, &[prepared], &valid)?
            .into_runtime_source();
        registry.refresh(|c| {
            c.replace_sources_of_kind(hya_engine::RuntimeSourceKind::Bundle, vec![source])
        })?;
        let manifest = registry
            .effective_manifest()
            .sources
            .remove(&SourceId::bundle(bundle_id))
            .ok_or_else(|| anyhow::anyhow!("missing bundle source"))?;
        assert_eq!(manifest.skill_entries.len(), 1);
        assert_eq!(manifest.skill_entries[0].name, "reviewer");
        Ok(())
    }
}
