use hya_proto::{Event, FinishReason, MessageId, OperationId, PartId, Role, SessionId};
use hya_store::{
    ActorClaim, AdmissionClaim, AdmissionClaimOutcome, AdmissionStartOutcome, AdmissionState,
    AdmissionTerminal, RecoveredActorClaim,
};
use tokio_util::sync::CancellationToken;

use super::SessionEngine;
use crate::TurnBinding;
use crate::error::CoreError;
use crate::hooks::{
    CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome,
};
use crate::orchestrator::OperationReservation;

/// Result of beginning a spawn admission against governor + durable claims.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpawnAdmissionOutcome {
    /// Fresh admission started for this operation.
    Started,
    /// Operation already admitted; returns durable state.
    Existing(AdmissionState),
    /// Per-run budget or concurrency overloaded.
    Overloaded,
    /// Subagent recursion depth exceeded.
    MaxDepth,
    /// Cancelled before admission completed.
    Cancelled,
}

impl SessionEngine {
    async fn admission_binding(
        &self,
        session: SessionId,
    ) -> Result<(TurnBinding, String), CoreError> {
        let projection = self.read_projection(session).await?;
        let stable_id = projection
            .session
            .agent
            .as_ref()
            .map_or("build", hya_proto::AgentName::as_str)
            .to_string();
        let workdir = projection
            .session
            .workdir
            .as_deref()
            .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
        Ok((
            self.bind_session_runtime(session, &workdir).await?,
            stable_id,
        ))
    }

    async fn apply_bundle_message_hook(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
        session: SessionId,
        text: String,
    ) -> String {
        let hooks = binding.bundle_hooks_for_agent(stable_id);
        if hooks.is_empty() {
            return text;
        }
        let dispatcher = crate::HookChain::new(hooks);
        match dispatcher
            .message_user_before(MessageUserBeforeInput { session, text })
            .await
        {
            MessageUserBeforeOutcome::Continue { text } => text,
        }
    }

