use hya_proto::{Event, FinishReason, MessageId, OperationId, PartId, Role, SessionId};
use hya_store::{
    ActorClaim, AdmissionClaim, AdmissionClaimOutcome, AdmissionStartOutcome, AdmissionState,
    AdmissionTerminal, RecoveredActorClaim,
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
        operation: hya_tool::ToolOperation,
        request_fingerprint: [u8; 32],
        admission_units: u32,
        actor_claim: Option<ActorClaim>,
        cancel: CancellationToken,
    ) -> Result<SpawnAdmissionOutcome, CoreError> {
        let operation_id = operation.operation_id();
        let source_tool_call_id = operation.source_tool_call_id();
        let (root, depth) = self.session_lineage(parent).await?;
        let claim = AdmissionClaim {
            operation_id,
            source_tool_call_id,
            root_session: root,
            request_fingerprint,
            admission_units,
            actor_claim,
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
                actor_claim.as_ref(),
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
                    actor_claim.as_ref(),
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
                        actor_claim.as_ref(),
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

        match self
            .store
            .start_admission(operation_id, actor_claim.as_ref())
            .await
        {
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
                        actor_claim.as_ref(),
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
        actor_claim: Option<&ActorClaim>,
    ) -> Result<(), CoreError> {
        let outcome = self
            .store
            .finalize_admission(operation_id, terminal, reason, actor_claim)
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
                None,
            )
            .await?;
        }
        if let Some(governor) = &self.governor {
            governor.release(root);
        }
        Ok(())
    }

    pub async fn abort_recovered_actor_operations(
        &self,
        recovered: &RecoveredActorClaim,
    ) -> Result<usize, CoreError> {
        let records = self
            .store
            .abort_recovered_actor_admissions(recovered, "resident actor takeover")
            .await?;
        if let Some(governor) = &self.governor {
            for record in &records {
                if record.logical_released {
                    governor.release_operation(record.operation_id);
                }
            }
        }
        Ok(records.len())
    }

    pub(crate) async fn recover_resident_actor_durable(
        &self,
        recovered: &RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<(hya_store::RecoveredResidentWork, usize), CoreError> {
        let outcome = self
            .store
            .recover_resident_actor(recovered, root, handle)
            .await?;
        for envelope in outcome.envelopes {
            self.publish_envelope(envelope);
        }
        if let Some(governor) = &self.governor {
            for record in &outcome.admissions {
                if record.logical_released {
                    governor.release_operation(record.operation_id);
                }
            }
        }
        let aborted_operations = outcome.admissions.len();
        Ok((outcome.work, aborted_operations))
    }

    pub(crate) async fn release_resident_actor_claim(
        &self,
        claim: &ActorClaim,
    ) -> Result<(), CoreError> {
        let records = self.store.release_claim(claim).await?;
        if let Some(governor) = &self.governor {
            for record in records {
                if record.logical_released {
                    governor.release_operation(record.operation_id);
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn finalize_resident_stop(
        &self,
        claim: &ActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<(), CoreError> {
        self.finalize_resident_failure(claim, root, handle, "resident stopped")
            .await
    }

    pub(crate) async fn finalize_resident_failure(
        &self,
        claim: &ActorClaim,
        root: SessionId,
        handle: &str,
        reason: &str,
    ) -> Result<(), CoreError> {
        let (envelopes, admissions) = self
            .store
            .finalize_resident_failure(claim, root, handle, reason)
            .await?;
        for envelope in envelopes {
            self.publish_envelope(envelope);
        }
        if let Some(governor) = &self.governor {
            for record in admissions {
                if record.logical_released {
                    governor.release_operation(record.operation_id);
                }
            }
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

    pub(crate) async fn inject_system_message_for_actor(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        content: String,
    ) -> Result<MessageId, CoreError> {
        let message = MessageId::new();
        let part = PartId::new();
        self.commit_resident_mutation(
            claim,
            session,
            vec![
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::System,
                },
                Event::TextStart {
                    session,
                    message,
                    part,
                },
                Event::TextDelta {
                    session,
                    message,
                    part,
                    delta: content,
                },
                Event::TextEnd {
                    session,
                    message,
                    part,
                },
                Event::MessageFinished {
                    session,
                    message,
                    role: Role::System,
                    finish: FinishReason::Stop,
                    tokens: None,
                },
            ],
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

    pub(crate) async fn admit_user_prompt_for_actor(
        &self,
        claim: &ActorClaim,
        session: SessionId,
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
        let message = MessageId::new();
        let part = PartId::new();
        self.commit_resident_mutation(
            claim,
            session,
            vec![
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::User,
                },
                Event::TextStart {
                    session,
                    message,
                    part,
                },
                Event::TextDelta {
                    session,
                    message,
                    part,
                    delta: text,
                },
                Event::TextEnd {
                    session,
                    message,
                    part,
                },
                Event::MessageFinished {
                    session,
                    message,
                    role: Role::User,
                    finish: FinishReason::Stop,
                    tokens: None,
                },
            ],
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

    use hya_proto::{AgentName, ModelRef, OwnerRunId, ToolCallId};
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
            crate::test_support::runtime(ToolRegistry::builtins()),
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
                .begin_spawn_admission(
                    root,
                    hya_tool::ToolOperation::from_tool_call(source),
                    [23; 32],
                    1,
                    None,
                    cancel.clone(),
                )
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
            crate::test_support::runtime(ToolRegistry::builtins()),
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
                .begin_spawn_admission(
                    root,
                    hya_tool::ToolOperation::from_tool_call(source),
                    [24; 32],
                    1,
                    None,
                    cancel,
                )
                .await
                .unwrap(),
            SpawnAdmissionOutcome::Cancelled
        );
        let record = store.admission(operation).await.unwrap().unwrap();
        assert_eq!(record.state, AdmissionState::Cancelled);
        assert!(!record.logical_released);
        assert_eq!(governor.remaining_budget(root), 1);
    }

    #[tokio::test]
    async fn actor_release_aborts_and_refunds_bound_operation_exactly_once() {
        let store = hya_store::SessionStore::connect_memory().await.unwrap();
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        let governor = SubagentGovernor::new(SubagentLimits {
            per_run_budget: 1,
            ..SubagentLimits::default()
        });
        let engine = SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new()),
            crate::test_support::runtime(ToolRegistry::builtins()),
            permission,
            EventBus::default(),
        )
        .with_governor(governor.clone());
        let actor = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("resident"),
                model: ModelRef::new("fake"),
                workdir: "/tmp".to_string(),
            })
            .await
            .unwrap();
        let claim = store.try_claim_new(actor, OwnerRunId::new()).await.unwrap();
        let source = ToolCallId::new();
        let operation = OperationId::from_tool_call(source);
        assert_eq!(
            engine
                .begin_spawn_admission(
                    actor,
                    hya_tool::ToolOperation::from_tool_call(source),
                    [25; 32],
                    1,
                    Some(claim),
                    CancellationToken::new(),
                )
                .await
                .unwrap(),
            SpawnAdmissionOutcome::Started
        );
        assert_eq!(governor.remaining_budget(actor), 0);

        engine.release_resident_actor_claim(&claim).await.unwrap();
        engine.release_resident_actor_claim(&claim).await.unwrap();

        assert_eq!(governor.remaining_budget(actor), 1);
        assert_eq!(
            store.admission(operation).await.unwrap().unwrap().state,
            AdmissionState::Aborted
        );
    }
}
