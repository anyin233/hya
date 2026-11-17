use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::{Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) struct Edit;

#[derive(Deserialize)]
struct Args {
    path: String,
    old: String,
    new: String,
}

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }

    fn schema(&self) -> ToolSchema {
        super::schema(
            "edit",
            "Replace the one occurrence of `old` in a file with `new`.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path, relative to the working directory or absolute"},
                    "old": {"type": "string", "description": "Exact text to replace; must occur exactly once"},
                    "new": {"type": "string", "description": "Replacement text"},
                },
                "required": ["path", "old", "new"],
            }),
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let Args { path, old, new } = super::args(input)?;
        let file = super::resolve(&ctx.workdir, &path);
        let text = tokio::fs::read_to_string(&file).await?;
        let count = text.matches(old.as_str()).count();
        if count != 1 {
            return Err(ToolError::Input(format!(
                "`old` occurs {count} times in {path}; it must occur exactly once"
            )));
        }
        tokio::fs::write(&file, text.replacen(old.as_str(), &new, 1)).await?;
        Ok(json!({"replaced": 1}))
    }
}
