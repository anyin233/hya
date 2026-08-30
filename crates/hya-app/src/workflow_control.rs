//! Shared Workflow discovery and durable Session control.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_core::{
    AgentSpec, CategoryRegistry, CompiledWorkflow, ResidentSupervisor, SessionEngine, TurnBinding,
    WorkflowError, WorkflowRoutingContext, WorkflowRunContext, WorkflowStatus,
    discover_workflow_files_in_root, load_workflow_file, prepare_workflow_run_for_actor,
    workflow_dirs_for_workdir,
};
use hya_proto::{
    Envelope, Event, OwnerRunId, SessionId, WorkflowAvailability, WorkflowCommand,
    WorkflowCommandResult, WorkflowDelivery, WorkflowIdentity, WorkflowInfo, WorkflowProjection,
    WorkflowRevision, WorkflowRunId, WorkflowRunProjection, WorkflowRunResult, WorkflowRunStatus,
    WorkflowSourceId, WorkflowStageInfo, WorkflowStagePlan, WorkflowSummary,
};
use hya_tool::ToolOperation;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Bundle identity reserved for the read-only first-party Workflow catalog.
pub const FIRST_PARTY_BUNDLE_ID: &str = "hya/plan-impl-review";

const BUNDLE_WORKFLOW_REVISION_DOMAIN: &[u8] = b"hya.workflow.bundle-revision/v1\0";

/// Source tier owning one resolved Workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowCatalogOwner {
    /// Project-local Workflow file.
    Project,
    /// User-level Workflow file.
    User,
    /// Installed WorkflowBundle payload.
    Installed {
        /// Owning installed bundle id.
        bundle_id: String,
    },
    /// Read-only first-party WorkflowBundle payload.
    FirstParty {
        /// Owning first-party bundle id.
        bundle_id: String,
    },
}

/// Explicit filesystem roots used by one immutable Workflow catalog build.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowCatalogRoots {
    project: PathBuf,
    user: Option<PathBuf>,
}

impl WorkflowCatalogRoots {
    /// Construct roots from Workflow directories, not their parent workdirs.
    #[must_use]
    pub fn new(project: PathBuf, user: Option<PathBuf>) -> Self {
        Self { project, user }
    }

    /// Derive the production project/user roots for one workdir.
    #[must_use]
    pub fn for_workdir(workdir: &Path) -> Self {
        let mut roots = workflow_dirs_for_workdir(workdir).into_iter();
        Self {
            project: roots
                .next()
                .unwrap_or_else(|| workdir.join(".hya/workflows")),
            user: roots.next(),
        }
    }
}

/// One immutable Workflow plus its exact identity and source owner.
pub struct ResolvedWorkflow {
    identity: WorkflowIdentity,
    workflow: Arc<CompiledWorkflow>,
    display_path: String,
    owner: WorkflowCatalogOwner,
}

impl ResolvedWorkflow {
    /// Return the exact source/name/revision identity.
    #[must_use]
    pub fn identity(&self) -> &WorkflowIdentity {
        &self.identity
    }

    /// Return the validated compiled Workflow.
    #[must_use]
    pub fn workflow(&self) -> &CompiledWorkflow {
        self.workflow.as_ref()
    }

    /// Return the path or package source label used for diagnostics.
    #[must_use]
    pub fn display_path(&self) -> &str {
        &self.display_path
    }

    /// Return the source tier that owns this Workflow.
    #[must_use]
    pub fn owner(&self) -> &WorkflowCatalogOwner {
        &self.owner
    }
}

/// One invalid filesystem source retained for list diagnostics and stale state.
struct InvalidWorkflowSource {
    name: String,
    source: WorkflowSourceId,
    path: String,
    error: String,
}

/// One immutable valid/invalid row in catalog precedence order.
enum WorkflowCatalogRow {
    Valid(usize),
    Invalid(InvalidWorkflowSource),
}

/// Immutable resolver over project, user, installed, and first-party sources.
pub struct WorkflowCatalog {
    entries: Vec<ResolvedWorkflow>,
    rows: Vec<WorkflowCatalogRow>,
}

impl WorkflowCatalog {
    /// Build one catalog from explicit filesystem roots and a pinned runtime.
    ///
    /// The binding supplies the exact installed/first-party bundle snapshot;
    /// catalog construction never consults a newer runtime generation.
    ///
    /// # Errors
    /// Returns a control error when a prepared bundle Workflow cannot compile or
    /// disagrees with its prepared identity.
    pub fn build(
        roots: WorkflowCatalogRoots,
        binding: &TurnBinding,
    ) -> Result<Self, WorkflowControlError> {
        let mut catalog = Self {
            entries: Vec::new(),
            rows: Vec::new(),
        };
        catalog.append_filesystem_root(&roots.project, "project");
        if let Some(user) = roots.user.as_deref() {
            catalog.append_filesystem_root(user, "user");
        }
        for bundle in binding.bundle_catalog().bundles() {
            let Some(prepared) = bundle.workflow() else {
                continue;
            };
            let source = format!("bundle:{}/workflow/{}", bundle.identity().id, prepared.id);
            let workflow =
                hya_workflow::compile(hya_workflow::WorkflowSource::new(&source, &prepared.source))
                    .map_err(|error| {
                        WorkflowControlError::InvalidSource(bounded_error(&error.to_string()))
                    })?;
            if workflow.definition().name() != prepared.id {
                return Err(WorkflowControlError::InvalidSource(format!(
                    "prepared Workflow `{}` declares `{}`",
                    prepared.id,
                    workflow.definition().name()
                )));
            }
            let compiler_revision =
                WorkflowRevision::from_bytes(workflow.revision().as_bytes()).to_string();
            if prepared.compiler_revision != compiler_revision {
                return Err(WorkflowControlError::InvalidSource(format!(
                    "prepared Workflow `{}` compiler revision does not match current compiler",
                    prepared.id
                )));
            }
            let revision = bundle_workflow_revision(&workflow, bundle.digest());
            let owner = if bundle.identity().id == FIRST_PARTY_BUNDLE_ID {
                WorkflowCatalogOwner::FirstParty {
                    bundle_id: bundle.identity().id.clone(),
                }
            } else {
                WorkflowCatalogOwner::Installed {
                    bundle_id: bundle.identity().id.clone(),
                }
            };
            let entry = ResolvedWorkflow {
                identity: WorkflowIdentity {
                    source: WorkflowSourceId::new(source),
                    name: prepared.id.clone(),
                    revision,
                },
                workflow: Arc::new(workflow),
                display_path: format!("{}:{}", bundle.identity().id, prepared.source_path),
                owner,
            };
            catalog.entries.push(entry);
            catalog
                .rows
                .push(WorkflowCatalogRow::Valid(catalog.entries.len() - 1));
        }
        Ok(catalog)
    }

