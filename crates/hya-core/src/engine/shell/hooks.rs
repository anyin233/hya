use hya_proto::{MessageId, SessionId, ToolCallId};
use hya_tool::ToolError;
use serde_json::Value;

use crate::engine::SessionEngine;
use crate::hooks::{ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolOutcomeNative};

pub(super) struct AfterHookCall<'a> {
    pub(super) session: SessionId,
    pub(super) message: MessageId,
    pub(super) call: ToolCallId,
    pub(super) tool: &'a str,
    pub(super) input: Option<Value>,
    pub(super) time_ms: u64,
}

pub(super) async fn apply_tool_after_hooks(
    engine: &SessionEngine,
    result: Result<Value, ToolError>,
    call: AfterHookCall<'_>,
) -> Result<Value, ToolError> {
    let Some(hooks) = &engine.hooks else {
        return result;
    };
    let was_permission_err = matches!(&result, Err(ToolError::Permission(_)));
    let native = match &result {
        Ok(output) => ToolOutcomeNative::Ok {
            output: output.clone(),
            time_ms: call.time_ms,
        },
        Err(error) => ToolOutcomeNative::Err {
            message: error.to_string(),
        },
    };
    let ToolExecuteAfterOutcome::Continue { result: rewritten } = hooks
        .tool_execute_after(ToolExecuteAfterInput {
            session: call.session,
            message: call.message,
            call: call.call,
            tool: call.tool.to_string(),
            input: call.input.unwrap_or_default(),
            result: native,
        })
        .await;
    if was_permission_err {
        result
    } else {
        match rewritten {
            ToolOutcomeNative::Ok { output, .. } => Ok(output),
            ToolOutcomeNative::Err { message } => Err(ToolError::Other(message)),
        }
    }
}
