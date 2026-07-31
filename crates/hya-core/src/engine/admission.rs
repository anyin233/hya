use hya_proto::{Event, FinishReason, MessageId, OperationId, PartId, Role, SessionId, ToolCallId};
use hya_store::{
    AdmissionClaim, AdmissionClaimOutcome, AdmissionStartOutcome, AdmissionState, AdmissionTerminal,
};
use tokio_util::sync::CancellationToken;

use super::SessionEngine;
use crate::error::CoreError;
use crate::hooks::{
    CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, MessageUserBeforeInput,
    MessageUserBeforeOutcome,
};
use crate::orchestrator::OperationReservation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpawnAdmissionOutcome {
    Started,
    Existing(AdmissionState),
    Overloaded,
    MaxDepth,
    Cancelled,
}

impl SessionEngine {
    pub async fn begin_spawn_admission(
        &self,
        parent: SessionId,
        source_tool_call_id: ToolCallId,
        operation_id: OperationId,
        request_fingerprint: [u8; 32],
        admission_units: u32,
        cancel: CancellationToken,
    ) -> Result<SpawnAdmissionOutcome, CoreError> {
        if operation_id != OperationId::from_tool_call(source_tool_call_id) {
            return Err(CoreError::Invalid(
                "operation id does not match source tool call".to_string(),
            ));
        }
        let (root, depth) = self.session_lineage(parent).await?;
        let claim = AdmissionClaim {
            operation_id,
            source_tool_call_id,
            root_session: root,
            request_fingerprint,
            admission_units,
        };
        match self.store.claim_admission(&claim).await? {
            AdmissionClaimOutcome::Existing(record) => {
                return Ok(SpawnAdmissionOutcome::Existing(record.state));
            }
            AdmissionClaimOutcome::Claimed(_) => {}
        }

        if cancel.is_cancelled() {
            self.finalize_spawn_admission(
                operation_id,
                AdmissionTerminal::Cancelled,
                "cancelled before debit",
            )
            .await?;
            return Ok(SpawnAdmissionOutcome::Cancelled);
        }
        if let Some(governor) = &self.governor {
            if depth.saturating_add(1) > governor.max_depth() {
                self.finalize_spawn_admission(
                    operation_id,
                    AdmissionTerminal::Aborted,
                    "maximum subagent depth exceeded",
                )
                .await?;
                return Ok(SpawnAdmissionOutcome::MaxDepth);
            }
            match governor.try_reserve_operation(
                root,
                operation_id,
                u64::from(admission_units),
                cancel,
            ) {
                OperationReservation::Overloaded => {
                    self.finalize_spawn_admission(
                        operation_id,
                        AdmissionTerminal::Aborted,
                        "spawn admission overloaded",
                    )
                    .await?;
                    return Ok(SpawnAdmissionOutcome::Overloaded);
                }
                OperationReservation::Existing | OperationReservation::Conflict => {
                    return Ok(SpawnAdmissionOutcome::Existing(AdmissionState::Accepted));
                }
                OperationReservation::Acquired => {}
            }
        }

        match self.store.start_admission(operation_id).await {
            Ok(AdmissionStartOutcome::Started(_)) => Ok(SpawnAdmissionOutcome::Started),
            Ok(AdmissionStartOutcome::Existing(record)) => {
                if let Some(governor) = &self.governor {
                    governor.release_operation(operation_id);
                }
                Ok(SpawnAdmissionOutcome::Existing(record.state))
            }
            Err(error) => {
                if let Some(governor) = &self.governor {
                    governor.release_operation(operation_id);
                }
                let _ = self
                    .store
                    .finalize_admission(
                        operation_id,
                        AdmissionTerminal::Aborted,
                        "failed to persist started state",
                    )
                    .await;
                Err(error.into())
            }
        }
    }

    pub async fn finalize_spawn_admission(
        &self,
        operation_id: OperationId,
        terminal: AdmissionTerminal,
        reason: &str,
    ) -> Result<(), CoreError> {
        let outcome = self
            .store
            .finalize_admission(operation_id, terminal, reason)
            .await?;
        if outcome.release_required
            && let Some(governor) = &self.governor
        {
            governor.release_operation(operation_id);
        }
        Ok(())
    }

    pub async fn finalize_root_spawn_admissions(&self, root: SessionId) -> Result<(), CoreError> {
        if let Some(governor) = &self.governor {
            governor.cancel_operations(root);
        }
        for record in self.store.nonterminal_admissions_for_root(root).await? {
            self.finalize_spawn_admission(
                record.operation_id,
                AdmissionTerminal::Cancelled,
                "root turn cleanup",
            )
            .await?;
        }
        if let Some(governor) = &self.governor {
            governor.release(root);
        }
        Ok(())
    }