    /// Return the effective list in source precedence order.
    #[must_use]
    pub fn list(&self) -> Vec<WorkflowSummary> {
        let mut seen = BTreeSet::new();
        let mut summaries = Vec::new();
        for row in &self.rows {
            match row {
                WorkflowCatalogRow::Valid(index) => {
                    let entry = &self.entries[*index];
                    if !seen.insert(entry.identity.name.clone()) {
                        continue;
                    }
                    summaries.push(WorkflowSummary {
                        name: entry.identity.name.clone(),
                        description: entry.workflow.definition().description().to_string(),
                        source: Some(entry.identity.source.clone()),
                        revision: Some(entry.identity.revision),
                        stages: entry
                            .workflow
                            .plan()
                            .stages()
                            .iter()
                            .map(|stage| stage.id().to_string())
                            .collect(),
                        path: entry.display_path.clone(),
                        error: None,
                    });
                }
                WorkflowCatalogRow::Invalid(invalid) => summaries.push(WorkflowSummary {
                    name: invalid.name.clone(),
                    description: String::new(),
                    source: None,
                    revision: None,
                    stages: Vec::new(),
                    path: invalid.path.clone(),
                    error: Some(invalid.error.clone()),
                }),
            }
        }
        summaries
    }

    /// Resolve a qualified source or an effective bare Workflow name.
    ///
    /// Project/user names shadow bundles. A bare bundle name resolves only when
    /// exactly one bundle owns that name; qualified bundle ids are exact.
    ///
    /// # Errors
    /// Returns [`WorkflowControlError::NotFound`] for an unavailable name and
    /// [`WorkflowControlError::InvalidSource`] for a matching broken file.
    pub fn resolve(&self, reference: &str) -> Result<&ResolvedWorkflow, WorkflowControlError> {
        if reference.starts_with("bundle:")
            || reference.starts_with("project:")
            || reference.starts_with("user:")
        {
            for row in &self.rows {
                match row {
                    WorkflowCatalogRow::Valid(index)
                        if self.entries[*index].identity.source.as_str() == reference =>
                    {
                        return Ok(&self.entries[*index]);
                    }
                    WorkflowCatalogRow::Invalid(invalid)
                        if invalid.source.as_str() == reference =>
                    {
                        return Err(WorkflowControlError::InvalidSource(invalid.error.clone()));
                    }
                    _ => {}
                }
            }
            return Err(WorkflowControlError::NotFound {
                name: reference.to_string(),
            });
        }

        let mut bundle_match = None;
        for row in &self.rows {
            let WorkflowCatalogRow::Valid(index) = row else {
                continue;
            };
            let entry = &self.entries[*index];
            if entry.identity.name != reference {
                continue;
            }
            if matches!(
                &entry.owner,
                WorkflowCatalogOwner::Project | WorkflowCatalogOwner::User
            ) {
                return Ok(entry);
            }
            if bundle_match.is_some() {
                bundle_match = None;
                break;
            }
            bundle_match = Some(entry);
        }
        if let Some(entry) = bundle_match {
            return Ok(entry);
        }
        for row in &self.rows {
            if let WorkflowCatalogRow::Invalid(invalid) = row
                && invalid.name == reference
            {
                return Err(WorkflowControlError::InvalidSource(invalid.error.clone()));
            }
        }
        Err(WorkflowControlError::NotFound {
            name: reference.to_string(),
        })
    }

    /// Resolve one Workflow and project its immutable compiled metadata.
    ///
    /// # Errors
    /// Propagates the exact resolution error from [`Self::resolve`].
    pub fn info(&self, reference: &str) -> Result<WorkflowInfo, WorkflowControlError> {
        let entry = self.resolve(reference)?;
        Ok(workflow_info(entry))
    }

    /// Compare one persisted identity with this exact catalog snapshot.
    #[must_use]
    pub fn availability(&self, selection: &WorkflowIdentity) -> WorkflowAvailability {
        for row in &self.rows {
            match row {
                WorkflowCatalogRow::Valid(index)
                    if self.entries[*index].identity.source == selection.source =>
                {
                    return if &self.entries[*index].identity == selection {
                        WorkflowAvailability::Available
                    } else {
                        WorkflowAvailability::Stale
                    };
                }
                WorkflowCatalogRow::Invalid(invalid) if invalid.source == selection.source => {
                    return WorkflowAvailability::Stale;
                }
                _ => {}
            }
        }
        WorkflowAvailability::Unavailable
    }

