use hya_proto::{Envelope, Event, FinishReason, MessageId, PartId, Role, SessionId};
use hya_store::{ActorClaim, RecoveredActorClaim};

use super::SessionEngine;
use crate::TurnBinding;
use crate::error::CoreError;
use crate::hooks::{
    CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome,
};

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
            .map_or(crate::MAIN_AGENT_ID, hya_proto::AgentName::as_str)
            .to_string();
        // Every session records its workdir at creation; there is no
        // process-level fallback (ADR-0024).
        let workdir = projection
            .session
            .workdir
            .as_deref()
            .map(std::path::PathBuf::from)
            .ok_or_else(|| CoreError::Invalid(format!("session not found: {session}")))?;
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
    pub(crate) async fn recover_resident_actor_durable(
        &self,
        recovered: &RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<hya_store::RecoveredResidentWork, CoreError> {
        let outcome = self
            .store
            .recover_resident_actor(recovered, root, handle)
            .await?;
        for envelope in outcome.envelopes {
            self.publish_envelope(envelope);
        }
        Ok(outcome.work)
    }

    /// Root teardown requests episode-owned endings, then reconciles orphans.
    /// No failure reports are mailed to parents during teardown.
    pub async fn force_archive_team(&self, root: SessionId) -> Result<(), CoreError> {
        if let Some(supervisor) = self.archive_reviver() {
            return supervisor.teardown_root(root).await;
        }
        // Without a supervisor there can be no in-memory episode. The store's
        // runtime-owner check remains authoritative for orphan Takeover.
        let projection = self.read_projection(root).await?;
        let mut members = projection
            .team
            .roster
            .iter()
            .filter(|(path, _)| path.as_str() != hya_proto::ROOT_HANDLE)
            .collect::<Vec<_>>();
        members.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        for (handle, member) in members {
            let handoff = self.degraded_terminal_handoff(member.session).await;
            let (_, envelopes) = self
                .store
                .finalize_resident_archive(
                    hya_store::ResidentArchiveOwner::Takeover,
                    root,
                    handle,
                    hya_store::ResidentArchive {
                        actor: member.session,
                        generation: handoff.generation,
                        doc: handoff.doc,
                        degraded: handoff.degraded,
                        reason: hya_proto::ArchiveReason::RootTeardown,
                        note: "root turn teardown".into(),
                        member_status: hya_proto::MemberRunStatus::Cancelled,
                        report: None,
                        check_gate: false,
                        expected_work: None,
                    },
                )
                .await?;
            self.publish_resident_finalization(envelopes);
        }
        Ok(())
    }

    pub(crate) fn publish_resident_finalization(&self, envelopes: Vec<Envelope>) {
        for envelope in envelopes {
            self.publish_envelope(envelope);
        }
    }

    /// Close this episode's straggler before its owner drops the turn future.
    pub(crate) async fn close_harness_turn(
        &self,
        claim: Option<&ActorClaim>,
        session: SessionId,
        cause: hya_proto::FinishCause,
    ) -> Result<(), CoreError> {
        if let Some(message) = self.turn_gate.message(session) {
            self.close_turn_message(
                claim,
                session,
                message,
                FinishReason::Cancelled,
                Some(cause),
            )
            .await?;
        }
        Ok(())
    }

    /// Refuse client turns and execution-state edits on registered team members.
    /// Member episodes are controlled through mail, including after archival.
    ///
    /// # Errors
    /// [`CoreError::Invalid`] with mail guidance for a team member; store failures.
    pub async fn ensure_external_turn_allowed(&self, session: SessionId) -> Result<(), CoreError> {
        let (root, depth) = self.session_lineage(session).await?;
        // Registration is a permanent identity fact. Unlike the current roster
        // or claim, its event survives archive/revive and needs no episode lock.
        if depth > 0
            && self.replay(root).await?.iter().any(|envelope| {
                matches!(envelope.event, Event::AgentRegistered { agent_session, .. } if agent_session == session)
            })
        {
            return Err(CoreError::Invalid(
                "this subagent's episodes own its turns; steer this subagent with mail".into(),
            ));
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
        self.ensure_external_turn_allowed(session).await?;
        let (binding, stable_id) = self.admission_binding(session).await?;
        self.admit_user_prompt_with_id_and_binding(session, message, text, &binding, &stable_id)
            .await
    }

    /// The user text after the `message.user.before` hooks (global, then
    /// the agent's bundle hooks).
    pub(crate) async fn user_text_after_hooks(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
        session: SessionId,
        text: String,
    ) -> String {
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
        self.apply_bundle_message_hook(binding, stable_id, session, text)
            .await
    }

    /// Admit a user prompt with image attachments (see [`crate::attachments`])
    /// under the caller-supplied `message` id.
    ///
    /// The images are stored in the session's blob table and the user
    /// message (`MessageStarted`, its text part, `UserPromptContextRecorded`
    /// naming the blobs, `MessageFinished`) is appended in the same
    /// transaction, so the prompt is never visible without its images and a
    /// failure admits nothing. Without attachments this is
    /// [`Self::admit_user_prompt_with_id`].
    ///
    /// # Errors
    /// [`CoreError::Invalid`] when the attachments fail
    /// [`crate::attachments::validate_prompt_attachments`] (callers validate
    /// first to map the error); store failures.
    pub async fn admit_user_prompt_with_attachments(
        &self,
        session: SessionId,
        message: MessageId,
        text: String,
        mut attachments: Vec<crate::attachments::PromptAttachment>,
    ) -> Result<MessageId, CoreError> {
        self.ensure_external_turn_allowed(session).await?;
        if attachments.is_empty() {
            return self.admit_user_prompt_with_id(session, message, text).await;
        }
        crate::attachments::validate_prompt_attachments(&mut attachments)
            .map_err(|error| CoreError::Invalid(error.to_string()))?;
        let (binding, stable_id) = self.admission_binding(session).await?;
        let channel_policy = crate::ChannelPolicy::from_binding(&binding)?.snapshot_for(&stable_id);
        let text = self
            .user_text_after_hooks(&binding, &stable_id, session, text)
            .await;
        let part = PartId::new();
        let mut blobs = Vec::with_capacity(attachments.len());
        let mut files = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            let hash = crate::attachments::blob_hash(&attachment.data);
            files.push(crate::attachments::file_entry(
                PartId::new(),
                &attachment,
                &hash,
            ));
            blobs.push((hash, attachment.data));
        }
        let events = vec![
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
            Event::UserPromptContextRecorded {
                session,
                message,
                files,
                agents: Vec::new(),
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::User,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        let envelopes = self
            .store
            .append_events_with_blobs(session, &blobs, events)
            .await?;
        for envelope in envelopes {
            self.publish_envelope(envelope);
        }
        self.update_session_channel_policy(session, channel_policy);
        Ok(message)
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
        let text = self
            .user_text_after_hooks(binding, stable_id, session, text)
            .await;
        let part = PartId::new();
        self.emit_batch_for_actor(
            None,
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

    pub(crate) async fn admit_user_prompt_for_actor_with_binding(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        text: String,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<MessageId, CoreError> {
        let channel_policy = crate::ChannelPolicy::from_binding(binding)?.snapshot_for(stable_id);
        let text = self
            .user_text_after_hooks(binding, stable_id, session, text)
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
        self.ensure_external_turn_allowed(session).await?;
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

    use hya_proto::{AgentName, ModelRef};
    use hya_provider::ProviderRouter;
    use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

    use super::*;
    use crate::{CreateSession, EventBus};

    #[tokio::test]
    async fn ordinary_user_message_commits_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let store = hya_store::SessionStore::connect_memory().await.unwrap();
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        let engine = SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new()),
            crate::test_support::runtime(ToolRegistry::builtins()),
            permission,
            EventBus::default(),
        );
        let session = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/tmp".into(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            })
            .await
            .unwrap();
        let commits = Arc::new(AtomicUsize::new(0));
        let mut tx = store.begin_transaction().await.unwrap();
        let counter = commits.clone();
        tx.lock_handle().await.unwrap().set_commit_hook(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            true
        });
        tx.rollback().await.unwrap();
        engine
            .admit_user_prompt(session, "hello".into())
            .await
            .unwrap();
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        let events = store.replay(session).await.unwrap();
        let tail = &events[events.len() - 5..];
        assert!(matches!(
            tail[0].event,
            Event::MessageStarted {
                role: Role::User,
                ..
            }
        ));
        assert!(matches!(tail[1].event, Event::TextStart { .. }));
        assert!(matches!(&tail[2].event, Event::TextDelta { delta, .. } if delta == "hello"));
        assert!(matches!(tail[3].event, Event::TextEnd { .. }));
        assert!(matches!(
            tail[4].event,
            Event::MessageFinished {
                role: Role::User,
                ..
            }
        ));
    }
}
