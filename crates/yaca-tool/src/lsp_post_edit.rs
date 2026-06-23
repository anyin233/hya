use std::path::Path;

use serde_json::Value;

use crate::lsp_plane::LspPlane;
use crate::tool::ToolError;

pub(crate) async fn touch_and_diagnostics(lsp: &LspPlane, path: &Path) -> Result<Value, ToolError> {
    lsp.touch_file(path, "document")
        .await
        .map_err(|error| ToolError::Other(error.to_string()))?;
    lsp.diagnostics()
        .await
        .map_err(|error| ToolError::Other(error.to_string()))
}
