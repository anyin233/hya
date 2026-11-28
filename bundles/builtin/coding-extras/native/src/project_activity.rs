//! Read-only activity query for sessions sharing a Project.
use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::tool::obj_schema;
use hya_tool::{ProjectActivityPlane, ProjectActivityRequest, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

/// Read-only activity query tool for sessions sharing a Project.
pub(crate) struct ProjectActivityTool;

#[derive(Deserialize, Default)]
struct Input {
    since_ms: Option<i64>,
    limit: Option<usize>,
    #[serde(default)]
    include_self: bool,
}

#[async_trait]
impl Tool for ProjectActivityTool {
    fn name(&self) -> &str {
        "project_activity"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "project_activity",
            "Show recent sessions and changed files in the caller's Project. Busy/idle is daemon-local live state.",
            json!({
                "since_ms": {"type":"integer", "description":"Unix epoch milliseconds; defaults to the last two hours"},
                "limit": {"type":"integer", "minimum":1, "maximum":200, "default":50},
                "include_self": {"type":"boolean", "default":false}
            }),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let plane = ctx
            .extension::<ProjectActivityPlane>()
            .cloned()
            .unwrap_or_else(ProjectActivityPlane::disconnected);
        let result = plane
            .query(ProjectActivityRequest {
                since_ms: input.since_ms,
                limit: input.limit,
                include_self: input.include_self,
            })
            .await?;
        Ok(json!({
            "title": "Project activity",
            "sessions": result.sessions,
            "files": result.files,
            "note": result.note,
        }))
    }
}