    /// Append valid and invalid filesystem rows below one explicit root.
    fn append_filesystem_root(&mut self, root: &Path, tier: &str) {
        for path in discover_workflow_files_in_root(root) {
            let relative = path.strip_prefix(root).map_or_else(
                |_| path.display().to_string(),
                |value| value.display().to_string(),
            );
            let source_value = if tier == "project" {
                relative
            } else {
                path.display().to_string()
            };
            let source = WorkflowSourceId::new(format!("{tier}:{source_value}"));
            match load_workflow_file(&path) {
                Ok(workflow) => {
                    let entry = ResolvedWorkflow {
                        identity: WorkflowIdentity {
                            source,
                            name: workflow.definition().name().to_string(),
                            revision: WorkflowRevision::from_bytes(workflow.revision().as_bytes()),
                        },
                        workflow: Arc::new(workflow),
                        display_path: path.display().to_string(),
                        owner: if tier == "project" {
                            WorkflowCatalogOwner::Project
                        } else {
                            WorkflowCatalogOwner::User
                        },
                    };
                    self.entries.push(entry);
                    self.rows
                        .push(WorkflowCatalogRow::Valid(self.entries.len() - 1));
                }
                Err(error) => self
                    .rows
                    .push(WorkflowCatalogRow::Invalid(InvalidWorkflowSource {
                        name: source_stem(&path),
                        source,
                        path: path.display().to_string(),
                        error: bounded_error(&error.to_string()),
                    })),
            }
        }
    }
}

/// Fold one bundle digest into a compiled Workflow revision.
fn bundle_workflow_revision(workflow: &CompiledWorkflow, bundle_digest: &str) -> WorkflowRevision {
    let mut hasher = Sha256::new();
    hasher.update(BUNDLE_WORKFLOW_REVISION_DOMAIN);
    hasher.update(workflow.revision().as_bytes());
    hasher.update(bundle_digest.as_bytes());
    WorkflowRevision::from_bytes(hasher.finalize().into())
}

/// Stable app-control failures shared by tool, CLI, HTTP, and SDK adapters.
#[derive(Debug, Error)]
pub enum WorkflowControlError {
    /// No compiled Workflow with the requested name exists in the Session catalog.
    #[error("Workflow `{name}` was not found")]
    NotFound {
        /// Requested declared name.
        name: String,
    },
    /// A discovered source did not compile.
    #[error("invalid Workflow source: {0}")]
    InvalidSource(String),
    /// The caller selected against an obsolete compiler revision.
    #[error("Workflow revision changed: expected {expected}, current {actual}")]
    StaleRevision {
        /// Revision supplied by the caller.
        expected: WorkflowRevision,
        /// Current compiler revision.
        actual: WorkflowRevision,
    },
    /// The Session does not exist or has no usable work directory.
    #[error("Session `{session}` was not found")]
    SessionNotFound {
        /// Requested Session.
        session: SessionId,
    },
    /// No Workflow has been selected for a selected-run request.
    #[error("Session `{session}` has no selected Workflow")]
    MissingSelection {
        /// Requested Session.
        session: SessionId,
    },
    /// Runtime inputs do not match the compiled Workflow declaration.
    #[error("invalid Workflow inputs: {0}")]
    InvalidInput(String),
    /// The caller cannot spawn one declared worker or verifier Agent.
    #[error("Workflow authorization failed: {0}")]
    Unauthorized(String),
    /// A different run still owns this Session.
    #[error("Session `{session}` already has running Workflow `{run}`")]
    Busy {
        /// Busy Session.
        session: SessionId,
        /// Active run identity.
        run: WorkflowRunId,
    },
    /// Runtime binding cannot provide a complete semantic fingerprint.
    #[error("Workflow runtime semantics are unavailable")]
    RuntimeUnavailable,
    /// One stable run id was retried with different immutable request data.
    #[error("Workflow run `{run}` conflicts with its original request")]
    OperationConflict {
        /// Conflicting Workflow run.
        run: WorkflowRunId,
    },
    /// Governed preflight or execution failed after catalog validation.
    #[error("Workflow execution failed: {0}")]
    Execution(String),
    /// Core persistence, projection, or runtime resolution failed.
    #[error(transparent)]
    Core(#[from] hya_core::CoreError),
}

impl WorkflowControlError {
    /// Machine-stable error code for HTTP, native, tool, and SDK adapters.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "WORKFLOW_NOT_FOUND",
            Self::InvalidSource(_) => "WORKFLOW_INVALID_SOURCE",
            Self::StaleRevision { .. } => "WORKFLOW_STALE_REVISION",
            Self::SessionNotFound { .. } => "SESSION_NOT_FOUND",
            Self::MissingSelection { .. } => "WORKFLOW_NOT_SELECTED",
            Self::InvalidInput(_) => "WORKFLOW_INVALID_INPUT",
            Self::Unauthorized(_) => "WORKFLOW_UNAUTHORIZED",
            Self::Busy { .. } => "WORKFLOW_BUSY",
            Self::RuntimeUnavailable => "WORKFLOW_RUNTIME_UNAVAILABLE",
            Self::OperationConflict { .. } => "WORKFLOW_OPERATION_CONFLICT",
            Self::Execution(_) | Self::Core(_) => "WORKFLOW_INTERNAL",
        }
    }
}
/// Invocation metadata retained while a caller crosses the Workflow control seam.
///
/// Unlike wire DTOs, this type keeps the complete [`ToolOperation`] and runtime
/// binding so actor fencing and idempotency cannot be accidentally discarded by
/// an adapter.
#[derive(Clone)]
pub struct WorkflowInvocation {
    /// Caller Agent id, when a surface has already resolved it.
    pub caller: Option<String>,
    /// Immutable Tool operation, when the request originated in a model turn.
    pub operation: Option<ToolOperation>,
    /// Captured runtime binding for Tool calls, or `None` for direct callers.
    pub binding: Option<TurnBinding>,
    /// Whether the caller waits for terminal completion.
    pub delivery: WorkflowDelivery,
}

