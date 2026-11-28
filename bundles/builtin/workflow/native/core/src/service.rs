//! Managed [`NativeService`] owning durable workflow selection, admission,
//! execution, cancellation, and restart recovery over [`HostRuntime`].

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use hya_proto::{AgentName, OwnerRunId, SessionId};
use hya_runtime_abi::{HostError, HostRuntime, NativeService, ResolvedModelRoute, ServiceHealth};
use hya_tool::ToolOperation;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::catalog::{WorkflowCatalog, WorkflowCatalogRoots};
use crate::compile::CompiledWorkflow;
use crate::dag::{PreparedWorkflowRun, WorkflowStatus, prepare_workflow_run};
use crate::error::{
    WorkflowControlError, bounded_error, map_workflow_error_with_detail,
    map_workflow_preflight_error,
};
use crate::events::WorkflowEvent;
use crate::model::{
    WorkflowCommand, WorkflowCommandResult, WorkflowDelivery, WorkflowIdentity,
    WorkflowModelResolvedCandidate, WorkflowProjection, WorkflowRevision, WorkflowRunId,
    WorkflowRunProjection, WorkflowRunResult, WorkflowRunStatus, WorkflowStagePlan,
    WorkflowSummary,
};
use crate::reducer::{decode_workflow_projection, extension_event_parts, historical_run};

/// Canonical bundle identity for `hya/workflow`.
pub const WORKFLOW_BUNDLE_ID: &str = "hya/workflow";

/// Service identifier registered by `hya/workflow`.
pub const WORKFLOW_SERVICE_ID: &str = "workflow-supervisor";

/// Invocation metadata retained when executing a workflow command.
#[derive(Clone, Debug)]
pub struct WorkflowInvocation {
    /// Optional caller Agent id override.
    pub caller: Option<String>,
    /// Optional Tool operation for idempotent run id & actor claim fencing.
    pub operation: Option<ToolOperation>,
    /// Whether `run` waits for terminal completion (`Finished`) or returns
    /// immediately after durable admission (`Started`).
    pub delivery: WorkflowDelivery,
}

impl Default for WorkflowInvocation {
    fn default() -> Self {
        Self {
            caller: None,
            operation: None,
            delivery: WorkflowDelivery::Finished,
        }
    }
}

struct ActiveWorkflowRun {
    run: WorkflowRunId,
    cancel: CancellationToken,
    done: CancellationToken,
}

struct WorkflowAdmissionGuard {
    active: Arc<Mutex<BTreeMap<SessionId, ActiveWorkflowRun>>>,
    session: SessionId,
    run: WorkflowRunId,
    done: CancellationToken,
}

impl Drop for WorkflowAdmissionGuard {
    fn drop(&mut self) {
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        if active.get(&self.session).map(|entry| entry.run) == Some(self.run) {
            active.remove(&self.session);
        }
        self.done.cancel();
    }
}

/// Long-lived workflow control and execution service owned by `hya/workflow`.
pub struct WorkflowService {
    runtime: RwLock<Option<Arc<dyn HostRuntime>>>,
    active_runs: Arc<Mutex<BTreeMap<SessionId, ActiveWorkflowRun>>>,
    session_locks: Mutex<BTreeMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
    tasks: TaskTracker,
    stop: CancellationToken,
}

