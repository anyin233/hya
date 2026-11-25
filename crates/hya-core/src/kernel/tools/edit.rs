use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::literal_span::{UniqueSpanError, find_unique_span};
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
        if old == new {
            return Err(ToolError::Input(
                "`old` and `new` are identical. The file was not changed".into(),
            ));
        }
        let file = super::resolve(&ctx.workdir, &path);
        let mut text = tokio::fs::read_to_string(&file).await?;
        let span = find_unique_span(&text, &old).map_err(|error| {
            let count = match error {
                UniqueSpanError::Empty => {
                    return ToolError::Input("`old` must be non-empty".into());
                }
                UniqueSpanError::Missing => 0,
                UniqueSpanError::Multiple { .. } => {
                    let count = text.matches(old.as_str()).count();
                    if count == 1 {
                        return ToolError::Input(format!(
                            "`old` has overlapping matches in {path}; it must occur exactly once"
                        ));
                    }
                    count
                }
            };
            ToolError::Input(format!(
                "`old` occurs {count} times in {path}; it must occur exactly once"
            ))
        })?;
        text.replace_range(span, &new);
        tokio::fs::write(&file, text).await?;
        Ok(json!({"replaced": 1}))
    }
}
