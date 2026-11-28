//! Native ABI v2 [`NativeHook`] delivering unread team mail inside tool results
//! (`HookPoint::ToolExecuteAfter`, ADR-0016 / ADR-0039).

use std::sync::Arc;

use async_trait::async_trait;
use hya_runtime_abi::{
    HookPoint, HostError, NativeHook, NativeToolAfterInput, NativeToolAfterOutcome,
    NativeToolResult,
};
use serde_json::{Value, json};

use crate::service::AgentChannelsService;

/// Hook id registered by `hya/agent-channels`.
pub const UNREAD_MAIL_STEER_HOOK_ID: &str = "unread-mail-steer";

/// In-turn unread-mail steer hook running on `HookPoint::ToolExecuteAfter`.
///
/// For any successful non-`wait` / non-`report` tool call where steering is not
/// suppressed on the host (`report` latch or deferred steer), checks the
/// session's durable team inbox past its acknowledged cursor, splices any
/// policy-permitted `[NEW MAIL]` notice into the tool result, and stages the
/// corresponding `mail_consumed` event on the host so the tool result and inbox
/// acknowledgment commit atomically in one store transaction.
pub struct UnreadMailSteerHook {
    service: Arc<AgentChannelsService>,
}

impl UnreadMailSteerHook {
    /// Construct an unread-mail steer hook backed by `service`.
    #[must_use]
    pub fn new(service: Arc<AgentChannelsService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl NativeHook for UnreadMailSteerHook {
    fn id(&self) -> &str {
        UNREAD_MAIL_STEER_HOOK_ID
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::ToolExecuteAfter]
    }

    async fn tool_execute_after(
        &self,
        input: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, HostError> {
        // `wait` owns its own inbox ack and `report` ends the turn immediately.
        if input.tool == "wait" || input.tool == "report" {
            return Ok(NativeToolAfterOutcome::Continue {
                result: input.result,
            });
        }
        let NativeToolResult::Ok {
            mut output,
            time_ms,
        } = input.result
        else {
            return Ok(NativeToolAfterOutcome::Continue {
                result: input.result,
            });
        };
        let Ok(runtime) = self.service.runtime() else {
            return Ok(NativeToolAfterOutcome::Continue {
                result: NativeToolResult::Ok { output, time_ms },
            });
        };
        if runtime
            .turn_runner()
            .is_mail_steer_suppressed(input.session)
        {
            return Ok(NativeToolAfterOutcome::Continue {
                result: NativeToolResult::Ok { output, time_ms },
            });
        }
        if let Some(prepared) =
            AgentChannelsService::prepare_unread_steer(runtime.as_ref(), input.session).await?
        {
            runtime
                .turn_runner()
                .stage_mail_steer_ack(input.session, prepared.ack);
            if let Some(notice) = prepared.notice {
                append_steer_notice(&mut output, &notice);
            }
        }
        Ok(NativeToolAfterOutcome::Continue {
            result: NativeToolResult::Ok { output, time_ms },
        })
    }
}

/// Splice a steer notice into a tool result's model-facing text.
pub fn append_steer_notice(output: &mut Value, notice: &str) {
    let Value::Object(object) = output else {
        *output = json!({
            "result": std::mem::take(output),
            "steer": notice.trim_start(),
        });
        return;
    };
    let (key, updated) = match (object.get("output"), object.get("steer")) {
        (Some(Value::String(text)), _) => ("output", format!("{text}{notice}")),
        (_, Some(Value::String(text))) => ("steer", format!("{text}{notice}")),
        _ => ("steer", notice.trim_start().to_string()),
    };
    object.insert(key.to_string(), Value::String(updated));
}
