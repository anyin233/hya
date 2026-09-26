//! Shared HTTP application state for native and Compat routes.

use std::sync::Arc;

use hya_core::{AgentSpec, SessionEngine};
use hya_proto::WorkspaceAdapterInfo;
use hya_tool::{AskRequest, FormatterStatus, QuestionRequest};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};

use crate::agent_model_control::{AgentModelControl, EmptyAgentModelControl};
use crate::mcp_control::{EmptyMcpControl, McpControl};
use crate::provider_control::{EmptyProviderControl, ProviderControl};
use crate::session_list::SessionListHub;
use crate::streams::StreamShutdown;
use crate::support;
use crate::workflow_control::{EmptyWorkflowControl, WorkflowControl};
use crate::{pending, runs};

/// Holds the session engine, process agent base, permission/question queues,
/// MCP and Workflow control handles, workspace adapters, and formatter status.
/// The router wraps this into internal `ServerState` (Compat process-local
/// state). The run registry and the session-list hub live here, so every
/// router and gRPC binding built from one `AppState` shares them.
#[derive(Clone)]
pub struct AppState {
    /// Shared session engine for all routes.
    pub engine: Arc<SessionEngine>,
    /// Process-level agent base used by native turns.
    pub agent: Arc<AgentSpec>,
    permission_requests: pending::PermissionRequests,
    question_requests: pending::QuestionRequests,
    mcp_control: Arc<dyn McpControl>,
    agent_model_control: Arc<dyn AgentModelControl>,
    provider_control: Arc<dyn ProviderControl>,
    workflow_control: Arc<dyn WorkflowControl>,
    workspace_adapters: Vec<WorkspaceAdapterInfo>,
    formatter_status: Vec<FormatterStatus>,
    default_agent: Option<String>,
    catalog_updates: broadcast::Sender<Value>,
    streams: StreamShutdown,
    pure_guidance: bool,
    auto_title: bool,
    runs: runs::RunRegistry,
    session_list: SessionListHub,
}

impl AppState {
    /// Create state with empty pending queues and no-op MCP, Workflow, and Agent-model controls.
    #[must_use]
    pub fn new(engine: Arc<SessionEngine>, agent: Arc<AgentSpec>) -> Self {
        let permission_requests = pending::PermissionRequests::new(engine.store().clone());
        let (catalog_updates, _) = broadcast::channel(16);
        Self {
            engine,
            agent,
            permission_requests,
            question_requests: pending::QuestionRequests::default(),
            mcp_control: Arc::new(EmptyMcpControl),
            agent_model_control: Arc::new(EmptyAgentModelControl),
            provider_control: Arc::new(EmptyProviderControl),
            workflow_control: Arc::new(EmptyWorkflowControl),
            workspace_adapters: Vec::new(),
            formatter_status: Vec::new(),
            default_agent: None,
            catalog_updates,
            streams: StreamShutdown::default(),
            pure_guidance: false,
            auto_title: false,
            runs: runs::RunRegistry::default(),
            session_list: SessionListHub::default(),
        }
    }

    /// Title root sessions automatically: the first prompt turn of a root
    /// session without a title starts one background call to the fixed
    /// `title` agent (see `SessionEngine::auto_title_session`). Off by
    /// default so embedders and tests with scripted providers opt in; the
    /// `hya` server and TUI backends turn it on.
    #[must_use]
    pub fn with_auto_title(mut self, enabled: bool) -> Self {
        self.auto_title = enabled;
        self
    }

    /// Whether prompt turns title their root session automatically.
    #[must_use]
    pub fn auto_title(&self) -> bool {
        self.auto_title
    }

    /// `--pure`: per-turn guidance skips AGENTS/context discovery entirely.
    #[must_use]
    pub fn with_pure_guidance(mut self, pure: bool) -> Self {
        self.pure_guidance = pure;
        self
    }

