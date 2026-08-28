//! Shared HTTP application state for native and Compat routes.

use std::sync::Arc;

use hya_core::{AgentSpec, SessionEngine};
use hya_proto::WorkspaceAdapterInfo;
use hya_tool::{AskRequest, FormatterStatus, QuestionRequest};
use tokio::sync::mpsc;

use crate::mcp_control::{EmptyMcpControl, McpControl};
use crate::workflow_control::{EmptyWorkflowControl, WorkflowControl};
use crate::{compat, pending, runs};

/// Holds the session engine, process agent base, permission/question queues,
/// MCP and Workflow control handles, workspace adapters, and formatter status.
/// The router wraps this into internal `ServerState` (run registry + Compat
/// process-local state).
#[derive(Clone)]
pub struct AppState {
    /// Shared session engine for all routes.
    pub engine: Arc<SessionEngine>,
    /// Process-level agent base used by native turns.
    pub agent: Arc<AgentSpec>,
    permission_requests: pending::PermissionRequests,
    question_requests: pending::QuestionRequests,
    mcp_control: Arc<dyn McpControl>,
    workflow_control: Arc<dyn WorkflowControl>,
    workspace_adapters: Vec<WorkspaceAdapterInfo>,
    formatter_status: Vec<FormatterStatus>,
    default_agent: Option<String>,
}

impl AppState {
    /// Create state with empty pending queues and no-op MCP and Workflow control handles.
    #[must_use]
    pub fn new(engine: Arc<SessionEngine>, agent: Arc<AgentSpec>) -> Self {
        let permission_requests = pending::PermissionRequests::new(engine.store().clone());
        Self {
            engine,
            agent,
            permission_requests,
            question_requests: pending::QuestionRequests::default(),
            mcp_control: Arc::new(EmptyMcpControl),
            workflow_control: Arc::new(EmptyWorkflowControl),
            workspace_adapters: Vec::new(),
            formatter_status: Vec::new(),
            default_agent: None,
        }
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
}

#[derive(Clone)]
pub(crate) struct ServerState {
    pub(crate) engine: Arc<SessionEngine>,
    pub(crate) agent: Arc<AgentSpec>,
    pub(crate) runs: runs::RunRegistry,
    pub(crate) permission_requests: pending::PermissionRequests,
    pub(crate) question_requests: pending::QuestionRequests,
    pub(crate) global: compat::GlobalState,
    pub(crate) mcp_control: Arc<dyn McpControl>,
    pub(crate) workflow_control: Arc<dyn WorkflowControl>,
    pub(crate) project: compat::ProjectState,
    pub(crate) pty: compat::PtyState,
    pub(crate) tui: compat::TuiState,
    pub(crate) workspace_adapters: Vec<WorkspaceAdapterInfo>,
    pub(crate) formatter_status: Vec<FormatterStatus>,
    pub(crate) default_agent: Option<String>,
}

impl ServerState {
    pub(crate) fn new(app: AppState) -> Self {
        Self {
            engine: app.engine,
            agent: app.agent,
            runs: runs::RunRegistry::default(),
            permission_requests: app.permission_requests,
            question_requests: app.question_requests,
            global: compat::GlobalState::new(),
            mcp_control: app.mcp_control,
            workflow_control: app.workflow_control,
            project: compat::ProjectState::new(),
            pty: compat::PtyState::new(),
            tui: compat::TuiState::new(),
            workspace_adapters: app.workspace_adapters,
            formatter_status: app.formatter_status,
            default_agent: app.default_agent,
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

    /// Return whether either execution surface currently owns the Session.
    pub(crate) fn is_busy(&self, session: hya_proto::SessionId) -> bool {
        self.runs.is_busy(session) || self.workflow_control.active_run(session).is_some()
    }

    /// Cancel both parent-model and Workflow execution surfaces for a Session.
    pub(crate) fn cancel_run(&self, session: hya_proto::SessionId) -> bool {
        let model = self.runs.cancel(session);
        let workflow = self.workflow_control.cancel(session);
        model || workflow
    }
}
