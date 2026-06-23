use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use yaca_proto::{ToolName, ToolSchema};

use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};

const UTF8_BOM: char = '\u{feff}';
const UTF8_BOM_BYTES: &[u8; 3] = b"\xEF\xBB\xBF";

#[derive(Deserialize)]
struct WriteInput {
    #[serde(default, alias = "filePath")]
    path: Option<String>,
    content: String,
}

pub(crate) struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("write"),
            description: "Write content to a file (creating parent dirs).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "filePath": { "type": "string" },
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["filePath", "content"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: WriteInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let file_path = input
            .path
            .as_deref()
            .ok_or_else(|| ToolError::Input("missing field `filePath`".to_string()))?;
        let workdir = normalize(&absolutize(&ctx.workdir));
        let path = resolve_file(&workdir, file_path);
        assert_external_file(ctx, &workdir, &path).await?;
        ctx.permission
            .assert(Action::Edit, Resource::Path(display_path(&path)))
            .await?;

        let exists = path.exists();
        let source_has_bom = if exists {
            tokio::fs::read(&path).await?.starts_with(UTF8_BOM_BYTES)
        } else {
            false
        };
        let (incoming_has_bom, content) = split_bom(&input.content);
        let desired_bom = source_has_bom || incoming_has_bom;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, encode_with_bom(content, desired_bom)).await?;
        ctx.formatter
            .format_file(&workdir, &path)
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;

        Ok(json!({
            "ok": true,
            "bytes": input.content.len(),
            "title": relative_title(&path, &workdir),
            "output": "Wrote file successfully.",
            "metadata": {
                "diagnostics": {},
                "filepath": display_path(&path),
                "exists": exists,
            },
        }))
    }
}

fn split_bom(text: &str) -> (bool, &str) {
    if text.starts_with(UTF8_BOM) {
        return (true, &text[UTF8_BOM.len_utf8()..]);
    }
    (false, text)
}

fn encode_with_bom(text: &str, bom: bool) -> Vec<u8> {
    let extra = if bom { UTF8_BOM_BYTES.len() } else { 0 };
    let mut out = Vec::with_capacity(text.len() + extra);
    if bom {
        out.extend_from_slice(UTF8_BOM_BYTES);
    }
    out.extend_from_slice(text.as_bytes());
    out
}

async fn assert_external_file(ctx: &ToolCtx, workdir: &Path, path: &Path) -> Result<(), ToolError> {
    if path.starts_with(workdir) {
        return Ok(());
    }
    let parent = path
        .parent()
        .map_or_else(|| Path::new("/").to_path_buf(), Path::to_path_buf);
    ctx.permission
        .assert(
            Action::ExternalDirectory,
            Resource::Path(display_path(&parent.join("*"))),
        )
        .await?;
    Ok(())
}

fn relative_title(path: &std::path::Path, workdir: &std::path::Path) -> String {
    path.strip_prefix(workdir)
        .map_or_else(|_| display_path(path), display_path)
}
