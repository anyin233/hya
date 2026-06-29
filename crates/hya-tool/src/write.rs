use std::path::Path;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::lsp_post_edit;
use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};
use crate::utf8_bom;

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
            utf8_bom::file_has_bom(&path).await?
        } else {
            false
        };
        let (incoming_has_bom, content) = utf8_bom::split(&input.content);
        let desired_bom = source_has_bom || incoming_has_bom;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, utf8_bom::encode(content, desired_bom)).await?;
        let formatted = ctx
            .formatter
            .format_file(&workdir, &path)
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
        if formatted {
            utf8_bom::sync_file(&path, desired_bom).await?;
        }
        let diagnostics = lsp_post_edit::touch_and_diagnostics(&ctx.lsp, &path).await?;
        let mut output = "Wrote file successfully.".to_string();
        lsp_post_edit::append_write_diagnostics(&mut output, &path, &diagnostics);

        Ok(json!({
            "ok": true,
            "bytes": input.content.len(),
            "title": relative_title(&path, &workdir),
            "output": output,
            "metadata": {
                "diagnostics": diagnostics,
                "filepath": display_path(&path),
                "exists": exists,
            },
        }))
    }
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
