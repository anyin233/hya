use super::*;

#[derive(Default)]
struct RevivalHook {
    block: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl crate::hooks::HookDispatcher for RevivalHook {
    fn dispatch_event(&self, _: &hya_proto::Envelope) {}

    async fn command_execute_before(
        &self,
        input: crate::hooks::CommandExecuteBeforeInput,
    ) -> crate::hooks::CommandExecuteBeforeOutcome {
        crate::hooks::CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(
        &self,
        input: crate::hooks::TextCompleteInput,
    ) -> crate::hooks::TextCompleteOutcome {
        crate::hooks::TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(
        &self,
        input: crate::hooks::MessageUserBeforeInput,
    ) -> crate::hooks::MessageUserBeforeOutcome {
        // Hold the first revived mail wake; creation hooks do not run again.
        if self.block.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        crate::hooks::MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(
        &self,
        input: crate::hooks::ChatParamsInput,
    ) -> crate::hooks::ChatParamsOutcome {
        crate::hooks::ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(
        &self,
        input: crate::hooks::ToolExecuteBeforeInput,
    ) -> crate::hooks::ToolExecuteBeforeOutcome {
        crate::hooks::ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(
        &self,
        input: crate::hooks::ToolExecuteAfterInput,
    ) -> crate::hooks::ToolExecuteAfterOutcome {
        crate::hooks::ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

async fn archived_child(
    engine: &Arc<SessionEngine>,
) -> (
    SessionId,
    Arc<ResidentSupervisor>,
    String,
    String,
    crate::engine::TurnLease,
) {
    let root = root_team(engine).await;
    let lead_turn = engine.try_begin_turn(root).unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("resident"),
            model: ModelRef::new("fake"),
            workdir: ".".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    supervisor
        .register_existing_resident(
            root,
            child,
            "resident-1".into(),
            AgentSpec {
                name: AgentName::new("resident"),
                model: ModelRef::new("fake"),
                system_prompt: String::new(),
                workdir: PathBuf::from("."),
                reasoning: None,
            },
            None,
        )
        .await
        .unwrap();
    let handle = "main/resident-1".to_string();
    let channel = dm_channel_between(
        &engine.read_projection(root).await.unwrap(),
        "main",
        &handle,
    )
    .unwrap();
    supervisor
        .archive_member(root, &handle, "archive for revival test")
        .await
        .unwrap();
    (root, supervisor, handle, channel, lead_turn)
}

#[tokio::test]
async fn mail_auth_handle_revival_requires_an_authorized_own_archived_child() {
    let engine = Arc::new(engine().await);
    let (root, supervisor, handle, channel, lead_turn) = archived_child(&engine).await;
    let allowed = engine.session_channel_policy(root).unwrap();
    let outsider = register_child(&engine, root, root, "main", "outsider-1").await;
    let before = engine.replay(root).await.unwrap().len();
    assert_mail_rejected(
        engine
            .mail_send(
                outsider.session,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "cannot revive a sibling".into(),
            )
            .await,
        &outsider.path,
        &format!("direct:{handle}"),
    );
    engine.update_session_channel_policy(
        root,
        hya_tool::ChannelPolicySnapshot {
            dm_parent: 0,
            ..Default::default()
        },
    );
    assert_mail_rejected(
        engine
            .mail_send(
                root,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "policy forbids revival".into(),
            )
            .await,
        "main",
        &channel,
    );
    assert_eq!(engine.replay(root).await.unwrap().len(), before);
    assert!(
        engine
            .read_projection(root)
            .await
            .unwrap()
            .team
            .archived
            .contains_key(&handle)
    );
    engine.update_session_channel_policy(root, allowed);
    let receipt = engine
        .mail_send(
            root,
            MailEndpoint::Handle("resident-1".into()),
            MailKind::Message,
            "authorized revival".into(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.recipients, 1);
    assert!(engine.replay(root).await.unwrap().iter().any(|e| {
        matches!(&e.event, Event::AgentRestarted { handle: restarted, .. } if restarted == &handle)
    }));
    drop(lead_turn);
    supervisor
        .drain(
            hya_proto::FinishCause::Shutdown,
            std::time::Duration::from_secs(2),
        )
        .await;
}

#[tokio::test]
async fn mail_auth_concurrent_handle_and_channel_sends_revive_exactly_once() {
    use std::time::Duration;

    tokio::time::timeout(Duration::from_secs(10), async {
        let hooks = Arc::new(RevivalHook::default());
        let engine = Arc::new(engine().await.with_hooks(hooks.clone()));
        let (root, supervisor, handle, channel, lead_turn) = archived_child(&engine).await;
        hooks.block.store(true, std::sync::atomic::Ordering::SeqCst);
        let reviving = tokio::spawn({
            let engine = engine.clone();
            let handle = handle.clone();
            async move {
                engine
                    .mail_send(
                        root,
                        MailEndpoint::Handle(handle),
                        MailKind::Message,
                        "episode two".into(),
                    )
                    .await
            }
        });
        hooks.entered.notified().await;
        let projection = engine.read_projection(root).await.unwrap();
        assert!(projection.team.roster.contains_key(&handle));
        assert!(!projection.team.archived.contains_key(&handle));
        let result = engine
            .mail_send(
                root,
                MailEndpoint::Channel(channel.clone()),
                MailKind::Message,
                "during revival".into(),
            )
            .await;
        let handle_result = engine
            .mail_send(
                root,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "handle during revival".into(),
            )
            .await;
        let delivered = inbox(&engine, root, &handle).await;
        let restarts_before_release = engine
            .replay(root)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(&e.event, Event::AgentRestarted { .. }))
            .count();
        // Release and join before asserting, including on the broken path.
        hooks.release.notify_one();
        reviving.await.unwrap().unwrap();
        drop(lead_turn);
        supervisor
            .drain(hya_proto::FinishCause::Shutdown, Duration::from_secs(2))
            .await;
        let receipt = result.expect("the live episode accepts concurrent mail");
        assert_eq!(handle_result.unwrap().recipients, 2);
        assert_eq!(receipt.to, MailEndpoint::Channel(channel));
        assert_eq!(receipt.recipients, 2);
        assert_eq!(
            delivered,
            ["episode two", "during revival", "handle during revival"]
        );
        assert_eq!(restarts_before_release, 1);
    })
    .await
    .expect("revival synchronization must complete");
}

#[tokio::test]
async fn resident_stop_commits_before_stale_alias_send_rechecks_the_dm() {
    let db_path =
        std::env::temp_dir().join(format!("hya-core-mailbox-stop-{}.db", SessionId::new()));
    let db_path = db_path.to_string_lossy().into_owned();
    let make_engine = |store: SessionStore, bus: EventBus| {
        let router = Arc::new(ProviderRouter::new());
        let runtime = local_test_runtime();
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
        SessionEngine::new(store, router, runtime, permission, bus)
    };

    let primary = Arc::new(make_engine(
        SessionStore::connect(&db_path).await.unwrap(),
        EventBus::default(),
    ));
    let root = root_team(&primary).await;
    let agent = AgentSpec {
        name: AgentName::new("resident"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: PathBuf::from("."),
        reasoning: None,
    };
    let binding = primary.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let supervisor = ResidentSupervisor::start(primary.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, None),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let sender = Arc::new(
        make_engine(
            SessionStore::connect(&db_path).await.unwrap(),
            EventBus::default(),
        )
        .with_mail_pre_append_gate(MailPreAppendGate::new(entered.clone(), release.clone())),
    );
    let sender_binding = sender.bind_runtime(std::path::Path::new(".")).unwrap();
    sender
        .capture_session_bundle_hooks(root, &sender_binding, "build")
        .await;
    let sender_handle = handle.clone();
    let send = tokio::spawn(async move {
        sender
            .mail_send(
                root,
                MailEndpoint::Handle(sender_handle),
                MailKind::Message,
                "stale direct mail".to_string(),
            )
            .await
    });
    entered.notified().await;

    let stop_result = supervisor.end_member(root, &handle).await;
    assert!(
        stop_result.is_ok(),
        "resident stop must complete before stale send resumes: {stop_result:?}"
    );
    assert!(
        !primary
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );

    release.notify_one();
    let send_result = send.await.unwrap();
    assert!(matches!(send_result,
        Err(CoreError::Store(hya_store::StoreError::MailRevivalRequired {
            sender, channel, target,
        })) if sender == "main" && channel.starts_with("DM-") && target == handle));

    let replay = primary.replay(root).await.unwrap();
    assert!(
        !replay
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::MailSent { .. }))
    );
}
