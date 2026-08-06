//! Language-server plane: operations, provider trait, and default-disconnected plane.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::lsp_path::{absolutize, file_uri, normalize};

/// LSP operation names advertised by the `lsp` tool (camelCase wire values).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LspOperation {
    /// Jump to definition.
    GoToDefinition,
    /// Find all references.
    FindReferences,
    /// Hover documentation.
    Hover,
    /// Document outline symbols.
    DocumentSymbol,
    /// Workspace-wide symbol search (uses `query`).
    WorkspaceSymbol,
    /// Jump to implementation.
    GoToImplementation,
    /// Prepare call hierarchy at a position.
    PrepareCallHierarchy,
    /// Incoming call hierarchy edges.
    IncomingCalls,
    /// Outgoing call hierarchy edges.
    OutgoingCalls,
}

impl LspOperation {
    /// Wire / display string for the operation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GoToDefinition => "goToDefinition",
            Self::FindReferences => "findReferences",
            Self::Hover => "hover",
            Self::DocumentSymbol => "documentSymbol",
            Self::WorkspaceSymbol => "workspaceSymbol",
            Self::GoToImplementation => "goToImplementation",
            Self::PrepareCallHierarchy => "prepareCallHierarchy",
            Self::IncomingCalls => "incomingCalls",
            Self::OutgoingCalls => "outgoingCalls",
        }
    }
}

/// Fully resolved request handed to an [`LspProvider`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspRequest {
    /// Operation to run.
    pub operation: LspOperation,
    /// Absolute file path.
    pub file: PathBuf,
    /// `file://` URI for the language server.
    pub uri: String,
    /// Zero-based line.
    pub line: u32,
    /// Zero-based character offset.
    pub character: u32,
    /// Query string for workspace symbol search.
    pub query: Option<String>,
}

/// Provider or plane error message.
#[derive(Error, Debug)]
#[error("{0}")]
pub struct LspError(pub String);

/// Backend that talks to one or more language servers.
#[async_trait]
pub trait LspProvider: Send + Sync {
    /// Whether any client can handle `file`.
    async fn has_clients(&self, file: &Path) -> Result<bool, LspError>;
    /// Execute a request and return JSON results.
    async fn execute(&self, request: LspRequest) -> Result<Vec<Value>, LspError>;
    /// Notify the server that a file changed (`kind` is implementation-defined).
    async fn touch_file(&self, _file: &Path, _kind: &str) -> Result<(), LspError> {
        Ok(())
    }
    /// Collect current diagnostics payload.
    async fn diagnostics(&self) -> Result<Value, LspError> {
        Ok(json!({}))
    }
    /// Status rows for UI / health display.
    async fn status(&self, workdir: &Path) -> Result<Vec<Value>, LspError> {
        if self.has_clients(workdir).await? {
            Ok(vec![json!({
                "id": "lsp",
                "name": "lsp",
                "root": "",
                "status": "connected"
            })])
        } else {
            Ok(Vec::new())
        }
    }
}

/// Optional LSP provider holder used by tools and post-edit hooks.
#[derive(Clone, Default)]
pub struct LspPlane {
    provider: Option<Arc<dyn LspProvider>>,
}

impl LspPlane {
    /// Build a plane around a concrete provider.
    #[must_use]
    pub fn new(provider: Arc<dyn LspProvider>) -> Self {
        Self {
            provider: Some(provider),
        }
    }

    pub(crate) async fn has_clients(&self, file: &Path) -> Result<bool, LspError> {
        match &self.provider {
            Some(provider) => provider.has_clients(file).await,
            None => Ok(false),
        }
    }

    pub(crate) async fn execute(&self, request: LspRequest) -> Result<Vec<Value>, LspError> {
        match &self.provider {
            Some(provider) => provider.execute(request).await,
            None => Err(LspError(
                "No LSP server available for this file type.".to_string(),
            )),
        }
    }

    /// Run a workspace-symbol query rooted at `workdir`.
    ///
    /// # Errors
    /// Propagates provider failures.
    pub async fn workspace_symbols(
        &self,
        workdir: &Path,
        query: String,
    ) -> Result<Vec<Value>, LspError> {
        let file = normalize(&absolutize(workdir));
        match &self.provider {
            Some(provider) => {
                provider
                    .execute(LspRequest {
                        operation: LspOperation::WorkspaceSymbol,
                        file: file.clone(),
                        uri: file_uri(&file),
                        line: 0,
                        character: 0,
                        query: Some(query),
                    })
                    .await
            }
            None => Ok(Vec::new()),
        }
    }

    /// Provider status rows, or empty when disconnected.
    ///
    /// # Errors
    /// Propagates provider failures.
    pub async fn status(&self, workdir: &Path) -> Result<Vec<Value>, LspError> {
        match &self.provider {
            Some(provider) => provider.status(workdir).await,
            None => Ok(Vec::new()),
        }
    }

    /// Notify the provider of a file change; no-op when disconnected.
    ///
    /// # Errors
    /// Propagates provider failures.
    pub async fn touch_file(&self, file: &Path, kind: &str) -> Result<(), LspError> {
        match &self.provider {
            Some(provider) => provider.touch_file(file, kind).await,
            None => Ok(()),
        }
    }

    /// Current diagnostics JSON, or `{}` when disconnected.
    ///
    /// # Errors
    /// Propagates provider failures.
    pub async fn diagnostics(&self) -> Result<Value, LspError> {
        match &self.provider {
            Some(provider) => provider.diagnostics().await,
            None => Ok(json!({})),
        }
    }
}
