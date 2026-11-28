//! One tool call of a turn, for its harness's `ToolGate`: hooks,
//! permissions, tool context, file snapshots, output capping, and steer
//! notices around the call.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use hya_proto::{ActorClaim, MessageId, SessionId, ToolCallId, ToolSchema};
use hya_tool::{AgentDef, ReportLatch, ToolCtx, ToolError};
use tokio::sync::Mutex;

use super::super::file_snapshot::{FileCapture, observation};
use super::super::roots::SessionWorkspace;
use super::super::shell::BashArtifactGuard;
use super::super::tool_error::{tool_error_message_value, tool_error_value};
use super::super::{AgentSpec, SessionEngine, authorize_tool_call};
use super::messages::hidden_behind;
use crate::TurnBinding;
use crate::error::CoreError;
use crate::hooks::{
    HookChain, NativeToolAfterInput, NativeToolBeforeInput, NativeToolBeforeOutcome,
    NativeToolResult, activation_hook_for,
};
use crate::kernel::ToolCall;
use crate::runtime_registry::CompiledResourceView;

/// One round's tool calls.
pub(super) struct TurnToolGate<'a> {
    pub(super) engine: &'a SessionEngine,
    pub(super) session: SessionId,
    pub(super) message: MessageId,
    pub(super) binding: &'a TurnBinding,
    pub(super) resources: &'a Arc<CompiledResourceView>,
    pub(super) agents: &'a Arc<[AgentDef]>,
    pub(super) agent: &'a AgentSpec,
    pub(super) external_dirs: &'a [PathBuf],
    pub(super) workspace: &'a SessionWorkspace,
    pub(super) actor_claim: Option<&'a ActorClaim>,
    pub(super) guidance: &'a Option<Arc<str>>,
    pub(super) cancel: &'a tokio_util::sync::CancellationToken,
    pub(super) state: &'a mut ToolState,
    /// Engine handle for nested tool calls, cloned once per turn on first use.
    pub(super) nested_engine: &'a mut Option<Arc<SessionEngine>>,
    pub(super) defer_steer: bool,
    pub(super) parent_session: Option<SessionId>,
    /// Tool schemas the round's request offered.
    pub(super) offered: &'a [ToolSchema],
}

#[derive(Default)]
pub(super) struct ToolState {
    pub(super) done: bool,
    report_latch: ReportLatch,
}