impl Default for WorkflowInvocation {
    fn default() -> Self {
        Self {
            caller: None,
            operation: None,
            binding: None,
            delivery: WorkflowDelivery::Finished,
        }
    }
}

struct RunRequest {
    workflow: Option<String>,
    expected_revision: Option<WorkflowRevision>,
    inputs: BTreeMap<String, String>,
    requested_run: Option<WorkflowRunId>,
    cancel: CancellationToken,
}

struct ActiveWorkflowRun {
    run: WorkflowRunId,
    cancel: CancellationToken,
}

struct WorkflowAdmissionGuard {
    active: Arc<std::sync::Mutex<BTreeMap<SessionId, ActiveWorkflowRun>>>,
    session: SessionId,
    run: WorkflowRunId,
}

impl Drop for WorkflowAdmissionGuard {
    fn drop(&mut self) {
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        if active.get(&self.session).map(|active| active.run) == Some(self.run) {
            active.remove(&self.session);
        }
    }
}

/// App-owned control adapter over one engine and resident runtime owner.
#[derive(Clone)]
pub struct WorkflowControl {
    engine: Arc<SessionEngine>,
    base_agent: AgentSpec,
    resident_supervisor: Arc<ResidentSupervisor>,
    owner: OwnerRunId,
    routing: WorkflowRoutingContext,
    active_runs: Arc<std::sync::Mutex<BTreeMap<SessionId, ActiveWorkflowRun>>>,
}

impl WorkflowControl {
    /// Construct Workflow control with the process's configured route plane.
    #[must_use]
    pub(crate) fn new_with_routing(
        engine: Arc<SessionEngine>,
        base_agent: AgentSpec,
        resident_supervisor: Arc<ResidentSupervisor>,
        owner: OwnerRunId,
        categories: Arc<CategoryRegistry>,
        router: Arc<hya_provider::ProviderRouter>,
    ) -> Self {
        Self {
            engine,
            base_agent,
            resident_supervisor,
            owner,
            routing: WorkflowRoutingContext::new(categories, router),
            active_runs: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
        }
    }

    /// Execute one Workflow command for a Session.
    ///
    /// All caller surfaces use this method. It performs catalog resolution,
    /// revision and input checks, durable admission, and dispatch through the
    /// shared core runner without creating a second Workflow model.
    ///
    /// # Errors
    /// Returns a bounded, machine-coded control error when admission or
    /// persistence fails.
    pub async fn execute(
        &self,
        session: SessionId,
        invocation: WorkflowInvocation,
        command: WorkflowCommand,
        cancel: CancellationToken,
    ) -> Result<WorkflowCommandResult, WorkflowControlError> {
        match command {
            WorkflowCommand::List => {
                let (workdir, binding) = self.session_binding(session, invocation.binding).await?;
                let catalog =
                    WorkflowCatalog::build(WorkflowCatalogRoots::for_workdir(&workdir), &binding)?;
                Ok(WorkflowCommandResult::List {
                    workflows: catalog.list(),
                })
            }
            WorkflowCommand::Info { name } => {
                let (workdir, binding) = self.session_binding(session, invocation.binding).await?;
                let catalog =
                    WorkflowCatalog::build(WorkflowCatalogRoots::for_workdir(&workdir), &binding)?;
                Ok(WorkflowCommandResult::Info {
                    workflow: catalog.info(&name)?,
                })
            }
            WorkflowCommand::Select {
                name,
                expected_revision,
            } => Ok(WorkflowCommandResult::Selected {
                state: self
                    .select_inner(
                        session,
                        &name,
                        expected_revision.as_ref(),
                        invocation.operation,
                        invocation.binding,
                    )
                    .await?,
            }),
            WorkflowCommand::State => Ok(WorkflowCommandResult::State {
                state: self.state_inner(session, invocation.binding).await?,
            }),
            WorkflowCommand::Run {
                name,
                expected_revision,
                inputs,
                run,
            } => Ok(WorkflowCommandResult::Run {
                result: self
                    .run_inner(
                        session,
                        invocation,
                        RunRequest {
                            workflow: name,
                            expected_revision,
                            inputs,
                            requested_run: run,
                            cancel,
                        },
                    )
                    .await?,
            }),
        }
    }

    /// Decorate persisted Workflow state with current catalog availability.
    ///
    /// Only the derived `availability` field changes. Selection, run, and all
    /// other projection data are passed through unchanged.
    ///
    /// # Errors
    /// Returns [`WorkflowControlError::SessionNotFound`] when the Session has
    /// no persisted work directory.
    pub async fn decorate(
        &self,
        session: SessionId,
        mut state: WorkflowProjection,
    ) -> Result<WorkflowProjection, WorkflowControlError> {
        let (workdir, binding) = self.session_binding(session, None).await?;
        let catalog =
            WorkflowCatalog::build(WorkflowCatalogRoots::for_workdir(&workdir), &binding)?;
        decorate_projection(&catalog, &mut state);
        Ok(state)
    }

