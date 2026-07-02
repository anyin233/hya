use std::path::PathBuf;

use hya_proto::{Event, FinishReason, MessageId, PartId, Role, SessionId, TokenUsage};
use hya_tool::{Action, Mode, PermissionPlane, Rule, ToolCtx, ToolError};
use tokio_util::sync::CancellationToken;

use super::tool_error::{tool_error_message_value, tool_error_value};
use super::{AgentSpec, SessionEngine, effective_agent_for_projection};
use crate::error::CoreError;
use crate::hooks::{
    ChatParamsInput, ChatParamsOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome,
    ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, ToolOutcomeNative,
};

mod messages;

use messages::{projection_to_messages, request_from_messages};

impl SessionEngine {
    pub async fn run_turn(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        cancel: CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs(session, agent, cancel, &[])
            .await
    }

    pub async fn run_turn_with_external_dirs(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        cancel: CancellationToken,
        external_dirs: &[PathBuf],
    ) -> Result<FinishReason, CoreError> {
        let message = MessageId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::Assistant,
            },
        )
        .await?;

        let outcome = self
            .run_turn_rounds(session, message, agent, &cancel, external_dirs)
            .await;
        if outcome.is_err() {
            // A provider/tool error after MessageStarted must still close the assistant
            // message, else UI clients (e.g. the hya TUI) wait forever for a finish event.
            let _ = self
                .emit(
                    session,
                    Event::MessageFinished {
                        session,
                        message,
                        role: Role::Assistant,
                        finish: FinishReason::Error,
                        tokens: None,
                    },
                )
                .await;
        }
        // A completed top-level (depth-0) turn ends the "run": release its per-run
        // subagent budget so long-lived root sessions do not leak budget entries and
        // the next top-level turn starts with a fresh budget.
        if let Some(gov) = &self.governor
            && let Ok((root, 0)) = self.session_lineage(session).await
        {
            gov.release(root);
        }
        outcome
    }

    async fn run_turn_rounds(
        &self,
        session: SessionId,
        message: MessageId,
        agent: &AgentSpec,
        cancel: &CancellationToken,
        external_dirs: &[PathBuf],
    ) -> Result<FinishReason, CoreError> {
        const MAX_TOOL_ROUNDS: u32 = 25;
        let mut rounds: u32 = 0;
        let mut total_tokens = None;
        // Depth in the subagent tree, derived from the parent chain. Only subagents
        // (depth > 0) are subject to the streaming-concurrency semaphore; the
        // interactive lead (depth 0) never waits behind background subagents.
        let depth = match &self.governor {
            Some(_) => self
                .session_lineage(session)
                .await
                .map(|(_, d)| d)
                .unwrap_or(0),
            None => 0,
        };
        loop {
            if cancel.is_cancelled() {
                self.emit(
                    session,
                    Event::MessageFinished {
                        session,
                        message,
                        role: Role::Assistant,
                        finish: FinishReason::Cancelled,
                        tokens: None,
                    },
                )
                .await?;
                return Ok(FinishReason::Cancelled);
            }

            let projection = self.store.read_projection(session).await?;
            let agent = effective_agent_for_projection(agent, &projection);
            let messages = projection_to_messages(&agent, &projection);
            let messages = if let Some(summarizer) = &self.summarizer {
                match crate::compaction::compact_with(
                    messages,
                    &self.compaction,
                    summarizer.as_ref(),
                )
                .await
                {
                    Ok(compacted) => compacted,
                    Err(_) => projection_to_messages(&agent, &projection),
                }
            } else {
                messages
            };
            let request =
                request_from_messages(&agent, &projection, messages, &self.tools, &self.providers);
            let request = if let Some(hooks) = &self.hooks {
                match hooks
                    .chat_params(ChatParamsInput {
                        session,
                        message,
                        request,
                    })
                    .await
                {
                    ChatParamsOutcome::Continue { request } => request,
                }
            } else {
                request
            };
            // Hold a global streaming permit ONLY around provider streaming, and only
            // for subagents. Acquired here and dropped before tool execution, so a
            // member blocked in the `task` tool (awaiting its children) holds no
            // permit — guaranteeing nested spawns can always make progress.
            let stream_permit = match (depth > 0, &self.governor) {
                (true, Some(gov)) => gov.acquire_stream().await,
                _ => None,
            };
            let stream = self.providers.stream(request, session, message).await?;
            let step = rounds;
            self.emit(
                session,
                Event::StepStarted {
                    session,
                    message,
                    step,
                },
            )
            .await?;
            let stream_round = self.collect_stream_round(session, message, stream).await?;
            add_tokens(&mut total_tokens, stream_round.tokens);
            self.emit(
                session,
                Event::StepFinished {
                    session,
                    message,
                    step,
                    finish: stream_round.finish,
                },
            )
            .await?;
            // Release the streaming slot before running tools (which may spawn and
            // await child subagents that need permits of their own).
            drop(stream_permit);

            if stream_round.tool_calls.is_empty() {
                self.emit(
                    session,
                    Event::MessageFinished {
                        session,
                        message,
                        role: Role::Assistant,
                        finish: stream_round.finish,
                        tokens: total_tokens,
                    },
                )
                .await?;
                return Ok(stream_round.finish);
            }

            for mut tc in stream_round.tool_calls {
                if let Some(hooks) = &self.hooks {
                    let input = std::mem::take(&mut tc.input);
                    match hooks
                        .tool_execute_before(ToolExecuteBeforeInput {
                            session,
                            message,
                            call: tc.call,
                            tool: tc.name.clone(),
                            input,
                        })
                        .await
                    {
                        ToolExecuteBeforeOutcome::Continue { input } => tc.input = input,
                        ToolExecuteBeforeOutcome::Veto { reason } => {
                            let message_text = format!("blocked by plugin: {reason}");
                            self.emit(
                                session,
                                Event::ToolError {
                                    session,
                                    message,
                                    part: tc.part,
                                    call: tc.call,
                                    value: Some(tool_error_message_value("blocked", &message_text)),
                                    message_text,
                                },
                            )
                            .await?;
                            continue;
                        }
                    }
                }
                let input_for_after = self.hooks.as_ref().map(|_| tc.input.clone());
                let started = std::time::Instant::now();
                let result = match self.tools.get(&tc.name) {
                    Some(tool) => {
                        let ctx = ToolCtx {
                            permission: permission_for_session(
                                &self.permission,
                                session,
                                external_dirs,
                            ),
                            interaction: self.interaction.for_session(session),
                            spawner: self.spawner.for_session(session),
                            session: Some(session),
                            parent_session: projection.session.parent,
                            todo: self.todo.clone(),
                            skills: self.skills.clone(),
                            websearch: self.websearch.clone(),
                            lsp: self.lsp.clone(),
                            formatter: self.formatter.clone(),
                            workdir: agent.workdir.clone(),
                            cancel: cancel.clone(),
                        };
                        tool.execute(&ctx, tc.input).await
                    }
                    None => Err(ToolError::Other(format!("unknown tool: {}", tc.name))),
                };
                let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let result = if let Some(hooks) = &self.hooks {
                    let was_permission_err = matches!(&result, Err(ToolError::Permission(_)));
                    let native = match &result {
                        Ok(output) => ToolOutcomeNative::Ok {
                            output: output.clone(),
                            time_ms,
                        },
                        Err(e) => ToolOutcomeNative::Err {
                            message: e.to_string(),
                        },
                    };
                    let ToolExecuteAfterOutcome::Continue { result: rewritten } = hooks
                        .tool_execute_after(ToolExecuteAfterInput {
                            session,
                            message,
                            call: tc.call,
                            tool: tc.name.clone(),
                            input: input_for_after.unwrap_or_default(),
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
                } else {
                    result
                };
                let event = match result {
                    Ok(output) => Event::ToolResult {
                        session,
                        message,
                        part: tc.part,
                        call: tc.call,
                        output,
                        time_ms,
                    },
                    Err(e) => Event::ToolError {
                        session,
                        message,
                        part: tc.part,
                        call: tc.call,
                        value: Some(tool_error_value(&e)),
                        message_text: e.to_string(),
                    },
                };
                self.emit(session, event).await?;
            }

            rounds += 1;
            if rounds >= MAX_TOOL_ROUNDS {
                let part = PartId::new();
                self.emit(
                    session,
                    Event::StepStarted {
                        session,
                        message,
                        step: rounds,
                    },
                )
                .await?;
                self.emit(
                    session,
                    Event::TextStart {
                        session,
                        message,
                        part,
                    },
                )
                .await?;
                self.emit(
                    session,
                    Event::TextDelta {
                        session,
                        message,
                        part,
                        delta: format!("[stopped: reached the {MAX_TOOL_ROUNDS}-tool-call limit]"),
                    },
                )
                .await?;
                self.emit(
                    session,
                    Event::TextEnd {
                        session,
                        message,
                        part,
                    },
                )
                .await?;
                self.emit(
                    session,
                    Event::StepFinished {
                        session,
                        message,
                        step: rounds,
                        finish: FinishReason::Error,
                    },
                )
                .await?;
                self.emit(
                    session,
                    Event::MessageFinished {
                        session,
                        message,
                        role: Role::Assistant,
                        finish: FinishReason::Error,
                        tokens: total_tokens,
                    },
                )
                .await?;
                return Ok(FinishReason::Error);
            }
        }
    }
}

fn add_tokens(target: &mut Option<TokenUsage>, update: Option<TokenUsage>) {
    if let Some(update) = update {
        let current = target.get_or_insert_with(TokenUsage::default);
        current.input = current.input.saturating_add(update.input);
        current.output = current.output.saturating_add(update.output);
        current.reasoning = current.reasoning.saturating_add(update.reasoning);
        current.cache_read = current.cache_read.saturating_add(update.cache_read);
        current.cache_write = current.cache_write.saturating_add(update.cache_write);
    }
}

fn permission_for_session(
    permission: &PermissionPlane,
    session: SessionId,
    external_dirs: &[PathBuf],
) -> PermissionPlane {
    let permission = permission.for_session(session);
    let rules = external_dirs
        .iter()
        .map(|dir| {
            Rule::new(
                Action::ExternalDirectory,
                dir.join("*").to_string_lossy().replace('\\', "/"),
                Mode::Allow,
            )
        })
        .collect();
    permission.with_snapshot_rules(rules)
}
