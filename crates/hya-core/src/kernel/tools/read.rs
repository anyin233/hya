use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::{Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) struct Read;

#[derive(Deserialize)]
struct Args {
    path: String,
}

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> ToolSchema {
        super::schema(
            "read",
            "Read a UTF-8 text file.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "File path, relative to the working directory or absolute"}},
                "required": ["path"],
            }),
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let Args { path } = super::args(input)?;
        let content = tokio::fs::read_to_string(super::resolve(&ctx.workdir, &path)).await?;
        Ok(json!({"content": content}))
    }
}