    /// Persist one selected identity after its optimistic revision check.
    async fn select_inner(
        &self,
        session: SessionId,
        name: &str,
        expected_revision: Option<&WorkflowRevision>,
        operation: Option<ToolOperation>,
        captured_binding: Option<TurnBinding>,
    ) -> Result<WorkflowProjection, WorkflowControlError> {
        let (workdir, binding) = self.session_binding(session, captured_binding).await?;
        let catalog =
            WorkflowCatalog::build(WorkflowCatalogRoots::for_workdir(&workdir), &binding)?;
        let entry = catalog.resolve(name)?;
        if let Some(run) = self.active_run(session)? {
            return Err(WorkflowControlError::Busy { session, run });
        }
        if let Some(expected) = expected_revision
            && *expected != entry.identity().revision
        {
            return Err(WorkflowControlError::StaleRevision {
                expected: *expected,
                actual: entry.identity().revision,
            });
        }
        let actor_claim = operation.and_then(ToolOperation::actor_claim);
        match self
            .engine
            .select_workflow(
                actor_claim.as_ref(),
                session,
                Event::WorkflowSelected {
                    session,
                    workflow: entry.identity.clone(),
                },
            )
            .await?
        {
            hya_core::DurableWorkflowSelection::Selected => {
                self.state_inner(session, Some(binding)).await
            }
            hya_core::DurableWorkflowSelection::Busy { run } => {
                Err(WorkflowControlError::Busy { session, run })
            }
        }
    }

    /// Read replay-derived Workflow state for one Session.
    async fn state_inner(
        &self,
        session: SessionId,
        captured_binding: Option<TurnBinding>,
    ) -> Result<WorkflowProjection, WorkflowControlError> {
        let projection = self.engine.read_projection(session).await?;
        if projection.session.workdir.is_none() || projection.session.id.is_none() {
            return Err(WorkflowControlError::SessionNotFound { session });
        }
        let (workdir, binding) = self.session_binding(session, captured_binding).await?;
        let catalog =
            WorkflowCatalog::build(WorkflowCatalogRoots::for_workdir(&workdir), &binding)?;
        let mut state = projection.session.workflow.unwrap_or_default();
        decorate_projection(&catalog, &mut state);
        Ok(state)
    }

    /// Resolve one Session's workdir and retain either its captured or fresh binding.
    async fn session_binding(
        &self,
        session: SessionId,
        captured_binding: Option<TurnBinding>,
    ) -> Result<(PathBuf, TurnBinding), WorkflowControlError> {
        let workdir = self.session_workdir(session).await?;
        let binding = match captured_binding {
            Some(binding) => binding,
            None => self.engine.bind_root_runtime(&workdir).await?,
        };
        Ok((workdir, binding))
    }