    async fn apply_bundle_command_hook(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
        input: CommandExecuteBeforeInput,
    ) -> String {
        let hooks = binding.bundle_hooks_for_agent(stable_id);
        if hooks.is_empty() {
            return input.text;
        }
        let dispatcher = crate::HookChain::new(hooks);
        let CommandExecuteBeforeOutcome::Continue { text } =
            dispatcher.command_execute_before(input).await;
        text
    }
    /// Reserve one durable operation, reconciling terminal journal releases on overload.
    ///
    /// `root` selects the run budget, `operation` identifies the exact debit,
    /// `units` is the all-or-nothing member count, and `cancel` controls the
    /// operation. `None` means this engine has no governor.
    pub async fn try_reserve_spawn_operation(
        &self,
        root: SessionId,
        operation: OperationId,
        units: u64,
        cancel: CancellationToken,
    ) -> Result<Option<OperationReservation>, CoreError> {
        let Some(governor) = self.governor.clone() else {
            return Ok(None);
        };
        let reservation = governor.try_reserve_operation(root, operation, units, cancel.clone());
        if reservation != OperationReservation::Overloaded {
            return Ok(Some(reservation));
        }
        for released in self
            .store
            .terminal_released_operations_for_root(root)
            .await?
        {
            governor.release_operation(released);
        }
        Ok(Some(
            governor.try_reserve_operation(root, operation, units, cancel),
        ))
    }
    /// Start admission for a spawn request; reserves budget and validates roster.
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
        if let Some(governor) = &self.governor
            && depth.saturating_add(1) > governor.max_depth()
        {
            self.finalize_spawn_admission(
                operation_id,
                AdmissionTerminal::Aborted,
                "maximum subagent depth exceeded",
                actor_claim.as_ref(),
            )
            .await?;
            return Ok(SpawnAdmissionOutcome::MaxDepth);
        }
        match self
            .try_reserve_spawn_operation(root, operation_id, u64::from(admission_units), cancel)
            .await?
        {
            Some(OperationReservation::Overloaded) => {
                self.finalize_spawn_admission(
                    operation_id,
                    AdmissionTerminal::Aborted,
                    "spawn admission overloaded",
                    actor_claim.as_ref(),
                )
                .await?;
                return Ok(SpawnAdmissionOutcome::Overloaded);
            }
            Some(OperationReservation::Existing | OperationReservation::Conflict) => {
                return Ok(SpawnAdmissionOutcome::Existing(AdmissionState::Accepted));
            }
            Some(OperationReservation::Acquired) | None => {}
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

    /// Commit a successful spawn admission after the child session exists.
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

    /// Finalize all pending admissions under a root session.
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

    /// Abort operations recovered after resident restart.
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

    /// Root-turn teardown (ADR-0015): force-archive every live descendant with
    /// a degraded handoff and a synthesized failure report, then hand claim
    /// release to the installed supervisor seam. The durable log keeps
    /// everything; the roster empties below the root.
    ///
    /// # Errors
    /// Propagates store/event failures; a failed row aborts the sweep.
    pub async fn force_archive_team(&self, root: SessionId) -> Result<(), CoreError> {
        let projection = self.read_projection(root).await?;
        // Deepest paths first so a child's report mail still finds its (live)
        // parent inside the same sweep.
        let mut paths: Vec<&String> = projection
            .team
            .roster
            .keys()
            .filter(|path| path.as_str() != hya_proto::ROOT_HANDLE)
            .collect();
        paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
        for path in paths {
            let child = projection.team.roster[path].session;
            crate::resident::archive_reported_agent(
                self,
                root,
                path,
                child,
                None,
                hya_proto::ReportOutcome::Failed,
                "root turn teardown".to_string(),
                hya_proto::ArchiveReason::RootTeardown,
                true,
            )
            .await?;
        }
        if let Some(reviver) = self.archive_reviver() {
            reviver.teardown_root(root).await?;
        }
        Ok(())
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

    /// Append a system message to the session transcript.
    pub async fn inject_system_message(
        &self,
        session: SessionId,
        content: String,
    ) -> Result<MessageId, CoreError> {
        self.inject_completed_text_message(session, content, Role::System)
            .await
    }

    /// Append a completed assistant message for a user-visible control-plane result.
    pub async fn inject_assistant_message(
        &self,
        session: SessionId,
        content: String,
    ) -> Result<MessageId, CoreError> {
        self.inject_completed_text_message(session, content, Role::Assistant)
            .await
    }

    /// Append one completed text message with `role` and return its generated message id.
    ///
    /// `session` selects the event log and `content` supplies the only text part. The returned id
    /// identifies the fully emitted message.
    async fn inject_completed_text_message(
        &self,
        session: SessionId,
        content: String,
        role: Role,
    ) -> Result<MessageId, CoreError> {
        let message = MessageId::new();
        let part = PartId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role,
                agent: None,
                model: None,
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
                role,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
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
                    agent: None,
                    model: None,
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
                    cause: None,
                },
            ],
        )
        .await?;
        Ok(message)
    }

    /// Admit a user prompt message and return its id.
    pub async fn admit_user_prompt(
        &self,
        session: SessionId,
        text: String,
    ) -> Result<MessageId, CoreError> {
        self.admit_user_prompt_with_id(session, MessageId::new(), text)
            .await
    }

    pub(crate) async fn admit_user_prompt_with_binding(
        &self,
        session: SessionId,
        text: String,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<MessageId, CoreError> {
        self.admit_user_prompt_with_id_and_binding(
            session,
            MessageId::new(),
            text,
            binding,
            stable_id,
        )
        .await
    }

    /// Admit a user prompt with a caller-supplied message id.
    pub async fn admit_user_prompt_with_id(
        &self,
        session: SessionId,
        message: MessageId,
        text: String,
    ) -> Result<MessageId, CoreError> {
        let (binding, stable_id) = self.admission_binding(session).await?;
        self.admit_user_prompt_with_id_and_binding(session, message, text, &binding, &stable_id)
            .await
    }

    pub(crate) async fn admit_user_prompt_with_id_and_binding(
        &self,
        session: SessionId,
        message: MessageId,
        text: String,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<MessageId, CoreError> {
        let channel_policy = crate::ChannelPolicy::from_binding(binding)?.snapshot_for(stable_id);
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
        let text = self
            .apply_bundle_message_hook(binding, stable_id, session, text)
            .await;
        let part = PartId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::User,
                agent: None,
                model: None,
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
                cause: None,
            },
        )
        .await?;
        self.update_session_channel_policy(session, channel_policy);
        Ok(message)
    }

    pub(crate) async fn admit_user_prompt_for_actor_with_binding(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        text: String,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<MessageId, CoreError> {
        let channel_policy = crate::ChannelPolicy::from_binding(binding)?.snapshot_for(stable_id);
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
        let text = self
            .apply_bundle_message_hook(binding, stable_id, session, text)
            .await;
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
                    agent: None,
                    model: None,
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
                    cause: None,
                },
            ],
        )
        .await?;
        self.update_session_channel_policy(session, channel_policy);
        Ok(message)
    }

    /// Record attached files/agents context for an admitted prompt.
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

    /// Admit a command-triggered prompt.
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

    /// Admit a command-triggered prompt with a fixed message id.
    pub async fn admit_command_prompt_with_id(
        &self,
        session: SessionId,
        message: MessageId,
        command: String,
        arguments: String,
        text: String,
    ) -> Result<MessageId, CoreError> {
        let (binding, stable_id) = self.admission_binding(session).await?;
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
        let text = self
            .apply_bundle_command_hook(
                &binding,
                &stable_id,
                CommandExecuteBeforeInput {
                    session,
                    command: command.clone(),
                    arguments: arguments.clone(),
                    text,
                },
            )
            .await;
        let message = self
            .admit_user_prompt_with_id_and_binding(session, message, text, &binding, &stable_id)
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
