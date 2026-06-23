use std::sync::Arc;

use tokio::sync::mpsc;
use yaca_core::{AgentSpec, SessionEngine};
use yaca_mcp::McpManager;
use yaca_proto::WorkspaceAdapterInfo;
use yaca_tool::{AskRequest, FormatterStatus, QuestionRequest};

use crate::{opencode, pending, runs};

#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<SessionEngine>,
    pub agent: Arc<AgentSpec>,
    permission_requests: pending::PermissionRequests,
    question_requests: pending::QuestionRequests,
    mcp_manager: Arc<McpManager>,
    workspace_adapters: Vec<WorkspaceAdapterInfo>,
    formatter_status: Vec<FormatterStatus>,
}

impl AppState {
    #[must_use]
    pub fn new(engine: Arc<SessionEngine>, agent: Arc<AgentSpec>) -> Self {
        let permission_requests = pending::PermissionRequests::new(engine.store().clone());
        Self {
            engine,
            agent,
            permission_requests,
            question_requests: Default::default(),
            mcp_manager: Default::default(),
            workspace_adapters: Vec::new(),
            formatter_status: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_permission_requests(mut self, rx: mpsc::UnboundedReceiver<AskRequest>) -> Self {
        self.permission_requests =
            pending::PermissionRequests::spawn(rx, self.engine.store().clone());
        self
    }

    #[must_use]
    pub fn with_question_requests(mut self, rx: mpsc::UnboundedReceiver<QuestionRequest>) -> Self {
        self.question_requests = pending::QuestionRequests::spawn(rx);
        self
    }

    #[must_use]
    pub fn with_mcp_manager(mut self, manager: McpManager) -> Self {
        self.mcp_manager = Arc::new(manager);
        self
    }

    #[must_use]
    pub fn with_workspace_adapters(mut self, adapters: Vec<WorkspaceAdapterInfo>) -> Self {
        self.workspace_adapters = adapters;
        self
    }

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
    pub(crate) global: opencode::GlobalState,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) mcp_http: opencode::McpHttpState,
    pub(crate) project: opencode::ProjectState,
    pub(crate) pty: opencode::PtyState,
    pub(crate) tui: opencode::TuiState,
    pub(crate) workspace_adapters: Vec<WorkspaceAdapterInfo>,
    pub(crate) formatter_status: Vec<FormatterStatus>,
}

impl ServerState {
    pub(crate) fn new(app: AppState) -> Self {
        Self {
            engine: app.engine,
            agent: app.agent,
            runs: runs::RunRegistry::default(),
            permission_requests: app.permission_requests,
            question_requests: app.question_requests,
            global: opencode::GlobalState::new(),
            mcp_manager: app.mcp_manager,
            mcp_http: opencode::McpHttpState::new(),
            project: opencode::ProjectState::new(),
            pty: opencode::PtyState::new(),
            tui: opencode::TuiState::new(),
            workspace_adapters: app.workspace_adapters,
            formatter_status: app.formatter_status,
        }
    }
}
