//! Native ABI v2 [`NativeHook`] delivering unread team mail inside tool results
//! (`HookPoint::ToolExecuteAfter`, ADR-0016 / ADR-0039).

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::Event;
use hya_runtime_abi::{
    HookPoint, HostError, NativeHook, NativeToolAfterInput, NativeToolAfterOutcome,
    NativeToolResult,
};
use serde_json::{Value, json};

use crate::service::{AGENT_CHANNELS_BUNDLE_ID, AgentChannelsService};

/// Hook id registered by `hya/agent-channels`.
pub const UNREAD_MAIL_STEER_HOOK_ID: &str = "unread-mail-steer";

/// Transient-state key the host sets to `true` around after-hooks whose
/// result shows no further notices (`report` latch, deferred steer, `wait`,
/// user shell commands).
pub(crate) const STEER_SUPPRESSED_KEY: &str = "steer_suppressed";

/// In-turn unread-mail steer hook running on `HookPoint::ToolExecuteAfter`.
///
/// For any successful non-`wait` / non-`report` tool call where the host has
/// not set the `steer_suppressed` transient key, checks the session's durable
/// team inbox past its acknowledged cursor, splices any policy-permitted
/// `[NEW MAIL]` notice into the tool result, and stages the `mail_consumed`
/// event with the tool result so both commit in one store transaction. The
/// host's mail watermark advances only after that commit.
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
        let suppressed = runtime.bundle_storage().get_transient_state(
            AGENT_CHANNELS_BUNDLE_ID,
            input.session,
            STEER_SUPPRESSED_KEY,
        )?;
        if suppressed == Some(Value::Bool(true)) {
            return Ok(NativeToolAfterOutcome::Continue {
                result: NativeToolResult::Ok { output, time_ms },
            });
        }
        if let Some(prepared) =
            AgentChannelsService::prepare_unread_steer(runtime.as_ref(), input.session).await?
        {
            let Event::Extension {
                session: root,
                bundle,
                kind,
                payload,
            } = prepared.ack
            else {
                return Err(HostError::Internal(
                    "mail acknowledgement is not an extension event".into(),
                ));
            };
            let notifier = Arc::clone(&runtime);
            let (session, mark, incoming) =
                (input.session, prepared.mark, prepared.consumed_incoming);
            runtime.session_store().stage_extension_event(
                input.session,
                root,
                &bundle,
                &kind,
                payload,
                Some(Box::new(move || {
                    notifier
                        .turn_runner()
                        .notify_mail_consumed(session, mark, incoming);
                })),
            )?;
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
