use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use yaca_proto::{ToolName, ToolSchema};

use crate::lsp_path::{absolutize, display_path, file_uri, normalize, resolve_file};
use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};

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

    async fn has_clients(&self, file: &Path) -> Result<bool, LspError> {
        match &self.provider {
            Some(provider) => provider.has_clients(file).await,
            None => Ok(false),
        }
    }

    async fn execute(&self, request: LspRequest) -> Result<Vec<Value>, LspError> {
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
}

pub(crate) struct LspTool;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LspInput {
    operation: LspOperation,
    file_path: String,
    line: u32,
    character: u32,
    #[serde(default)]
    query: Option<String>,
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("lsp"),
            description: include_str!("lsp.txt").to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["goToDefinition", "findReferences", "hover", "documentSymbol", "workspaceSymbol", "goToImplementation", "prepareCallHierarchy", "incomingCalls", "outgoingCalls"],
                        "description": "The LSP operation to perform"
                    },
                    "filePath": { "type": "string", "description": "The absolute or relative path to the file" },
                    "line": { "type": "integer", "minimum": 1, "description": "The line number (1-based, as shown in editors)" },
                    "character": { "type": "integer", "minimum": 1, "description": "The character offset (1-based, as shown in editors)" },
                    "query": { "type": "string", "description": "Search query for workspaceSymbol. Empty string requests all symbols." }
                },
                "required": ["operation", "filePath", "line", "character"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: LspInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        if input.line == 0 || input.character == 0 {
            return Err(ToolError::Input(
                "line and character must be greater than or equal to 1".to_string(),
            ));
        }

        let workdir = normalize(&absolutize(&ctx.workdir));
        let file = resolve_file(&workdir, &input.file_path);
        assert_external_directory(ctx, &workdir, &file).await?;
        ctx.permission
            .assert(Action::Lsp, Resource::Path(display_path(&file)))
            .await?;

        if tokio::fs::metadata(&file).await.is_err() {
            return Err(ToolError::Other(format!(
                "File not found: {}",
                display_path(&file)
            )));
        }
        if !ctx
            .lsp
            .has_clients(&file)
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?
        {
            return Err(ToolError::Other(
                "No LSP server available for this file type.".to_string(),
            ));
        }

        let rel = file.strip_prefix(&workdir).unwrap_or(&file);
        let detail = match input.operation {
            LspOperation::WorkspaceSymbol => String::new(),
            LspOperation::DocumentSymbol => display_path(rel),
            _ => format!("{}:{}:{}", display_path(rel), input.line, input.character),
        };
        let title = if detail.is_empty() {
            input.operation.as_str().to_string()
        } else {
            format!("{} {detail}", input.operation.as_str())
        };
        let request = LspRequest {
            operation: input.operation,
            file: file.clone(),
            uri: file_uri(&file),
            line: input.line - 1,
            character: input.character - 1,
            query: input.query,
        };
        let result = ctx
            .lsp
            .execute(request)
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        let output = if result.is_empty() {
            format!("No results found for {}", input.operation.as_str())
        } else {
            serde_json::to_string_pretty(&result)?
        };
        Ok(json!({
            "title": title,
            "metadata": { "result": result },
            "output": output,
        }))
    }
}

async fn assert_external_directory(
    ctx: &ToolCtx,
    workdir: &Path,
    file: &Path,
) -> Result<(), ToolError> {
    if file.starts_with(workdir) {
        return Ok(());
    }
    let parent = file.parent().unwrap_or(file);
    let glob = parent.join("*");
    ctx.permission
        .assert(
            Action::ExternalDirectory,
            Resource::Path(display_path(&glob)),
        )
        .await?;
    Ok(())
}
