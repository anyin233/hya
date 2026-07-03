use hya_proto::{Event, FinishReason, MessageId, PartId, Role, SessionId, ToolCallId, ToolName};
use hya_tool::{ToolCtx, ToolError};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::tool_error::{tool_error_message_value, tool_error_value};
use super::{AgentSpec, SessionEngine, session_workdir};
use crate::error::CoreError;
use crate::hooks::{ToolExecuteBeforeInput, ToolExecuteBeforeOutcome};

mod admission;
mod hooks;

use hooks::{AfterHookCall, apply_tool_after_hooks};

struct ShellPart {
    message: MessageId,
    part: PartId,
    call: ToolCallId,
    name: ToolName,
}

impl SessionEngine {
    pub async fn run_shell(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        command: String,
        cancel: CancellationToken,
    ) -> Result<(MessageId, FinishReason), CoreError> {
        self.admit_shell_user_message(session).await?;

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

        let part = PartId::new();
        let call = ToolCallId::new();
        let name = ToolName::new("shell");
        self.emit(
            session,
            Event::ToolInputStart {
                session,
                message,
                part,
                call,
                name: name.clone(),
            },
        )
        .await?;

        let finish = self
            .execute_shell_part(
                session,
                ShellPart {
                    message,
                    part,
                    call,
                    name,
                },
                command,
                agent,
                cancel,
            )
            .await?;
        self.emit(
            session,
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish,
                tokens: None,
            },
        )
        .await?;
        Ok((message, finish))
    }

    async fn execute_shell_part(
        &self,
        session: SessionId,
        shell_part: ShellPart,
        command: String,
        agent: &AgentSpec,
        cancel: CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        let tool = shell_part.name.to_string();
        let mut input = json!({ "command": command });
        if let Some(hooks) = &self.hooks {
            let current = std::mem::take(&mut input);
            match hooks
                .tool_execute_before(ToolExecuteBeforeInput {
                    session,
                    message: shell_part.message,
                    call: shell_part.call,
                    tool: tool.clone(),
                    input: current,
                })
                .await
            {
                ToolExecuteBeforeOutcome::Continue { input: next } => input = next,
                ToolExecuteBeforeOutcome::Veto { reason } => {
                    let message_text = format!("blocked by plugin: {reason}");
                    self.emit(
                        session,
                        Event::ToolError {
                            session,
                            message: shell_part.message,
                            part: shell_part.part,
                            call: shell_part.call,
                            value: Some(tool_error_message_value("blocked", &message_text)),
                            message_text,
                        },
                    )
                    .await?;
                    return Ok(FinishReason::Error);
                }
            }
        }

        self.emit(
            session,
            Event::ToolCallRequested {
                session,
                message: shell_part.message,
                part: shell_part.part,
                call: shell_part.call,
                name: shell_part.name,
                input: input.clone(),
            },
        )
        .await?;

        let projection = self.store.read_projection(session).await?;
        let workdir = session_workdir(agent, &projection);
        let input_for_after = self.hooks.as_ref().map(|_| input.clone());
        let started = std::time::Instant::now();
        let result = match self.tools.get(&tool) {
            Some(shell) => {
                let ctx = ToolCtx {
                    permission: self.permission.for_session(session),
                    interaction: self.interaction.for_session(session),
                    spawner: self.spawner.for_session(session),
                    session: Some(session),
                    parent_session: projection.session.parent,
                    todo: self.todo.clone(),
                    skills: self.skills.clone(),
                    agents: self.agents.clone(),
                    websearch: self.websearch.clone(),
                    lsp: self.lsp.clone(),
                    formatter: self.formatter.clone(),
                    workdir,
                    cancel,
                };
                shell.execute(&ctx, input).await
            }
            None => Err(ToolError::Other("unknown tool: shell".to_string())),
        };
        let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let result = apply_tool_after_hooks(
            self,
            result,
            AfterHookCall {
                session,
                message: shell_part.message,
                call: shell_part.call,
                tool: &tool,
                input: input_for_after,
                time_ms,
            },
        )
        .await;

        match result {
            Ok(output) => {
                self.emit(
                    session,
                    Event::ToolResult {
                        session,
                        message: shell_part.message,
                        part: shell_part.part,
                        call: shell_part.call,
                        output,
                        time_ms,
                    },
                )
                .await?;
                Ok(FinishReason::Stop)
            }
            Err(error) => {
                let finish = finish_from_tool_error(&error);
                self.emit(
                    session,
                    Event::ToolError {
                        session,
                        message: shell_part.message,
                        part: shell_part.part,
                        call: shell_part.call,
                        value: Some(tool_error_value(&error)),
                        message_text: error.to_string(),
                    },
                )
                .await?;
                Ok(finish)
            }
        }
    }
}

fn finish_from_tool_error(error: &ToolError) -> FinishReason {
    if matches!(error, ToolError::Cancelled) {
        FinishReason::Cancelled
    } else {
        FinishReason::Error
    }
}