    /// Admit, execute, and terminalize one Workflow run.
    async fn run_inner(
        &self,
        session: SessionId,
        invocation: WorkflowInvocation,
        request: RunRequest,
    ) -> Result<WorkflowRunResult, WorkflowControlError> {
        let RunRequest {
            workflow,
            expected_revision,
            inputs,
            requested_run,
            cancel,
        } = request;
        let projection = self.engine.read_projection(session).await?;
        if projection.session.workdir.is_none() {
            return Err(WorkflowControlError::SessionNotFound { session });
        }
        let Some(caller) = projection.session.agent.as_ref().map(ToString::to_string) else {
            return Err(WorkflowControlError::SessionNotFound { session });
        };
        let selected = projection
            .session
            .workflow
            .as_ref()
            .and_then(|state| state.selection.as_ref())
            .cloned();
        let workflow_name = workflow
            .as_deref()
            .or_else(|| selected.as_ref().map(|selection| selection.name.as_str()))
            .ok_or(WorkflowControlError::MissingSelection { session })?;
        let (workdir, binding) = self
            .session_binding(session, invocation.binding.clone())
            .await?;
        let catalog =
            WorkflowCatalog::build(WorkflowCatalogRoots::for_workdir(&workdir), &binding)?;
        let entry = catalog.resolve(workflow_name)?;
        let selected_same_name = selected
            .as_ref()
            .filter(|selection| selection.name == workflow_name);
        if selected_same_name.is_some_and(|selection| selection.source != entry.identity().source) {
            return Err(WorkflowControlError::NotFound {
                name: workflow_name.to_string(),
            });
        }
        let expected_revision = expected_revision
            .as_ref()
            .or_else(|| selected_same_name.map(|selection| &selection.revision));
        if let Some(expected) = expected_revision
            && *expected != entry.identity().revision
        {
            return Err(WorkflowControlError::StaleRevision {
                expected: *expected,
                actual: entry.identity().revision,
            });
        }
        entry.workflow().validate_inputs(&inputs).map_err(|error| {
            WorkflowControlError::InvalidInput(bounded_error(&error.to_string()))
        })?;

        let runtime_fingerprint = self
            .engine
            .runtime_semantic_fingerprint_v1(&binding)
            .ok_or(WorkflowControlError::RuntimeUnavailable)?;
        let caller_name = invocation.caller.as_deref().unwrap_or(caller.as_str());
        let request_hash =
            workflow_request_hash(entry.identity(), caller_name, &inputs, runtime_fingerprint);
        let operation = invocation.operation;
        let run = match (operation, requested_run) {
            (Some(operation), _) => WorkflowRunId::from_operation(operation.operation_id()),
            (None, Some(run)) => run,
            (None, None) => WorkflowRunId::new(),
        };
        let admission_guard = self.claim_run(session, run, cancel.clone())?;
        if let Some(existing) = self.existing_run(session, run, &request_hash).await? {
            if existing.status == WorkflowRunStatus::Running {
                return Err(WorkflowControlError::Busy { session, run });
            }
            return Ok(WorkflowRunResult {
                run: existing,
                replayed: true,
            });
        }
        if let Some(active) = projection
            .session
            .workflow
            .as_ref()
            .and_then(|state| state.run.as_ref())
            && active.status == WorkflowRunStatus::Running
        {
            return Err(WorkflowControlError::Busy {
                session,
                run: active.id,
            });
        }

        let actor_claim = operation.and_then(ToolOperation::actor_claim);
        let finish_binding = binding.clone();
        let context = WorkflowRunContext {
            binding,
            caller: caller_name.to_string(),
            base_agent: self.base_agent.clone(),
            inputs,
            resident_supervisor: Some(self.resident_supervisor.clone()),
            routing: Some(self.routing.clone()),
        };
        let prepared = prepare_workflow_run_for_actor(
            self.engine.clone(),
            session,
            entry.workflow(),
            context,
            Some(run),
            actor_claim,
        )
        .await
        .map_err(map_workflow_preflight_error)?;
        let stages = workflow_stage_plan(entry.workflow(), &prepared);
        let admission = self
            .engine
            .admit_workflow_run(
                actor_claim.as_ref(),
                session,
                Event::WorkflowRunStarted {
                    session,
                    run,
                    workflow: entry.identity().clone(),
                    request_hash: request_hash.clone(),
                    owner: self.owner,
                    stages,
                },
            )
            .await?;
        match admission {
            hya_core::DurableWorkflowAdmission::Admitted => {}
            hya_core::DurableWorkflowAdmission::Existing => {
                let existing = self
                    .existing_run(session, run, &request_hash)
                    .await?
                    .ok_or_else(|| {
                        WorkflowControlError::Execution(
                            "admitted Workflow run missing from Session log".to_string(),
                        )
                    })?;
                if existing.status == WorkflowRunStatus::Running {
                    return Err(WorkflowControlError::Busy { session, run });
                }
                return Ok(WorkflowRunResult {
                    run: existing,
                    replayed: true,
                });
            }
            hya_core::DurableWorkflowAdmission::Conflict => {
                return Err(WorkflowControlError::OperationConflict { run });
            }
            hya_core::DurableWorkflowAdmission::Busy { run } => {
                return Err(WorkflowControlError::Busy { session, run });
            }
        }

        if invocation.delivery == WorkflowDelivery::Started {
            let control = self.clone();
            let background_binding = finish_binding.clone();
            tokio::spawn(async move {
                let _admission_guard = admission_guard;
                control
                    .finish_prepared_run(
                        session,
                        run,
                        operation,
                        prepared,
                        background_binding,
                        cancel,
                    )
                    .await
            });
            let current = self
                .state_inner(session, Some(finish_binding))
                .await?
                .run
                .filter(|current| current.id == run)
                .ok_or_else(|| {
                    WorkflowControlError::Execution(
                        "admitted Workflow run missing from Session projection".to_string(),
                    )
                })?;
            return Ok(WorkflowRunResult {
                run: current,
                replayed: false,
            });
        }

        self.finish_prepared_run(session, run, operation, prepared, finish_binding, cancel)
            .await
    }

    /// Execute an admitted run and append its terminal lifecycle event.
    async fn finish_prepared_run(
        &self,
        session: SessionId,
        run: WorkflowRunId,
        operation: Option<ToolOperation>,
        prepared: hya_core::PreparedWorkflowRun,
        binding: TurnBinding,
        cancel: CancellationToken,
    ) -> Result<WorkflowRunResult, WorkflowControlError> {
        let actor_claim = operation.and_then(ToolOperation::actor_claim);
        let report = match prepared.execute(cancel).await {
            Ok(report) => report,
            Err(error) => {
                let detail = bounded_error(&error.to_string());
                self.engine
                    .record_workflow_event_for_actor(
                        actor_claim.as_ref(),
                        session,
                        Event::WorkflowRunFinished {
                            session,
                            run,
                            status: WorkflowRunStatus::Failed,
                            error: Some(detail.clone()),
                        },
                    )
                    .await?;
                return Err(map_workflow_error_with_detail(error, detail));
            }
        };
        let status = match report.status {
            WorkflowStatus::Completed => WorkflowRunStatus::Completed,
            WorkflowStatus::Failed => WorkflowRunStatus::Failed,
            WorkflowStatus::Cancelled => WorkflowRunStatus::Cancelled,
        };
        self.engine
            .record_workflow_event_for_actor(
                actor_claim.as_ref(),
                session,
                Event::WorkflowRunFinished {
                    session,
                    run,
                    status,
                    error: None,
                },
            )
            .await?;
        let run_projection = self
            .state_inner(session, Some(binding))
            .await?
            .run
            .filter(|current| current.id == run)
            .ok_or_else(|| {
                WorkflowControlError::Execution(
                    "terminal Workflow run missing from Session projection".to_string(),
                )
            })?;
        Ok(WorkflowRunResult {
            run: run_projection,
            replayed: false,
        })
    }

