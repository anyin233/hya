//! Test-only ABI v2 replacement for the `hya/multi-agent` bundle, built as a real shared library.
use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_runtime_abi::HostError;
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

struct Replacement(&'static str);

#[async_trait]
impl Tool for Replacement {
    fn name(&self) -> &str {
        self.0
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.0),
            description: "replacement fixture".into(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
        }
    }

    async fn execute(&self, _: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let mut output = json!({"implementation":"replacement", "input":input});
        if let Some(deadline) = hya_tool::deadline::current() {
            output["budgetSeconds"] = json!(deadline.budget().as_secs());
            output["cappedSeconds"] = json!(deadline.cap(None).0.as_secs());
        }
        Ok(output)
    }
}

hya_runtime_abi::export_native_bundle_v2!(|_ctx, out| {
    for name in ["list_agents", "task", "search_agent", "archive", "wait"] {
        out.tools.push(Arc::new(Replacement(name)));
    }
    Ok::<(), HostError>(())
});