impl Default for WorkflowService {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkflowService {
    /// Construct a new stopped [`WorkflowService`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtime: RwLock::new(None),
            active_runs: Arc::new(Mutex::new(BTreeMap::new())),
            session_locks: Mutex::new(BTreeMap::new()),
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
        }
    }

    /// Return `true` if [`NativeService::start`] has attached a [`HostRuntime`].
    #[must_use]
    pub fn is_started(&self) -> bool {
        self.runtime
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|_| true))
            .unwrap_or(false)
    }

    fn runtime(&self) -> Result<Arc<dyn HostRuntime>, WorkflowControlError> {
        if self.stop.is_cancelled() {
            return Err(WorkflowControlError::RuntimeUnavailable);
        }
        self.runtime
            .read()
            .map_err(|_| WorkflowControlError::RuntimeUnavailable)?
            .clone()
            .ok_or(WorkflowControlError::RuntimeUnavailable)
    }

    fn session_lock(&self, session: SessionId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .session_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            locks
                .entry(session)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// List discovered workflows for an optional directory scope.
    ///
    /// # Errors
    /// Returns [`WorkflowControlError`] on runtime unavailability or invalid prepared bundle workflow.
    pub async fn list(
        &self,
        directory: Option<&Path>,
    ) -> Result<Vec<WorkflowSummary>, WorkflowControlError> {
        let runtime = self.runtime()?;
        let scope = runtime
            .catalog_reader()
            .resolve_scope_catalog(None, directory)
            .await?;
        let dir = if scope.workdir.as_os_str().is_empty() {
            directory
        } else {
            Some(scope.workdir.as_path())
        };
        let roots = WorkflowCatalogRoots::for_scope(dir, &scope.project_roots);
        let catalog = WorkflowCatalog::from_bundles(roots, &scope.bundles)?;
        Ok(catalog.summaries())
    }

    /// The scope `hya workflow list|info` show for the client's `cwd`.
    pub(crate) async fn cli_scope(
        &self,
        cwd: &Path,
    ) -> Result<hya_runtime_abi::ScopeCatalogSnapshot, WorkflowControlError> {
        Ok(self
            .runtime()?
            .catalog_reader()
            .resolve_scope_catalog(None, Some(cwd))
            .await?)
    }

    /// Execute one typed [`WorkflowCommand`] against `session`.
    ///
    /// # Errors
    /// Returns a machine-coded [`WorkflowControlError`] on validation,
    /// authorization, revision, busy, or runtime failures.
    pub async fn execute(
        self: &Arc<Self>,
        session: SessionId,
        invocation: WorkflowInvocation,
        command: WorkflowCommand,
        cancel: CancellationToken,
    ) -> Result<WorkflowCommandResult, WorkflowControlError> {
        let _token = self.tasks.token();
        if self.stop.is_cancelled() {
            return Err(WorkflowControlError::RuntimeUnavailable);
        }
        let execution = async {
            match command {
                WorkflowCommand::List => {
                    let catalog = self.session_catalog(session).await?;
                    Ok(WorkflowCommandResult::List {
                        workflows: catalog.summaries(),
                    })
                }
                WorkflowCommand::Info { name } => {
                    let catalog = self.session_catalog(session).await?;
                    Ok(WorkflowCommandResult::Info {
                        workflow: catalog.describe(&name)?,
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
                        )
                        .await?,
                }),
                WorkflowCommand::State => Ok(WorkflowCommandResult::State {
                    state: self.state_inner(session).await?,
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
                            name,
                            expected_revision,
                            inputs,
                            run,
                            cancel.clone(),
                        )
                        .await?,
                }),
            }
        };
        tokio::pin!(execution);
        tokio::select! {
            biased;
            () = self.stop.cancelled() => {
                cancel.cancel();
                execution.await
            }
            result = &mut execution => result,
        }
    }

    /// [`Self::execute`] in this service's task set, so the command keeps
    /// running when the caller stops waiting for it.
    ///
    /// # Errors
    /// Returns the command's [`WorkflowControlError`], or
    /// [`WorkflowControlError::Execution`] when its task panicked.
    pub async fn execute_detached(
        self: &Arc<Self>,
        session: SessionId,
        invocation: WorkflowInvocation,
        command: WorkflowCommand,
    ) -> Result<WorkflowCommandResult, WorkflowControlError> {
        let service = Arc::clone(self);
        self.tasks
            .spawn(async move {
                service
                    .execute(session, invocation, command, CancellationToken::new())
                    .await
            })
            .await
            .map_err(|error| WorkflowControlError::Execution(format!("Workflow task: {error}")))?
    }

    async fn session_catalog(
        &self,
        session: SessionId,
    ) -> Result<WorkflowCatalog, WorkflowControlError> {
        let runtime = self.runtime()?;
        let (scope, _) = self.resolve_session_scope(&runtime, session).await?;
        let roots =
            WorkflowCatalogRoots::for_scope(Some(scope.workdir.as_path()), &scope.project_roots);
        WorkflowCatalog::from_bundles(roots, &scope.bundles)
    }

    async fn resolve_session_scope(
        &self,
        runtime: &Arc<dyn HostRuntime>,
        session: SessionId,
    ) -> Result<(hya_runtime_abi::ScopeCatalogSnapshot, AgentName), WorkflowControlError> {
        let projection = runtime.session_store().read_projection(session).await?;
        if projection.session.id.is_none() || projection.session.workdir.is_none() {
            return Err(WorkflowControlError::SessionNotFound { session });
        }
        let caller = projection
            .session
            .agent
            .clone()
            .ok_or(WorkflowControlError::SessionNotFound { session })?;
        let workdir = projection
            .session
            .workdir
            .as_deref()
            .map(Path::new)
            .ok_or(WorkflowControlError::SessionNotFound { session })?;
        let scope = runtime
            .catalog_reader()
            .resolve_scope_catalog(Some(session), Some(workdir))
            .await?;
        Ok((scope, caller))
    }

    async fn state_inner(
        &self,
        session: SessionId,
    ) -> Result<WorkflowProjection, WorkflowControlError> {
        let runtime = self.runtime()?;
        let projection = runtime.session_store().read_projection(session).await?;
        if projection.session.id.is_none() || projection.session.workdir.is_none() {
            return Err(WorkflowControlError::SessionNotFound { session });
        }
        let workdir = projection
            .session
            .workdir
            .as_deref()
            .map(Path::new)
            .ok_or(WorkflowControlError::SessionNotFound { session })?;
        let scope = runtime
            .catalog_reader()
            .resolve_scope_catalog(Some(session), Some(workdir))
            .await?;
        let roots =
            WorkflowCatalogRoots::for_scope(Some(scope.workdir.as_path()), &scope.project_roots);
        let catalog = WorkflowCatalog::from_bundles(roots, &scope.bundles)?;
        let mut state =
            decode_workflow_projection(projection.extensions.get(WORKFLOW_BUNDLE_ID).cloned());
        decorate_projection(&catalog, &mut state);
        Ok(state)
    }

    async fn select_inner(
        &self,
        session: SessionId,
        name: &str,
        expected_revision: Option<&WorkflowRevision>,
        operation: Option<ToolOperation>,
    ) -> Result<WorkflowProjection, WorkflowControlError> {
        let runtime = self.runtime()?;
        let lock = self.session_lock(session);
        let _guard = lock.lock().await;

        let catalog = self.session_catalog(session).await?;
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

        // Check durable active run before mutating selection.
        let current = decode_workflow_projection(
            runtime
                .session_store()
                .read_extension_state(session, WORKFLOW_BUNDLE_ID)
                .await?,
        );
        if let Some(active) = current
            .run
            .filter(|r| r.status == WorkflowRunStatus::Running)
        {
            return Err(WorkflowControlError::Busy {
                session,
                run: active.id,
            });
        }

        let actor_claim = operation.and_then(ToolOperation::actor_claim);
        let (target_session, bundle, kind, payload) =
            extension_event_parts(WorkflowEvent::WorkflowSelected {
                session,
                workflow: entry.identity().clone(),
            })?;
        runtime
            .session_store()
            .append_extension_event(target_session, &bundle, &kind, payload, actor_claim)
            .await?;
        self.state_inner(session).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_inner(
        self: &Arc<Self>,
        session: SessionId,
        invocation: WorkflowInvocation,
        workflow: Option<String>,
        expected_revision: Option<WorkflowRevision>,
        inputs: BTreeMap<String, String>,
        requested_run: Option<WorkflowRunId>,
        cancel: CancellationToken,
    ) -> Result<WorkflowRunResult, WorkflowControlError> {
        let runtime = self.runtime()?;
        let projection = runtime.session_store().read_projection(session).await?;
        if projection.session.id.is_none() || projection.session.workdir.is_none() {
            return Err(WorkflowControlError::SessionNotFound { session });
        }
        let Some(session_caller) = projection.session.agent.clone() else {
            return Err(WorkflowControlError::SessionNotFound { session });
        };
        let workflow_proj =
            decode_workflow_projection(projection.extensions.get(WORKFLOW_BUNDLE_ID).cloned());
        let selected = workflow_proj.selection.clone();
        let active_run = workflow_proj
            .run
            .as_ref()
            .filter(|run| run.status == WorkflowRunStatus::Running)
            .map(|run| run.id);
        let workdir = projection
            .session
            .workdir
            .as_deref()
            .map(Path::new)
            .ok_or(WorkflowControlError::SessionNotFound { session })?;
        let scope = runtime
            .catalog_reader()
            .resolve_scope_catalog(Some(session), Some(workdir))
            .await?;
        drop(projection);

        let workflow_name = workflow
            .as_deref()
            .or_else(|| selected.as_ref().map(|selection| selection.name.as_str()))
            .ok_or(WorkflowControlError::MissingSelection { session })?;
        let roots =
            WorkflowCatalogRoots::for_scope(Some(scope.workdir.as_path()), &scope.project_roots);
        let catalog = WorkflowCatalog::from_bundles(roots, &scope.bundles)?;
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

        if scope.runtime_fingerprint.is_empty() {
            return Err(WorkflowControlError::RuntimeUnavailable);
        }
        let caller = invocation
            .caller
            .as_deref()
            .map(AgentName::new)
            .unwrap_or(session_caller);
        let request_hash = workflow_request_hash(
            entry.identity(),
            caller.as_str(),
            &inputs,
            &scope.runtime_fingerprint,
        );

        let operation = invocation.operation;
        let run = match (operation, requested_run) {
            (Some(operation), _) => WorkflowRunId::from_operation(operation.operation_id()),
            (None, Some(run)) => run,
            (None, None) => WorkflowRunId::new(),
        };

        let admission_guard = self.claim_run(session, run, cancel.clone())?;
        if let Some(existing) = self
            .existing_run(&runtime, session, run, &request_hash)
            .await?
        {
            if existing.status == WorkflowRunStatus::Running {
                return Err(WorkflowControlError::Busy { session, run });
            }
            return Ok(WorkflowRunResult {
                run: existing,
                replayed: true,
            });
        }
        if let Some(active) = active_run {
            return Err(WorkflowControlError::Busy {
                session,
                run: active,
            });
        }

        let actor_claim = operation.and_then(ToolOperation::actor_claim);
        let prepared = prepare_workflow_run(
            Arc::clone(&runtime),
            session,
            caller,
            entry.workflow(),
            inputs,
            Some(run),
            actor_claim,
        )
        .await
        .map_err(map_workflow_preflight_error)?;

        let stages = workflow_stage_plan(entry.workflow(), &prepared);
        let owner = runtime
            .session_store()
            .runtime_owner_id()
            .unwrap_or_else(OwnerRunId::new);

        // Atomic admission check + durable WorkflowRunStarted append under per-session lock.
        {
            let lock = self.session_lock(session);
            let _guard = lock.lock().await;
            if let Some(existing) = self
                .existing_run(&runtime, session, run, &request_hash)
                .await?
            {
                if existing.status == WorkflowRunStatus::Running {
                    return Err(WorkflowControlError::Busy { session, run });
                }
                return Ok(WorkflowRunResult {
                    run: existing,
                    replayed: true,
                });
            }
            let current = decode_workflow_projection(
                runtime
                    .session_store()
                    .read_extension_state(session, WORKFLOW_BUNDLE_ID)
                    .await?,
            );
            if let Some(active) = current
                .run
                .filter(|r| r.status == WorkflowRunStatus::Running)
            {
                return Err(WorkflowControlError::Busy {
                    session,
                    run: active.id,
                });
            }
            let (target_session, bundle, kind, payload) =
                extension_event_parts(WorkflowEvent::WorkflowRunStarted {
                    session,
                    run,
                    workflow: entry.identity().clone(),
                    request_hash: request_hash.clone(),
                    owner,
                    stages,
                })?;
            runtime
                .session_store()
                .append_extension_event(target_session, &bundle, &kind, payload, actor_claim)
                .await?;
        }

        if invocation.delivery == WorkflowDelivery::Started {
            let service = Arc::clone(self);
            self.tasks.spawn(async move {
                let _admission_guard = admission_guard;
                let execution =
                    service.finish_prepared_run(&runtime, session, run, prepared, cancel.clone());
                tokio::pin!(execution);
                tokio::select! {
                    biased;
                    () = service.stop.cancelled() => {
                        cancel.cancel();
                        let _ = execution.await;
                    }
                    _ = &mut execution => {}
                }
            });
            let current = self
                .state_inner(session)
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

        let result = self
            .finish_prepared_run(&runtime, session, run, prepared, cancel)
            .await;
        drop(admission_guard);
        result
    }

    async fn finish_prepared_run(
        &self,
        runtime: &Arc<dyn HostRuntime>,
        session: SessionId,
        run: WorkflowRunId,
        prepared: PreparedWorkflowRun,
        cancel: CancellationToken,
    ) -> Result<WorkflowRunResult, WorkflowControlError> {
        let _activity = runtime.turn_runner().begin_root_activity(session).await?;
        let report = match prepared.execute(cancel.clone()).await {
            Ok(report) => report,
            Err(error) => {
                let detail = bounded_error(&error.to_string());
                let (target_session, bundle, kind, payload) =
                    extension_event_parts(WorkflowEvent::WorkflowRunFinished {
                        session,
                        run,
                        status: if cancel.is_cancelled() {
                            WorkflowRunStatus::Cancelled
                        } else {
                            WorkflowRunStatus::Failed
                        },
                        error: Some(detail.clone()),
                    })?;
                runtime
                    .session_store()
                    .append_extension_event(target_session, &bundle, &kind, payload, None)
                    .await?;
                return Err(map_workflow_error_with_detail(error, detail));
            }
        };
        let status = match report.status {
            WorkflowStatus::Completed => WorkflowRunStatus::Completed,
            WorkflowStatus::Failed => WorkflowRunStatus::Failed,
            WorkflowStatus::Cancelled => WorkflowRunStatus::Cancelled,
        };
        let (target_session, bundle, kind, payload) =
            extension_event_parts(WorkflowEvent::WorkflowRunFinished {
                session,
                run,
                status,
                error: None,
            })?;
        runtime
            .session_store()
            .append_extension_event(target_session, &bundle, &kind, payload, None)
            .await?;
        let run_projection = self
            .state_inner(session)
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

    fn claim_run(
        &self,
        session: SessionId,
        run: WorkflowRunId,
        cancel: CancellationToken,
    ) -> Result<WorkflowAdmissionGuard, WorkflowControlError> {
        let mut active = self.active_runs.lock().map_err(|_| {
            WorkflowControlError::Execution("Workflow run registry is poisoned".to_string())
        })?;
        if let Some(existing) = active.get(&session) {
            return Err(WorkflowControlError::Busy {
                session,
                run: existing.run,
            });
        }
        let done = CancellationToken::new();
        active.insert(
            session,
            ActiveWorkflowRun {
                run,
                cancel,
                done: done.clone(),
            },
        );
        Ok(WorkflowAdmissionGuard {
            active: Arc::clone(&self.active_runs),
            session,
            run,
            done,
        })
    }

    fn active_run(
        &self,
        session: SessionId,
    ) -> Result<Option<WorkflowRunId>, WorkflowControlError> {
        self.active_runs
            .lock()
            .map(|active| active.get(&session).map(|entry| entry.run))
            .map_err(|_| {
                WorkflowControlError::Execution("Workflow run registry is poisoned".to_string())
            })
    }

    async fn existing_run(
        &self,
        runtime: &Arc<dyn HostRuntime>,
        session: SessionId,
        run: WorkflowRunId,
        request_hash: &str,
    ) -> Result<Option<WorkflowRunProjection>, WorkflowControlError> {
        let events = runtime
            .session_store()
            .read_events(session, None, None)
            .await?;
        let Some(existing_hash) = events.iter().find_map(
            |envelope| match WorkflowEvent::from_event(&envelope.event) {
                Some(WorkflowEvent::WorkflowRunStarted {
                    run: existing,
                    request_hash,
                    ..
                }) if existing == run => Some(request_hash),
                _ => None,
            },
        ) else {
            return Ok(None);
        };
        if existing_hash != request_hash {
            return Err(WorkflowControlError::OperationConflict { run });
        }
        Ok(historical_run(&events, run))
    }

    async fn recover_nonterminal_workflows(
        runtime: &Arc<dyn HostRuntime>,
    ) -> Result<(), HostError> {
        let Some(current_owner) = runtime.session_store().runtime_owner_id() else {
            return Ok(());
        };
        let sessions = runtime
            .session_store()
            .find_sessions_with_extension(WORKFLOW_BUNDLE_ID, "workflow_run_started")
            .await?;
        for session in sessions {
            let state = decode_workflow_projection(
                runtime
                    .session_store()
                    .read_extension_state(session, WORKFLOW_BUNDLE_ID)
                    .await?,
            );
            let Some(run) = state.run else {
                continue;
            };
            if run.status != WorkflowRunStatus::Running || run.owner == current_owner {
                continue;
            }
            let (target_session, bundle, kind, payload) =
                extension_event_parts(WorkflowEvent::WorkflowRunFinished {
                    session,
                    run: run.id,
                    status: WorkflowRunStatus::Interrupted,
                    error: Some("backend startup recovery".to_string()),
                })?;
            runtime
                .session_store()
                .append_extension_event(target_session, &bundle, &kind, payload, None)
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl NativeService for WorkflowService {
    fn id(&self) -> &str {
        WORKFLOW_SERVICE_ID
    }

    async fn start(&self, runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        {
            let mut slot = self.runtime.write().map_err(|_| {
                HostError::Internal("workflow service runtime lock poisoned".into())
            })?;
            *slot = Some(Arc::clone(&runtime));
        }
        Self::recover_nonterminal_workflows(&runtime).await?;
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        self.stop.cancel();
        let active_cancels: Vec<CancellationToken> = self
            .active_runs
            .lock()
            .ok()
            .map(|active| active.values().map(|entry| entry.cancel.clone()).collect())
            .unwrap_or_default();
        for cancel in active_cancels {
            cancel.cancel();
        }
        self.tasks.close();
        self.tasks.wait().await;
        if let Ok(mut slot) = self.runtime.write() {
            *slot = None;
        }
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        if self.is_started() && !self.stop.is_cancelled() {
            ServiceHealth::healthy("ready")
        } else {
            ServiceHealth::unhealthy("stopped")
        }
    }

    fn is_session_busy(&self, session: SessionId) -> bool {
        self.active_runs
            .lock()
            .ok()
            .is_some_and(|active| active.contains_key(&session))
    }

    fn cancel_session(&self, session: SessionId) -> bool {
        let cancel = self
            .active_runs
            .lock()
            .ok()
            .and_then(|active| active.get(&session).map(|entry| entry.cancel.clone()));
        cancel.is_some_and(|cancel| {
            cancel.cancel();
            true
        })
    }

    async fn wait_session_idle(&self, session: SessionId) -> Result<(), HostError> {
        let done = self
            .active_runs
            .lock()
            .map_err(|_| HostError::Internal("workflow active_runs lock poisoned".into()))?
            .get(&session)
            .map(|entry| entry.done.clone());
        if let Some(done) = done {
            done.cancelled().await;
        }
        Ok(())
    }
}

/// Hash immutable Workflow request data without persisting input values.
#[must_use]
pub fn workflow_request_hash(
    workflow: &WorkflowIdentity,
    caller: &str,
    inputs: &BTreeMap<String, String>,
    runtime_fingerprint: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"hya.workflow.request/v2\0");
    hash_field(&mut hasher, workflow.source.as_str());
    hash_field(&mut hasher, &workflow.name);
    hasher.update(workflow.revision.as_bytes());
    hash_field(&mut hasher, caller);
    hash_field(&mut hasher, runtime_fingerprint);
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

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn workflow_stage_plan(
    workflow: &CompiledWorkflow,
    prepared: &PreparedWorkflowRun,
) -> Vec<WorkflowStagePlan> {
    workflow
        .plan()
        .stages()
        .iter()
        .enumerate()
        .map(|(index, stage)| WorkflowStagePlan {
            id: stage.id().to_string(),
            title: stage.title().map(str::to_string),
            agent: AgentName::new(stage.agent()),
            mode: stage.mode().to_string(),
            level: stage.level(),
            worker_model: stage.model().cloned(),
            selected_worker_model: prepared
                .worker_route(index)
                .and_then(workflow_selected_candidate),
            verifier_model: stage.verify().and_then(|verify| verify.model()).cloned(),
            selected_verifier_model: prepared
                .verifier_route(index)
                .and_then(workflow_selected_candidate),
        })
        .collect()
}

fn workflow_selected_candidate(
    route: &ResolvedModelRoute,
) -> Option<WorkflowModelResolvedCandidate> {
    let selected = route.selected()?;
    Some(WorkflowModelResolvedCandidate {
        index: u32::try_from(route.selected_index).unwrap_or(u32::MAX),
        id: selected.model.to_string(),
        reasoning: selected.reasoning.clone(),
    })
}

fn decorate_projection(catalog: &WorkflowCatalog, state: &mut WorkflowProjection) {
    state.availability = state
        .selection
        .as_ref()
        .map(|selection| catalog.availability(selection));
}