impl TurnToolGate<'_> {
    /// Run `tc` and record its outcome; an `Err` ends the turn.
    pub(super) async fn call(&mut self, mut tc: ToolCall) -> Result<(), CoreError> {
        let &mut Self {
            engine,
            session,
            message,
            binding,
            resources,
            agents,
            agent: live_agent,
            external_dirs,
            workspace,
            actor_claim,
            guidance,
            cancel,
            ref mut state,
            ref mut nested_engine,
            ref mut defer_steer,
            parent_session,
            offered,
        } = self;
        engine.validate_actor_claim(actor_claim).await?;
        let activation_hooks = activation_hook_for(session);
        let hook_context = ToolHookContext {
            session,
            message,
            call: tc.call,
            tool: &tc.name,
        };
        let tool_hooks = engine.active_hook_dispatcher(session);
        let has_hooks = tool_hooks.is_some();
        if let Some(hooks) = &tool_hooks {
            let input = apply_tool_execute_before_hooks(
                hooks,
                &hook_context,
                std::mem::take(&mut tc.input),
            )
            .await;
            if activation_hooks
                .as_ref()
                .is_some_and(|hooks| !hooks.is_healthy())
            {
                return Err(CoreError::Cancelled);
            }
            match input {
                Ok(input) => tc.input = input,
                Err(reason) => {
                    let message_text = format!("blocked by plugin: {reason}");
                    let value = tool_error_message_value("blocked", &message_text);
                    engine
                        .emit_for_actor(
                            actor_claim,
                            session,
                            tc.error_event(session, message, message_text, Some(value)),
                        )
                        .await?;
                    return Ok(());
                }
            }
        }
        let input_for_after = has_hooks.then(|| tc.input.clone());
        let started = std::time::Instant::now();
        let mut file_capture = FileCapture::default();
        let nested_stamps = Arc::new(Mutex::new(BTreeMap::new()));
        let behind = hidden_behind(offered, engine.exclusive_tool.as_deref(), tc.name.as_str());
        let wait_ack = hya_tool::WaitAckSlot::default();
        let mut ends_turn = false;
        let collect_tool = async {
            Ok::<_, CoreError>(match (behind, resources.resolve_tool(&tc.name)) {
                (Some(exclusive), Some(_)) => (
                    Err(ToolError::Other(format!(
                        "`{}` is not offered here: call it from a `{exclusive}` program",
                        tc.name
                    ))),
                    hya_tool::ToolResultPolicy::Default,
                ),
                (_, Some(resolved)) => {
                    let result_policy = resolved.tool.result_policy();
                    let permission = engine
                        .tool_call_permission(
                            binding,
                            session,
                            &live_agent.name,
                            external_dirs,
                            &workspace.grant_scope,
                            activation_hooks.as_ref(),
                        )
                        .await?;
                    let authorized = {
                        authorize_tool_call(&resolved, &tc.input, permission, message, tc.call)
                            .await
                    };
                    let result = match authorized {
                        Ok(permission) => {
                            let channel_policy = crate::channel_policy::from_binding(binding)
                                .map_err(CoreError::bundle)?
                                .snapshot_for(live_agent.name.as_str());
                            let mut extensions = hya_tool::ExtensionPlanes::new()
                                .with(
                                    engine
                                        .spawner
                                        .for_binding(binding)
                                        .for_session_with_agents_and_guidance(
                                            session,
                                            agents.clone(),
                                            guidance.clone(),
                                        ),
                                )
                                .with(
                                    engine
                                        .lifecycle
                                        .for_session_with_actor(session, actor_claim.copied())
                                        .with_channel_policy(channel_policy)
                                        .with_report_latch(state.report_latch.clone())
                                        .with_wait_ack(wait_ack.clone()),
                                )
                                .with(engine.project_activity.for_session(session))
                                .with(agents.clone());
                            if let Some(parent) = parent_session {
                                extensions.insert(hya_tool::ParentSession(parent));
                            }
                            if engine.lsp.is_configured() {
                                extensions.insert(engine.lsp.clone());
                            }
                            if engine.formatter.is_configured() {
                                extensions.insert(engine.formatter.clone());
                            }
                            let mut ctx = ToolCtx {
                                workdir: binding.workdir().to_path_buf(),
                                roots: workspace.roots.clone(),
                                cancel: cancel.clone(),
                                session: Some(session),
                                operation: hya_tool::ToolOperation::from_tool_call(tc.call)
                                    .with_actor_claim(actor_claim.copied()),
                                permission,
                                interaction: engine.interaction.for_session(session),
                                skills: resources.skill_plane(),
                                artifacts: engine.artifacts,
                                schemas: engine.schemas.clone(),
                                nested: hya_tool::NestedToolPlane::disconnected(),
                                extensions,
                            };
                            let visibility = engine
                                .tool_visibility_outcome(session, Some(&live_agent.name), resources)
                                .await;
                            let nested = super::super::nested_tools::TurnNestedTools {
                                engine: Arc::clone(
                                    nested_engine.get_or_insert_with(|| Arc::new(engine.clone())),
                                ),
                                binding: binding.clone(),
                                resources: Arc::clone(resources),
                                agent: live_agent.name.clone(),
                                session,
                                message,
                                actor_claim: actor_claim.copied(),
                                external_dirs: external_dirs.into(),
                                grant_scope: workspace.grant_scope.clone(),
                                activation_hooks: activation_hooks.clone(),
                                caller: resolved.tool.name().to_string(),
                                stamps: Arc::clone(&nested_stamps),
                                base: ctx.clone(),
                                visibility,
                            };
                            ctx.nested = hya_tool::NestedToolPlane::new(Arc::new(nested));
                            // Permission and plugin hooks can await. Recheck at the
                            // actual dispatch boundary so takeover cannot turn a
                            // previously valid resident into an unfenced launch.
                            engine.validate_actor_claim(actor_claim).await?;
                            file_capture = Box::pin(FileCapture::full(
                                resolved.tool.name(),
                                &tc.input,
                                binding.workdir(),
                            ))
                            .await;
                            engine.validate_actor_claim(actor_claim).await?;
                            let result = resolved
                                .tool
                                .execute(&ctx, std::mem::take(&mut tc.input))
                                .await;
                            ends_turn = result.is_ok() && resolved.tool.ends_turn();
                            result
                        }
                        Err(error) => Err(error),
                    };
                    (result, result_policy)
                }
                (_, None) => (
                    Err(ToolError::Other(format!("unknown tool: {}", tc.name))),
                    hya_tool::ToolResultPolicy::Default,
                ),
            })
        };
        let (result, result_policy) = tokio::select! {
            biased;
            () = cancel.cancelled() => (Err(ToolError::Cancelled), hya_tool::ToolResultPolicy::Default),
            result = collect_tool => result?,
        };
        let (changed, stamps) = Box::pin(file_capture.finish(
            &engine.store,
            session,
            message,
            tc.call,
            result.as_ref().ok(),
            (&engine.workspace_watches, binding.workdir()),
        ))
        .await?;
        if let Some(changed) = changed {
            engine.emit_for_actor(actor_claim, session, changed).await?;
        }
        let mut stamps_by_path = std::mem::take(&mut *nested_stamps.lock().await);
        stamps_by_path.extend(stamps.into_iter().map(|file| (file.path.clone(), file)));
        let mut artifact_guard =
            BashArtifactGuard::capture(tc.name.as_str(), result.as_ref().ok(), binding.workdir());
        let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        engine.validate_actor_claim(actor_claim).await?;
        let mut was_hook_replaced = false;
        let mut staged = Vec::new();
        let result =
            if !cancel.is_cancelled() && !matches!(&result, Err(ToolError::Cancelled)) && has_hooks
            {
                let was_permission_err = matches!(&result, Err(ToolError::Permission(_)));
                let mut native = match &result {
                    Ok(output) => NativeToolResult::Ok {
                        output: output.clone(),
                        time_ms,
                    },
                    Err(e) => NativeToolResult::Err {
                        message: e.to_string(),
                    },
                };
                let original_native = native.clone();
                engine.open_staged_events(session);
                engine.set_mail_steer_suppressed(
                    session,
                    state.report_latch.is_set() || *defer_steer || wait_ack.saw_wait(),
                );
                native = apply_tool_execute_after_hooks(
                    tool_hooks.as_deref().unwrap_or(&HookChain::default()),
                    &hook_context,
                    input_for_after.unwrap_or_default(),
                    native,
                )
                .await;
                engine.set_mail_steer_suppressed(session, false);
                staged = engine.take_staged_events(session);
                if activation_hooks
                    .as_ref()
                    .is_some_and(|hooks| !hooks.is_healthy())
                {
                    return Err(CoreError::Cancelled);
                }
                if was_permission_err || native == original_native {
                    result
                } else {
                    was_hook_replaced = true;
                    match native {
                        NativeToolResult::Ok { output, .. } => Ok(output),
                        NativeToolResult::Err { message } => Err(ToolError::Other(message)),
                    }
                }
            } else {
                result
            };
        let wait_state = wait_ack.take_state();
        let wait_ack = if wait_state.saw_wait {
            let intact = result.is_ok() && !was_hook_replaced && !cancel.is_cancelled();
            *defer_steer = !intact || wait_state.more_mail;
            wait_state.ack.filter(|_| intact)
        } else {
            None
        };
        let tool_succeeded = result.is_ok();
        state.done |= state.report_latch.is_set() || (ends_turn && tool_succeeded);
        let (event, retains_artifact) = match result {
            Ok(mut output) => {
                artifact_guard.remove_unowned_path(&mut output);
                if !artifact_guard.retained_by(&output) {
                    artifact_guard.discard()?;
                }
                // Cap every tool (builtin/MCP/plugin) after hooks and
                // immediately before durable Event publication. Whatever
                // the cap drops is preserved as an artifact first, so a
                // truncated result stays reachable without re-running the
                // tool that produced it.
                let mut output = hya_tool::cap_tool_output_spilling(
                    output,
                    result_policy,
                    &engine.artifacts.store(binding.workdir()),
                    tc.name.as_str(),
                );
                // Deadline notices ride the same result tail; unread-mail
                // steering ran through the `hya/agent-channels` NativeHook
                // during `apply_tool_execute_after_hooks`.
                if !state.done
                    && let Some(deadline) = hya_tool::deadline::current()
                {
                    if let Some(stage) = deadline.take_notice(session) {
                        let delivery = super::hooks::delivery_tool(
                            engine,
                            binding,
                            live_agent,
                            resources,
                            parent_session,
                            offered,
                        )?;
                        let notice = deadline.notice(stage, delivery);
                        append_steer_notice(&mut output, &format!("\n\n{notice}"));
                    } else if let Some(clock) = deadline.take_clock(session) {
                        append_steer_notice(&mut output, &format!("\n\n{clock}"));
                    }
                }
                let retains_artifact = artifact_guard.retained_by(&output);
                (
                    tc.result_event(session, message, output, time_ms),
                    retains_artifact,
                )
            }
            Err(e) => {
                artifact_guard.discard()?;
                (
                    tc.error_event(session, message, e.to_string(), Some(tool_error_value(&e))),
                    false,
                )
            }
        };
        // A wait ack takes precedence; staged events commit only with a
        // successful result, and dropped ones take their callbacks with them.
        let (extra, wait_mail, callbacks) = match wait_ack {
            Some(ack) => (
                vec![ack.event],
                Some((ack.mark, ack.consumed_incoming)),
                Vec::new(),
            ),
            None if tool_succeeded => {
                let (extra, callbacks) = staged
                    .into_iter()
                    .map(|staged| (staged.event, staged.on_commit))
                    .unzip();
                (extra, None, callbacks)
            }
            None => (Vec::new(), None, Vec::new()),
        };
        let mut events = vec![event];
        events.extend(observation(
            session,
            false,
            stamps_by_path.into_values().collect(),
        ));
        engine
            .emit_tool_result_with_staged(actor_claim, session, events, extra)
            .await?;
        if let Some((mark, consumed_incoming)) = wait_mail {
            engine.notify_mail_consumed(session, Some(mark), consumed_incoming);
        }
        callbacks
            .into_iter()
            .flatten()
            .for_each(|on_commit| on_commit());
        if retains_artifact {
            artifact_guard.disarm();
        } else {
            artifact_guard.discard()?;
        }
        Ok(())
    }
}