    /// Set the agent selected by default when a workdir does not configure one.
    #[must_use]
    pub fn with_default_agent(mut self, agent: Option<String>) -> Self {
        self.default_agent = agent;
        self
    }

    /// Attach the permission-ask receiver and start the pending-request bridge.
    #[must_use]
    pub fn with_permission_requests(mut self, rx: mpsc::UnboundedReceiver<AskRequest>) -> Self {
        self.permission_requests =
            pending::PermissionRequests::spawn(rx, self.engine.store().clone());
        self
    }

    /// Reload saved "allow always" grants from the store into the engine's
    /// process permission plane, so remembered approvals survive a restart.
    /// Call once at startup before serving; returns how many grants were
    /// restored.
    ///
    /// # Errors
    /// Returns the store error when the saved rows cannot be read.
    pub async fn restore_saved_permissions(&self) -> Result<usize, hya_store::StoreError> {
        self.permission_requests
            .restore_saved(self.engine.permission_plane())
            .await
    }

    /// Attach the user-question receiver and start the pending-question bridge.
    #[must_use]
    pub fn with_question_requests(mut self, rx: mpsc::UnboundedReceiver<QuestionRequest>) -> Self {
        self.question_requests = pending::QuestionRequests::spawn(rx);
        self
    }

    /// Install the app-owned MCP control handle for Compat MCP routes.
    #[must_use]
    pub fn with_mcp_control(mut self, control: Arc<dyn McpControl>) -> Self {
        self.mcp_control = control;
        self
    }

    /// Install the app-owned Agent model preference control for TUI routes.
    #[must_use]
    pub fn with_agent_model_control(mut self, control: Arc<dyn AgentModelControl>) -> Self {
        self.agent_model_control = control;
        self
    }

    /// Install the app-owned provider control (keys, provider upsert, model
    /// refresh, config model overrides) for the provider routes.
    #[must_use]
    pub fn with_provider_control(mut self, control: Arc<dyn ProviderControl>) -> Self {
        self.provider_control = control;
        self
    }

    /// Install the app-owned Workflow control handle for native and Compat routes.
    #[must_use]
    pub fn with_workflow_control(mut self, control: Arc<dyn WorkflowControl>) -> Self {
        self.workflow_control = control;
        self
    }

    /// Register plugin workspace adapters for experimental workspace routes.
    #[must_use]
    pub fn with_workspace_adapters(mut self, adapters: Vec<WorkspaceAdapterInfo>) -> Self {
        self.workspace_adapters = adapters;
        self
    }

    /// Publish formatter status rows for Compat formatter endpoints.
    #[must_use]
    pub fn with_formatter_status(mut self, status: Vec<FormatterStatus>) -> Self {
        self.formatter_status = status;
        self
    }

    /// Publish a provider-catalog change: every v1 event stream (global and
    /// session, SSE and gRPC) delivers it as a live `catalogUpdated` frame.
    pub fn notify_catalog_updated(&self) {
        let payload = serde_json::json!({
            "id": format!(
                "catalog-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis())
                    .unwrap_or(0)
            ),
            "type": "catalog.updated",
            "properties": {}
        });
        let _ = self.catalog_updates.send(payload);
    }

    /// Subscribe to provider-catalog refresh notifications.
    #[must_use]
    pub fn subscribe_catalog_updates(&self) -> broadcast::Receiver<Value> {
        self.catalog_updates.subscribe()
    }

    /// The live event streams' shutdown signal. Every clone of this state,
    /// and every router or gRPC binding built from it, shares it: call
    /// [`StreamShutdown::close`] when the server starts shutting down so open
    /// streams end instead of holding the graceful shutdown open.
    #[must_use]
    pub fn streams(&self) -> StreamShutdown {
        self.streams.clone()
    }

