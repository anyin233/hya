use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::{Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) struct Write;

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &str {
        "write"
    }

    fn schema(&self) -> ToolSchema {
        super::schema(
            "write",
            "Create or overwrite a file, creating parent directories.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path, relative to the working directory or absolute"},
                    "content": {"type": "string", "description": "Full new file content"},
                },
                "required": ["path", "content"],
            }),
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let Args { path, content } = super::args(input)?;
        let path = super::resolve(&ctx.workdir, &path);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, content.as_bytes()).await?;
        Ok(json!({"bytes": content.len()}))
    }
}