    /// Claim this process's preflight/execution window without waiting.
    fn claim_run(
        &self,
        session: SessionId,
        run: WorkflowRunId,
        cancel: CancellationToken,
    ) -> Result<WorkflowAdmissionGuard, WorkflowControlError> {
        let mut active = self.active_runs.lock().map_err(|_| {
            WorkflowControlError::Execution("Workflow run registry is poisoned".to_string())
        })?;
        if let Some(active_run) = active.get(&session) {
            return Err(WorkflowControlError::Busy {
                session,
                run: active_run.run,
            });
        }
        active.insert(session, ActiveWorkflowRun { run, cancel });
        Ok(WorkflowAdmissionGuard {
            active: Arc::clone(&self.active_runs),
            session,
            run,
        })
    }

    /// Return the current local preflight/execution owner, when present.
    pub(crate) fn active_run(
        &self,
        session: SessionId,
    ) -> Result<Option<WorkflowRunId>, WorkflowControlError> {
        self.active_runs
            .lock()
            .map(|active| active.get(&session).map(|active| active.run))
            .map_err(|_| {
                WorkflowControlError::Execution("Workflow run registry is poisoned".to_string())
            })
    }

    /// Cancel the active Workflow execution for one Session, when present.
    pub(crate) fn cancel_run(&self, session: SessionId) -> bool {
        let cancel = self
            .active_runs
            .lock()
            .ok()
            .and_then(|active| active.get(&session).map(|active| active.cancel.clone()));
        cancel.is_some_and(|cancel| {
            cancel.cancel();
            true
        })
    }

    /// Find a prior run and reject changed immutable request data.
    async fn existing_run(
        &self,
        session: SessionId,
        run: WorkflowRunId,
        request_hash: &str,
    ) -> Result<Option<WorkflowRunProjection>, WorkflowControlError> {
        let events = self.engine.replay(session).await?;
        let Some(existing_hash) = events.iter().find_map(|envelope| match &envelope.event {
            Event::WorkflowRunStarted {
                run: existing,
                request_hash,
                ..
            } if *existing == run => Some(request_hash.as_str()),
            _ => None,
        }) else {
            return Ok(None);
        };
        if existing_hash != request_hash {
            return Err(WorkflowControlError::OperationConflict { run });
        }
        Ok(historical_run(&events, run))
    }

    /// Resolve the durable Session work directory used for discovery.
    async fn session_workdir(&self, session: SessionId) -> Result<PathBuf, WorkflowControlError> {
        let projection = self.engine.read_projection(session).await?;
        match (projection.session.id, projection.session.workdir) {
            (Some(_), Some(workdir)) => Ok(PathBuf::from(workdir)),
            _ => Err(WorkflowControlError::SessionNotFound { session }),
        }
    }
}

/// Hash immutable Workflow request data without persisting input values.
fn workflow_request_hash(
    workflow: &WorkflowIdentity,
    caller: &str,
    inputs: &BTreeMap<String, String>,
    runtime_fingerprint: [u8; 32],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"hya.workflow.request/v1\0");
    hash_field(&mut hasher, workflow.source.as_str());
    hash_field(&mut hasher, &workflow.name);
    hasher.update(workflow.revision.as_bytes());
    hash_field(&mut hasher, caller);
    hasher.update(runtime_fingerprint);
    hasher.update(
        u64::try_from(inputs.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for (key, value) in inputs {
        hash_field(&mut hasher, key);
        hash_field(&mut hasher, value);
    }
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Add one length-delimited UTF-8 field to a canonical request digest.
fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value.as_bytes());
}

/// Reconstruct one historical run from only events owned by its run id.
fn historical_run(events: &[Envelope], run: WorkflowRunId) -> Option<WorkflowRunProjection> {
    let filtered = events
        .iter()
        .filter(|envelope| workflow_event_run(&envelope.event) == Some(run))
        .cloned()
        .collect::<Vec<_>>();
    hya_proto::Projection::from_events(&filtered)
        .session
        .workflow
        .and_then(|workflow| workflow.run)
}

/// Return the Workflow run identity carried by one lifecycle event.
fn workflow_event_run(event: &Event) -> Option<WorkflowRunId> {
    match event {
        Event::WorkflowRunStarted { run, .. }
        | Event::WorkflowStageStarted { run, .. }
        | Event::WorkflowStageMemberLinked { run, .. }
        | Event::WorkflowStageRouteOutcome { run, .. }
        | Event::WorkflowStageFinished { run, .. }
        | Event::WorkflowRunFinished { run, .. } => Some(*run),
        _ => None,
    }
}

/// Convert failures detected before durable run admission to request categories.
fn map_workflow_preflight_error(error: WorkflowError) -> WorkflowControlError {
    let detail = bounded_error(&error.to_string());
    match error {
        WorkflowError::Unauthorized { .. } => WorkflowControlError::Unauthorized(detail),
        WorkflowError::Render(_) => WorkflowControlError::InvalidInput(detail),
        WorkflowError::Source { .. }
        | WorkflowError::Compile(_)
        | WorkflowError::Invalid { .. } => WorkflowControlError::InvalidSource(detail),
        WorkflowError::Engine(error) => WorkflowControlError::Core(error),
        WorkflowError::Admission(_) => WorkflowControlError::Execution(detail),
    }
}

/// Convert a core Workflow failure while retaining a bounded rendered detail.
fn map_workflow_error_with_detail(error: WorkflowError, detail: String) -> WorkflowControlError {
    match error {
        WorkflowError::Unauthorized { .. } => WorkflowControlError::Unauthorized(detail),
        WorkflowError::Render(_) => WorkflowControlError::InvalidInput(detail),
        WorkflowError::Engine(error) => WorkflowControlError::Core(error),
        WorkflowError::Source { .. }
        | WorkflowError::Compile(_)
        | WorkflowError::Invalid { .. }
        | WorkflowError::Admission(_) => WorkflowControlError::Execution(detail),
    }
}

