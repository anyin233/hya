use std::sync::Arc;

use hya_proto::{
    ActorClaim, AgentName, Envelope, Event, FinishReason, MessageId, PartId, Role, SessionId,
};
use hya_runtime_abi::{
    HookPoint, HostError, NativeHook, NativeTurnAdmitInput, NativeTurnAdmitOutcome,
};
use tokio_util::sync::CancellationToken;

use super::SessionEngine;
use crate::error::CoreError;
use crate::hooks::{NativeCommandBeforeInput, NativeMessageUserBeforeInput, activation_hook_for};
use crate::store_bridge::{
    RecoveredActorClaim, RecoveredResidentWork, ResidentArchive, ResidentArchiveOwner,
    SessionStoreExt as _,
};
use crate::{HookChain, TurnBinding};

// Keep the external-entry restriction until its bundle owns admission.
struct ExternalTurnGuard(Vec<Envelope>);

#[async_trait::async_trait]
impl NativeHook for ExternalTurnGuard {
    fn id(&self) -> &str {
        "external-turn-guard"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::TurnAdmit]
    }

    async fn turn_admit(
        &self,
        input: NativeTurnAdmitInput,
    ) -> Result<NativeTurnAdmitOutcome, HostError> {
        // Registration survives archive and revival.
        if self.0.iter().any(|envelope| {
            matches!(
                hya_tool::extension::ExtensionEvent::from_event(&envelope.event),
                Some(hya_tool::extension::ExtensionEvent::AgentRegistered {
                    agent_session, ..
                }) if agent_session == input.session
            )
        }) {
            return Ok(NativeTurnAdmitOutcome::Refuse {
                reason: "this subagent's episodes own its turns; steer this subagent with mail"
                    .into(),
            });
        }
        Ok(NativeTurnAdmitOutcome::Admit)
    }
}

fn map_admission_error(error: HostError) -> CoreError {
    match error {
        HostError::Cancelled => CoreError::Cancelled,
        HostError::InvalidInput(reason) => CoreError::Invalid(reason),
        other => CoreError::Tool(Arc::new(other.into())),
    }
}