    pub async fn inject_system_message(
        &self,
        session: SessionId,
        content: String,
    ) -> Result<MessageId, CoreError> {
        let message = MessageId::new();
        let part = PartId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::System,
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
                delta: content,
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
            Event::MessageFinished {
                session,
                message,
                role: Role::System,
                finish: FinishReason::Stop,
                tokens: None,
            },
        )
        .await?;
        Ok(message)
    }

    pub async fn admit_user_prompt(
        &self,
        session: SessionId,
        text: String,
    ) -> Result<MessageId, CoreError> {
        self.admit_user_prompt_with_id(session, MessageId::new(), text)
            .await
    }

    pub async fn admit_user_prompt_with_id(
        &self,
        session: SessionId,
        message: MessageId,
        text: String,
    ) -> Result<MessageId, CoreError> {
        let text = if let Some(hooks) = &self.hooks {
            match hooks
                .message_user_before(MessageUserBeforeInput { session, text })
                .await
            {
                MessageUserBeforeOutcome::Continue { text } => text,
            }
        } else {
            text
        };
        let part = PartId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::User,
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
                delta: text,
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
            Event::MessageFinished {
                session,
                message,
                role: Role::User,
                finish: FinishReason::Stop,
                tokens: None,
            },
        )
        .await?;
        Ok(message)
    }

    pub async fn record_user_prompt_context(
        &self,
        session: SessionId,
        message: MessageId,
        files: Vec<serde_json::Value>,
        agents: Vec<serde_json::Value>,
    ) -> Result<(), CoreError> {
        if files.is_empty() && agents.is_empty() {
            return Ok(());
        }
        self.emit(
            session,
            Event::UserPromptContextRecorded {
                session,
                message,
                files,
                agents,
            },
        )
        .await
    }

    pub async fn admit_command_prompt(
        &self,
        session: SessionId,
        command: String,
        arguments: String,
        text: String,
    ) -> Result<MessageId, CoreError> {
        self.admit_command_prompt_with_id(session, MessageId::new(), command, arguments, text)
            .await
    }

    pub async fn admit_command_prompt_with_id(
        &self,
        session: SessionId,
        message: MessageId,
        command: String,
        arguments: String,
        text: String,
    ) -> Result<MessageId, CoreError> {
        let text = if let Some(hooks) = &self.hooks {
            match hooks
                .command_execute_before(CommandExecuteBeforeInput {
                    session,
                    command: command.clone(),
                    arguments: arguments.clone(),
                    text,
                })
                .await
            {
                CommandExecuteBeforeOutcome::Continue { text } => text,
            }
        } else {
            text
        };
        let message = self
            .admit_user_prompt_with_id(session, message, text)
            .await?;
        self.emit(
            session,
            Event::CommandExecuted {
                session,
                command,
                arguments,
                message,
            },
        )
        .await?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use hya_proto::{AgentName, ModelRef};
    use hya_provider::ProviderRouter;
    use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

    use super::*;
    use crate::{CreateSession, EventBus, SubagentGovernor, SubagentLimits};

    #[tokio::test]
    async fn root_cleanup_cancels_and_finalizes_started_operation_once() {
        let store = hya_store::SessionStore::connect_memory().await.unwrap();
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        let governor = SubagentGovernor::new(SubagentLimits {
            per_run_budget: 2,
            ..SubagentLimits::default()
        });
        let engine = SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new()),
            Arc::new(ToolRegistry::builtins()),
            permission,
            EventBus::default(),
        )
        .with_governor(governor.clone());
        let root = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/tmp".to_string(),
            })
            .await
            .unwrap();
        let source = ToolCallId::new();
        let operation = OperationId::from_tool_call(source);
        let cancel = CancellationToken::new();
        assert_eq!(
            engine
                .begin_spawn_admission(root, source, operation, [23; 32], 1, cancel.clone())
                .await
                .unwrap(),
            SpawnAdmissionOutcome::Started
        );
        assert_eq!(governor.remaining_budget(root), 1);

        engine.finalize_root_spawn_admissions(root).await.unwrap();
        engine.finalize_root_spawn_admissions(root).await.unwrap();

        assert!(cancel.is_cancelled());
        let record = store.admission(operation).await.unwrap().unwrap();
        assert_eq!(record.state, AdmissionState::Cancelled);
        assert!(record.logical_released);
        assert_eq!(governor.remaining_budget(root), 2);
    }

    #[tokio::test]
    async fn cancelled_before_debit_terminalizes_without_release_or_budget_change() {
        let store = hya_store::SessionStore::connect_memory().await.unwrap();
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        let governor = SubagentGovernor::new(SubagentLimits {
            per_run_budget: 1,
            ..SubagentLimits::default()
        });
        let engine = SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new()),
            Arc::new(ToolRegistry::builtins()),
            permission,
            EventBus::default(),
        )
        .with_governor(governor.clone());
        let root = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/tmp".to_string(),
            })
            .await
            .unwrap();
        let source = ToolCallId::new();
        let operation = OperationId::from_tool_call(source);
        let cancel = CancellationToken::new();
        cancel.cancel();

        assert_eq!(
            engine
                .begin_spawn_admission(root, source, operation, [24; 32], 1, cancel)
                .await
                .unwrap(),
            SpawnAdmissionOutcome::Cancelled
        );
        let record = store.admission(operation).await.unwrap().unwrap();
        assert_eq!(record.state, AdmissionState::Cancelled);
        assert!(!record.logical_released);
        assert_eq!(governor.remaining_budget(root), 1);
    }
}
