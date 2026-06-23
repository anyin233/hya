use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::lsp_path::{absolutize, file_uri, normalize};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LspOperation {
    GoToDefinition,
    FindReferences,
    Hover,
    DocumentSymbol,
    WorkspaceSymbol,
    GoToImplementation,
    PrepareCallHierarchy,
    IncomingCalls,
    OutgoingCalls,
}

impl LspOperation {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspRequest {
    pub operation: LspOperation,
    pub file: PathBuf,
    pub uri: String,
    pub line: u32,
    pub character: u32,
    pub query: Option<String>,
}

#[derive(Error, Debug)]
#[error("{0}")]
pub struct LspError(pub String);

#[async_trait]
pub trait LspProvider: Send + Sync {
    async fn has_clients(&self, file: &Path) -> Result<bool, LspError>;
    async fn execute(&self, request: LspRequest) -> Result<Vec<Value>, LspError>;
    async fn touch_file(&self, _file: &Path, _kind: &str) -> Result<(), LspError> {
        Ok(())
    }
    async fn diagnostics(&self) -> Result<Value, LspError> {
        Ok(json!({}))
    }
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

#[derive(Clone, Default)]
pub struct LspPlane {
    provider: Option<Arc<dyn LspProvider>>,
}

impl LspPlane {
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

    pub async fn status(&self, workdir: &Path) -> Result<Vec<Value>, LspError> {
        match &self.provider {
            Some(provider) => provider.status(workdir).await,
            None => Ok(Vec::new()),
        }
    }

    pub async fn touch_file(&self, file: &Path, kind: &str) -> Result<(), LspError> {
        match &self.provider {
            Some(provider) => provider.touch_file(file, kind).await,
            None => Ok(()),
        }
    }

    pub async fn diagnostics(&self) -> Result<Value, LspError> {
        match &self.provider {
            Some(provider) => provider.diagnostics().await,
            None => Ok(json!({})),
        }
    }
}