struct ToolHookContext<'a> {
    session: SessionId,
    message: MessageId,
    call: ToolCallId,
    tool: &'a str,
}

async fn apply_tool_execute_before_hooks(
    hooks: &HookChain,
    context: &ToolHookContext<'_>,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match hooks
        .tool_execute_before(NativeToolBeforeInput {
            session: context.session,
            message: context.message,
            call: context.call,
            tool: context.tool.to_string(),
            input,
        })
        .await
    {
        NativeToolBeforeOutcome::Continue { input } => Ok(input),
        NativeToolBeforeOutcome::Veto { reason } => Err(reason),
    }
}

async fn apply_tool_execute_after_hooks(
    hooks: &HookChain,
    context: &ToolHookContext<'_>,
    input: serde_json::Value,
    result: NativeToolResult,
) -> NativeToolResult {
    hooks
        .tool_execute_after(NativeToolAfterInput {
            session: context.session,
            message: context.message,
            call: context.call,
            tool: context.tool.to_string(),
            input,
            result,
        })
        .await
}

/// Splice a steer notice into a tool result's model-facing text.
fn append_steer_notice(output: &mut serde_json::Value, notice: &str) {
    let serde_json::Value::Object(object) = output else {
        *output = serde_json::json!({
            "result": std::mem::take(output),
            "steer": notice.trim_start(),
        });
        return;
    };
    let (key, updated) = match (object.get("output"), object.get("steer")) {
        (Some(serde_json::Value::String(text)), _) => ("output", format!("{text}{notice}")),
        (_, Some(serde_json::Value::String(text))) => ("steer", format!("{text}{notice}")),
        _ => ("steer", notice.trim_start().to_string()),
    };
    object.insert(key.to_string(), serde_json::Value::String(updated));
}

#[cfg(test)]
#[path = "tool_gate_tests.rs"]
mod tests;