/// Bound a persisted/public error to 2,048 UTF-8 characters.
fn bounded_error(value: &str) -> String {
    value.chars().take(2_048).collect()
}

/// Convert one immutable resolved Workflow to the shared info DTO.
fn workflow_info(entry: &ResolvedWorkflow) -> WorkflowInfo {
    let plan = entry.workflow().plan();
    WorkflowInfo {
        identity: entry.identity().clone(),
        description: entry.workflow().definition().description().to_string(),
        inputs: entry.workflow().definition().inputs().clone(),
        on_failure: entry.workflow().definition().on_failure().to_string(),
        stages: plan
            .stages()
            .iter()
            .map(|stage| WorkflowStageInfo {
                id: stage.id().to_string(),
                title: stage.title().map(str::to_string),
                agent: stage.agent().to_string(),
                level: stage.level(),
                predecessors: stage
                    .predecessor_indices()
                    .iter()
                    .map(|&index| plan.stages()[index].id().to_string())
                    .collect(),
                actor: stage.actor().map(str::to_string),
                mode: stage.mode().to_string(),
                worker_model: stage.model().map(workflow_model_assignment),
                verifier_model: stage
                    .verify()
                    .and_then(|verify| verify.model())
                    .map(workflow_model_assignment),
            })
            .collect(),
        path: entry.display_path().to_string(),
    }
}

/// Capture declaration-ordered display/provenance data and admitted selections.
fn workflow_stage_plan(
    workflow: &CompiledWorkflow,
    prepared: &hya_core::PreparedWorkflowRun,
) -> Vec<WorkflowStagePlan> {
    workflow
        .plan()
        .stages()
        .iter()
        .enumerate()
        .map(|(index, stage)| WorkflowStagePlan {
            id: stage.id().to_string(),
            title: stage.title().map(str::to_string),
            agent: hya_proto::AgentName::new(stage.agent()),
            mode: stage.mode().to_string(),
            level: stage.level(),
            worker_model: stage.model().map(workflow_model_assignment),
            selected_worker_model: prepared
                .worker_route(index)
                .and_then(workflow_selected_candidate),
            verifier_model: stage
                .verify()
                .and_then(|verify| verify.model())
                .map(workflow_model_assignment),
            selected_verifier_model: prepared
                .verifier_route(index)
                .and_then(workflow_selected_candidate),
        })
        .collect()
}

fn workflow_selected_candidate(
    route: &hya_core::WorkflowModelRoute,
) -> Option<hya_proto::WorkflowModelResolvedCandidate> {
    let selected = route.selected()?;
    Some(hya_proto::WorkflowModelResolvedCandidate {
        index: u32::try_from(route.selected_index).unwrap_or(u32::MAX),
        id: selected.model.to_string(),
        reasoning: selected.reasoning.as_str().to_string(),
    })
}

/// Convert one compiler-owned assignment into the string-based wire mirror.
fn workflow_model_assignment(
    assignment: &hya_workflow::WorkflowModelAssignment,
) -> hya_proto::WorkflowModelAssignment {
    hya_proto::WorkflowModelAssignment {
        id: assignment.id().to_string(),
        reasoning: assignment.reasoning().map(str::to_string),
        fallback: assignment
            .fallback()
            .iter()
            .map(|candidate| hya_proto::WorkflowModelCandidate {
                id: candidate.id().to_string(),
                reasoning: candidate.reasoning().map(str::to_string),
            })
            .collect(),
    }
}

/// Set only runtime availability from one immutable Workflow catalog snapshot.
fn decorate_projection(catalog: &WorkflowCatalog, state: &mut WorkflowProjection) {
    state.availability = state
        .selection
        .as_ref()
        .map(|selection| catalog.availability(selection));
}

/// Strip the complete Workflow suffix for an invalid-source discovery row.
fn source_stem(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".hya.md"))
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{WorkflowControlError, map_workflow_preflight_error, workflow_event_run};
    use hya_core::WorkflowError;

    /// Invalid runtime topology is rejected as source validation before admission.
    #[test]
    fn preflight_invalid_workflow_maps_to_invalid_source() {
        let error = map_workflow_preflight_error(WorkflowError::Invalid {
            workflow: "demo".to_string(),
            detail: "resident Agent requires a resident lifecycle".to_string(),
        });
        assert!(matches!(
            error,
            WorkflowControlError::InvalidSource(detail)
                if detail.contains("resident Agent requires a resident lifecycle")
        ));
    }

    /// Idempotent historical run reconstruction must retain route outcomes.
    #[test]
    fn historical_run_classifier_includes_route_outcomes() {
        let run = hya_proto::WorkflowRunId::new();
        let event = hya_proto::Event::WorkflowStageRouteOutcome {
            session: hya_proto::SessionId::new(),
            run,
            stage: "execute".to_string(),
            member: hya_proto::MemberId::new(),
            role: hya_proto::WorkflowMemberRole::Worker,
            iteration: 0,
            step: 0,
            candidate_index: 0,
            model: hya_proto::ModelRef::new("fake/model"),
            reasoning: "none".to_string(),
            failure_class: hya_proto::WorkflowRouteFailureClass::None,
        };

        assert_eq!(workflow_event_run(&event), Some(run));
    }
}
