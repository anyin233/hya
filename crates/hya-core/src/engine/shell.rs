use hya_proto::{Event, FinishReason, MessageId, PartId, Role, SessionId, ToolCallId, ToolName};
use hya_tool::{ToolCtx, ToolError};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::tool_error::{tool_error_message_value, tool_error_value};
use super::{
    AgentSpec, SessionEngine, agent_roster, authorize_tool_call, effective_agent_for_binding,
    session_workdir,
};
use crate::TurnBinding;
use crate::error::CoreError;
use crate::hooks::{ToolExecuteBeforeInput, ToolExecuteBeforeOutcome};
use crate::runtime_registry::CompiledResourceView;

mod admission;
mod hooks;

use hooks::{AfterHookCall, apply_tool_after_hooks};

struct ShellPart {
    session: SessionId,
    message: MessageId,
    part: PartId,
    call: ToolCallId,
    name: ToolName,
}

impl SessionEngine {
    /// Run a shell command as a session operation with hooks and permissions.
    pub async fn run_shell(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        command: String,
        cancel: CancellationToken,
    ) -> Result<(MessageId, FinishReason), CoreError> {
        self.admit_shell_user_message(session).await?;
        let projection = self.store.read_projection(session).await?;
        let workdir = session_workdir(agent, &projection);
        let binding = self.bind_root_runtime(&workdir).await?;
        let stable_id = projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&agent.name)
            .as_str();
        // Shell turns do not attach project/reference guidance.
        let (agent, resources) = effective_agent_for_binding(agent, stable_id, &binding, None)?;

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
        self.emit(
            session,
            Event::TurnBindingRecorded {
                session,
                message,
                generation: binding.generation(),
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
                ShellPart {
                    session,
                    message,
                    part,
                    call,
                    name,
                },
                command,
                &binding,
                &agent,
                &resources,
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
        shell_part: ShellPart,
        command: String,
        binding: &TurnBinding,
        agent: &AgentSpec,
        resources: &CompiledResourceView,
        cancel: CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        let session = shell_part.session;
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
        let input_for_after = self.hooks.as_ref().map(|_| input.clone());
        let started = std::time::Instant::now();
        let result = match resources.resolve_tool(&tool) {
            Some(resolved) => match authorize_tool_call(
                &resolved,
                &input,
                self.permission.for_session(session),
                shell_part.message,
                shell_part.call,
            )
            .await
            {
                Ok(permission) => {
                    let ctx = ToolCtx {
                        permission,
                        interaction: self.interaction.for_session(session),
                        spawner: self.spawner.for_binding(binding).for_session_with_agents(
                            session,
                            agent_roster(binding, agent.name.as_str())?,
                        ),
                        operation: hya_tool::ToolOperation::from_tool_call(shell_part.call),
                        mailbox: self.mailbox.for_session(session),
                        session: Some(session),
                        parent_session: projection.session.parent,
                        todo: self.todo.clone(),
                        skills: resources.skill_plane(),
                        agents: agent_roster(binding, agent.name.as_str())?,
                        websearch: self.websearch.clone(),
                        lsp: self.lsp.clone(),
                        formatter: self.formatter.clone(),
                        workdir: binding.workdir().to_path_buf(),
                        cancel,
                    };
                    resolved.tool.execute(&ctx, input).await
                }
                Err(error) => Err(error),
            },
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
