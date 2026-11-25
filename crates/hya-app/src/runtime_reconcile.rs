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
pub(crate) struct AttemptTicket {
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
    pub(crate) io_tasks: Option<tokio_util::task::TaskTracker>,
    pub(crate) registry: Arc<RuntimeRegistry>,
    state: Mutex<ReconcileState>,
}

pub(crate) struct ReconcilePlan {
    pub(crate) tickets: BTreeMap<SourceId, AttemptTicket>,
    pub(crate) sources: Vec<DesiredSource>,
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
    pub(crate) hooks: Option<Arc<dyn hya_engine::hooks::HookDispatcher>>,
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
    pub(crate) fn source(&self) -> &SourceId {
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

pub(crate) async fn prepare_desired_source(source: DesiredSource) -> PreparedResult {
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

    /// `POST /v1/mcp` semantics: a server that cannot start is stored and
    /// reported FAILED with its error (the add succeeds), and a disabled
    /// server is stored without connecting.
    #[test]
    fn hot_reload_unrelated_source_does_not_invalidate_pending_preparation() -> anyhow::Result<()> {
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
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
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
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
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
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

    #[test]
    fn stale_success_is_closed_and_cannot_publish_over_newer_ticket() -> anyhow::Result<()> {
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
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
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
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
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
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
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
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
        let _env = StableEnvGuard::new();
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
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
        let _env = StableEnvGuard::new();
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
            private: false,
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
            advertise: Vec::new(),
            handoff: hya_bundle::HandoffPolicy::Model,
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
        let registry = Arc::new(RuntimeRegistry::new(hya_tool_loader::builtins(), catalog));
        let source_id = SourceId::bundle(bundle_id);
        let source = super::prepared_static_bundle_source(bundle_id, &[prepared], &valid)?
            .into_runtime_source();
        let published = registry.refresh(|candidate| {
            candidate.replace_sources_of_kind(
                hya_engine::RuntimeSourceKind::Bundle,
                vec![source.clone()],
            )
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