fn completed_text_events(session: SessionId, role: Role, text: String) -> (MessageId, [Event; 5]) {
    let message = MessageId::new();
    let part = PartId::new();
    (
        message,
        [
            Event::MessageStarted {
                session,
                message,
                role,
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
                role,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ],
    )
}

impl SessionEngine {
    async fn admission_binding(
        &self,
        session: SessionId,
    ) -> Result<(TurnBinding, String), CoreError> {
        let projection = self.read_projection_shared(session).await?;
        // Every session records its workdir at creation; there is no
        // process-level fallback (ADR-0024).
        let workdir = projection
            .session
            .workdir
            .as_deref()
            .map(std::path::PathBuf::from)
            .ok_or_else(|| CoreError::Invalid(format!("session not found: {session}")))?;
        let binding = self.bind_session_runtime(session, &workdir).await?;
        let stable_id = projection
            .session
            .agent
            .as_ref()
            .map_or(
                binding.agent_catalog().default_agent_id(),
                hya_proto::AgentName::as_str,
            )
            .to_string();
        Ok((binding, stable_id))
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
        crate::HookChain::new(hooks)
            .message_user_before(NativeMessageUserBeforeInput { session, text })
            .await
    }

    async fn apply_bundle_command_hook(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
        input: NativeCommandBeforeInput,
    ) -> String {
        let hooks = binding.bundle_hooks_for_agent(stable_id);
        if hooks.is_empty() {
            return input.text;
        }
        crate::HookChain::new(hooks)
            .command_execute_before(input)
            .await
    }
    pub(crate) async fn recover_resident_actor_durable(
        &self,
        recovered: &RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<RecoveredResidentWork, CoreError> {
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
        let projection = self.read_projection_shared(root).await?;
        let mut members = hya_tool::extension::team_projection(&projection)
            .roster
            .iter()
            .filter(|(path, _)| path.as_str() != hya_proto::ROOT_HANDLE)
            .map(|(path, entry)| (path.clone(), entry.session))
            .collect::<Vec<_>>();
        drop(projection);
        members.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        for (handle, member) in members {
            let handoff = self.degraded_terminal_handoff(member).await;
            let (_, envelopes) = self
                .store
                .finalize_resident_archive(
                    ResidentArchiveOwner::Takeover,
                    root,
                    &handle,
                    ResidentArchive {
                        actor: member,
                        generation: handoff.generation,
                        doc: handoff.doc,
                        degraded: handoff.degraded,
                        reason: hya_tool::ArchiveReason::RootTeardown,
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
        if depth > 0 {
            let guard: Arc<dyn NativeHook> = Arc::new(ExternalTurnGuard(self.replay(root).await?));
            let agent = self
                .store
                .with_projection(session, |projection| projection.session.agent.clone())
                .await?;
            HookChain::of_hooks([guard])
                .turn_admit(NativeTurnAdmitInput {
                    session,
                    root,
                    agent,
                    actor_claim: None,
                })
                .await
                .map_err(map_admission_error)?;
        }
        Ok(())
    }

    pub(super) async fn apply_turn_admission_hooks(
        &self,
        session: SessionId,
        agent: AgentName,
        claim: Option<&ActorClaim>,
        cancel: &CancellationToken,
    ) -> Result<(), CoreError> {
        let global = self.global_hooks();
        let scoped = activation_hook_for(session);
        let hooks = HookChain::concat([Some(&global), scoped.as_deref()].into_iter().flatten());
        if !hooks.handles(HookPoint::TurnAdmit) {
            return Ok(());
        }
        let (root, _) = self.session_lineage(session).await?;
        let input = NativeTurnAdmitInput {
            session,
            root,
            agent: Some(agent),
            actor_claim: claim.copied(),
        };
        let steering = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(CoreError::Cancelled),
            result = hooks.turn_admit(input) => result.map_err(map_admission_error)?,
        };
        if !steering.is_empty() {
            let events = steering
                .into_iter()
                .flat_map(|text| completed_text_events(session, Role::System, text).1)
                .collect();
            self.emit_batch_for_actor(claim, session, events).await?;
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
        let (message, events) = completed_text_events(session, role, content);
        for event in events {
            self.emit(session, event).await?;
        }
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

    /// Admit a user prompt using a pre-captured runtime [`TurnBinding`].
    pub async fn admit_user_prompt_with_binding(
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
        let text = self
            .hooks
            .message_user_before(NativeMessageUserBeforeInput { session, text })
            .await;
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
        let channel_policy = crate::channel_policy::from_binding(&binding)
            .map_err(CoreError::bundle)?
            .snapshot_for(&stable_id);
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
        let channel_policy = crate::channel_policy::from_binding(binding)
            .map_err(CoreError::bundle)?
            .snapshot_for(stable_id);
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
        let text = self
            .hooks
            .command_execute_before(NativeCommandBeforeInput {
                session,
                command: command.clone(),
                arguments: arguments.clone(),
                text,
            })
            .await;
        let text = self
            .apply_bundle_command_hook(
                &binding,
                &stable_id,
                NativeCommandBeforeInput {
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
    use hya_tool::{PermissionPlane, PermissionRules};

    use super::*;
    use crate::{CreateSession, EventBus};

    struct TestAdmissionHook {
        outcome: std::sync::Mutex<Option<NativeTurnAdmitOutcome>>,
        entered: CancellationToken,
    }

    impl TestAdmissionHook {
        fn once(outcome: NativeTurnAdmitOutcome) -> Arc<Self> {
            Arc::new(Self {
                outcome: std::sync::Mutex::new(Some(outcome)),
                entered: CancellationToken::new(),
            })
        }
    }

    #[async_trait::async_trait]
    impl NativeHook for TestAdmissionHook {
        fn id(&self) -> &str {
            "test-admission"
        }

        fn hook_points(&self) -> &[HookPoint] {
            &[HookPoint::TurnAdmit]
        }

        async fn turn_admit(
            &self,
            _input: NativeTurnAdmitInput,
        ) -> Result<NativeTurnAdmitOutcome, HostError> {
            self.entered.cancel();
            Ok(self.outcome.lock().unwrap().take().unwrap())
        }
    }

    #[tokio::test]
    async fn admission_waits_in_order_and_commits_steering_only_after_acceptance() {
        let store = hya_store::SessionStore::connect_memory().await.unwrap();
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        let engine = SessionEngine::new(
            store,
            Arc::new(ProviderRouter::new()),
            crate::test_support::runtime(hya_tool_loader::builtins()),
            permission,
            EventBus::default(),
        );
        let agent = AgentName::new("build");
        let session = engine
            .create(CreateSession {
                parent: None,
                agent: agent.clone(),
                model: ModelRef::new("fake"),
                workdir: "/tmp".into(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            })
            .await
            .unwrap();
        let original_seq = engine
            .read_projection_shared(session)
            .await
            .unwrap()
            .last_seq;
        let steer = || {
            TestAdmissionHook::once(NativeTurnAdmitOutcome::Steer {
                message: "steering".into(),
            })
        };
        let refused = engine.clone().with_hook_list(vec![
            steer(),
            TestAdmissionHook::once(NativeTurnAdmitOutcome::Refuse {
                reason: "admission refused".into(),
            }),
        ]);
        let cancel = CancellationToken::new();
        let error = refused
            .apply_turn_admission_hooks(session, agent.clone(), None, &cancel)
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::Invalid(reason) if reason == "admission refused"));
        assert_eq!(
            engine
                .read_projection_shared(session)
                .await
                .unwrap()
                .last_seq,
            original_seq
        );

        let (ready, until) = tokio::sync::oneshot::channel();
        let deferred = TestAdmissionHook::once(NativeTurnAdmitOutcome::Defer { until });
        let entered = deferred.entered.clone();
        let accepted = engine.with_hook_list(vec![deferred, steer()]);
        let admission = accepted.apply_turn_admission_hooks(session, agent, None, &cancel);
        let mut admission = std::pin::pin!(admission);
        tokio::select! {
            outcome = &mut admission => panic!("admitted before readiness: {outcome:?}"),
            () = entered.cancelled() => {}
        }
        assert!(futures::poll!(&mut admission).is_pending());
        ready.send(()).unwrap();
        admission.await.unwrap();
        let events = accepted.replay(session).await.unwrap();
        assert_eq!(events.last().unwrap().seq.0, original_seq + 5);
        assert!(events.iter().any(|envelope| {
            matches!(&envelope.event, Event::TextDelta { delta, .. } if delta == "steering")
        }));
    }

    #[tokio::test]
    async fn ordinary_user_message_commits_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let store = hya_store::SessionStore::connect_memory().await.unwrap();
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        let engine = SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new()),
            crate::test_support::runtime(hya_tool_loader::builtins()),
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