    /// Clone the catalog-update publisher for background refresh tasks.
    #[must_use]
    pub fn catalog_updates_sender(&self) -> broadcast::Sender<Value> {
        self.catalog_updates.clone()
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct ServerState {
    pub(crate) engine: Arc<SessionEngine>,
    pub(crate) agent: Arc<AgentSpec>,
    pub(crate) runs: runs::RunRegistry,
    pub(crate) permission_requests: pending::PermissionRequests,
    pub(crate) question_requests: pending::QuestionRequests,
    pub(crate) global: support::global_state::GlobalState,
    pub(crate) mcp_control: Arc<dyn McpControl>,
    pub(crate) agent_model_control: Arc<dyn AgentModelControl>,
    pub(crate) provider_control: Arc<dyn ProviderControl>,
    pub(crate) workflow_control: Arc<dyn WorkflowControl>,
    pub(crate) pty: support::pty_state::PtyState,
    pub(crate) workspace_adapters: Vec<WorkspaceAdapterInfo>,
    pub(crate) formatter_status: Vec<FormatterStatus>,
    pub(crate) default_agent: Option<String>,
    pub(crate) catalog_updates: broadcast::Sender<Value>,
    pub(crate) streams: StreamShutdown,
    pub(crate) pure_guidance: bool,
    pub(crate) auto_title: bool,
    pub(crate) session_list: SessionListHub,
}

impl ServerState {
    pub(crate) fn new(app: AppState) -> Self {
        let global = support::global_state::GlobalState::new(app.engine.lsp().is_configured());
        Self {
            engine: app.engine,
            agent: app.agent,
            runs: app.runs,
            permission_requests: app.permission_requests,
            question_requests: app.question_requests,
            global,
            mcp_control: app.mcp_control,
            agent_model_control: app.agent_model_control,
            provider_control: app.provider_control,
            workflow_control: app.workflow_control,
            pty: support::pty_state::PtyState::new(),
            workspace_adapters: app.workspace_adapters,
            formatter_status: app.formatter_status,
            default_agent: app.default_agent,
            catalog_updates: app.catalog_updates,
            streams: app.streams,
            pure_guidance: app.pure_guidance,
            auto_title: app.auto_title,
            session_list: app.session_list,
        }
    }

    /// Start a parent-model run only when no Workflow owns the Session.
    pub(crate) fn start_run(&self, session: hya_proto::SessionId) -> Option<runs::RunGuard> {
        self.reserve_run(session)
    }

    /// Reserve the Session for a mutating Workflow command.
    ///
    /// Parent-model and Workflow admissions use the same process-local
    /// registry. The reservation remains held by the caller while it crosses
    /// the Workflow control port, so a parent turn cannot slip in before the
    /// app-owned Workflow claim becomes visible.
    pub(crate) fn reserve_workflow_run(
        &self,
        session: hya_proto::SessionId,
    ) -> Option<runs::RunGuard> {
        self.reserve_run(session)
    }

    fn reserve_run(&self, session: hya_proto::SessionId) -> Option<runs::RunGuard> {
        if self.workflow_control.active_run(session).is_some() {
            return None;
        }
        self.runs.start(session)
    }

    /// Return whether the Session has any active turn: a server run, a
    /// Workflow run, or an engine turn the server did not start (a resident
    /// wake, a deferred quiescence-synthesis turn).
    pub(crate) fn is_busy(&self, session: hya_proto::SessionId) -> bool {
        self.runs.is_busy(session)
            || self.workflow_control.active_run(session).is_some()
            || self.engine.turn_active(session)
    }

    /// Cancel every execution surface for a Session: the engine turn first
    /// (recording `cause: user_cancel` on its closing `MessageFinished`), then
    /// the parent-model run and the Workflow run.
    pub(crate) fn cancel_run(&self, session: hya_proto::SessionId) -> bool {
        let turn = self
            .engine
            .cancel_turn(session, hya_proto::FinishCause::UserCancel);
        let model = self.runs.cancel(session);
        let workflow = self.workflow_control.cancel(session);
        turn || model || workflow
    }
}
