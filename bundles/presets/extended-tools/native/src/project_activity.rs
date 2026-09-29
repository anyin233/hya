//! Read-only activity query for sessions sharing a Project.
use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::tool::obj_schema;
use hya_tool::{ProjectActivityError, ProjectActivityRequest, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

pub struct ProjectActivityTool;

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
        let result = ctx
            .project_activity
            .query(ProjectActivityRequest {
                since_ms: input.since_ms,
                limit: input.limit,
                include_self: input.include_self,
            })
            .await
            .map_err(map_err)?;
        Ok(json!({
            "title": "Project activity",
            "sessions": result.sessions,
            "files": result.files,
            "note": result.note,
        }))
    }
}

fn map_err(error: ProjectActivityError) -> ToolError {
    match error {
        ProjectActivityError::Unavailable => {
            ToolError::Other("project activity service unavailable".to_string())
        }
        ProjectActivityError::Rejected(message) => ToolError::Input(message),
    }
}
