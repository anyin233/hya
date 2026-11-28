use hya_proto::{MessageId, SessionId, ToolCallId};
use hya_tool::ToolError;
use serde_json::Value;

use crate::engine::SessionEngine;
use crate::hooks::{NativeToolAfterInput, NativeToolResult};

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
    if matches!(&result, Err(ToolError::Cancelled)) {
        return result;
    }
    let Some(hooks) = engine.active_hook_dispatcher(call.session) else {
        return result;
    };
    let was_permission_err = matches!(&result, Err(ToolError::Permission(_)));
    let native = match &result {
        Ok(output) => NativeToolResult::Ok {
            output: output.clone(),
            time_ms: call.time_ms,
        },
        Err(error) => NativeToolResult::Err {
            message: error.to_string(),
        },
    };
    // The shell commits no staged events, so unread mail waits for the next
    // model tool call instead of being shown here without an ack.
    engine.set_mail_steer_suppressed(call.session, true);
    let rewritten = hooks
        .tool_execute_after(NativeToolAfterInput {
            session: call.session,
            message: call.message,
            call: call.call,
            tool: call.tool.to_string(),
            input: call.input.unwrap_or_default(),
            result: native,
        })
        .await;
    engine.set_mail_steer_suppressed(call.session, false);
    if was_permission_err {
        result
    } else {
        match rewritten {
            NativeToolResult::Ok { output, .. } => Ok(output),
            NativeToolResult::Err { message } => Err(ToolError::Other(message)),
        }
    }
}
