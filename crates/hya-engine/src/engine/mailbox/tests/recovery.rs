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
        i: crate::hooks::CommandExecuteBeforeInput,
    ) -> crate::hooks::CommandExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.command_execute_before(i).await
    }
    async fn text_complete(
        &self,
        i: crate::hooks::TextCompleteInput,
    ) -> crate::hooks::TextCompleteOutcome {
        crate::hooks::NoopHookHost.text_complete(i).await
    }
    async fn message_user_before(
        &self,
        i: crate::hooks::MessageUserBeforeInput,
    ) -> crate::hooks::MessageUserBeforeOutcome {
        if self.block.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        crate::hooks::MessageUserBeforeOutcome::Continue { text: i.text }
    }
    async fn chat_params(
        &self,
        i: crate::hooks::ChatParamsInput,
    ) -> crate::hooks::ChatParamsOutcome {
        crate::hooks::NoopHookHost.chat_params(i).await
    }
    async fn tool_execute_before(
        &self,
        i: crate::hooks::ToolExecuteBeforeInput,
    ) -> crate::hooks::ToolExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.tool_execute_before(i).await
    }
    async fn tool_execute_after(
        &self,
        i: crate::hooks::ToolExecuteAfterInput,
    ) -> crate::hooks::ToolExecuteAfterOutcome {
        crate::hooks::NoopHookHost.tool_execute_after(i).await
    }
}

async fn archived_child(
    eng: &Arc<SessionEngine>,
) -> (
    SessionId,
    Arc<ResidentSupervisor>,
    String,
    String,
    crate::engine::TurnLease,
) {
    let root = root_team(eng).await;
    let lead_turn = eng.try_begin_turn(root).unwrap();
    let sup = ResidentSupervisor::start(eng.clone());
    let child = eng
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
    sup.register_existing_resident(
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
    let channel =
        dm_channel_between(&eng.read_projection(root).await.unwrap(), "main", &handle).unwrap();
    sup.archive_member(root, &handle, "archive for revival test")
        .await
        .unwrap();
    (root, sup, handle, channel, lead_turn)
}

#[tokio::test]
async fn mail_auth_revival_requires_authorized_parent_and_deduplicates_concurrent_sends() {
    use std::time::Duration;
    tokio::time::timeout(Duration::from_secs(10), async {
        let hooks = Arc::new(RevivalHook::default());
        let eng = Arc::new(engine().await.with_hooks(hooks.clone()));
        let (root, sup, handle, channel, lead_turn) = archived_child(&eng).await;
        let allowed = eng.session_channel_policy(root).unwrap();
        let outsider = register_child(&eng, root, root, "main", "outsider-1").await;
        let before = eng.replay(root).await.unwrap().len();

        assert_mail_rejected(
            eng.mail_send(
                outsider.session,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "sibling".into(),
            )
            .await,
            &outsider.path,
            &format!("direct:{handle}"),
        );
        eng.update_session_channel_policy(
            root,
            hya_tool::ChannelPolicySnapshot {
                dm_parent: 0,
                ..Default::default()
            },
        );
        assert_mail_rejected(
            eng.mail_send(
                root,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "policy".into(),
            )
            .await,
            "main",
            &channel,
        );
        assert_eq!(eng.replay(root).await.unwrap().len(), before);
        assert!(
            hya_tool::extension::team_projection(&eng.read_projection(root).await.unwrap())
                .archived
                .contains_key(&handle)
        );
        eng.update_session_channel_policy(root, allowed);

        hooks.block.store(true, std::sync::atomic::Ordering::SeqCst);
        let reviving = tokio::spawn({
            let eng = eng.clone();
            async move {
                eng.mail_send(
                    root,
                    MailEndpoint::Handle("resident-1".into()),
                    MailKind::Message,
                    "episode two".into(),
                )
                .await
            }
        });
        hooks.entered.notified().await;
        let proj = eng.read_projection(root).await.unwrap();
        let team = hya_tool::extension::team_projection(&proj);
        assert!(team.roster.contains_key(&handle) && !team.archived.contains_key(&handle));
        let ch_res = eng
            .mail_send(
                root,
                MailEndpoint::Channel(channel.clone()),
                MailKind::Message,
                "during revival".into(),
            )
            .await
            .unwrap();
        let h_res = eng
            .mail_send(
                root,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "handle during revival".into(),
            )
            .await
            .unwrap();
        let delivered = inbox(&eng, root, &handle).await;
        let restarts = eng
            .replay(root)
            .await
            .unwrap()
            .iter()
            .filter(|e| {
                matches!(
                    hya_tool::extension::ExtensionEvent::from_event(&e.event),
                    Some(hya_tool::extension::ExtensionEvent::AgentRestarted { .. })
                )
            })
            .count();
        hooks.release.notify_one();
        assert_eq!(reviving.await.unwrap().unwrap().recipients, 1);
        drop(lead_turn);
        sup.drain(hya_proto::FinishCause::Shutdown, Duration::from_secs(2))
            .await;
        assert_eq!(
            (ch_res.to, ch_res.recipients, h_res.recipients, restarts),
            (MailEndpoint::Channel(channel), 2, 2, 1)
        );
        assert_eq!(
            delivered,
            ["episode two", "during revival", "handle during revival"]
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn resident_stop_commits_before_stale_alias_send_rechecks_the_dm() {
    let db_path = std::env::temp_dir()
        .join(format!("hya-core-mailbox-stop-{}.db", SessionId::new()))
        .to_string_lossy()
        .into_owned();
    let make = |store: SessionStore| {
        let (perm, _) = PermissionPlane::new(PermissionRules::new(Vec::new()));
        SessionEngine::new(
            store,
            Arc::new(ProviderRouter::new()),
            local_test_runtime(),
            perm,
            EventBus::default(),
        )
    };
    let primary = Arc::new(make(SessionStore::connect(&db_path).await.unwrap()));
    let root = root_team(&primary).await;
    let agent = AgentSpec {
        name: AgentName::new("resident"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: PathBuf::from("."),
        reasoning: None,
    };
    let b = primary.bind_runtime(&agent.workdir).unwrap();
    let sup = ResidentSupervisor::start(primary.clone());
    let (child, handle) = sup
        .spawn_resident(
            root,
            agent,
            (
                b.clone(),
                Arc::from([]),
                b.agent_resource_policy("resident").unwrap(),
                None,
            ),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();

    let (entered, release) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    let sender = Arc::new(
        make(SessionStore::connect(&db_path).await.unwrap())
            .with_mail_pre_append_gate(MailPreAppendGate::new(entered.clone(), release.clone())),
    );
    let sb = sender.bind_runtime(std::path::Path::new(".")).unwrap();
    sender
        .capture_session_bundle_hooks(root, &sb, "build")
        .await;
    let send = tokio::spawn({
        let h = handle.clone();
        async move {
            sender
                .mail_send(
                    root,
                    MailEndpoint::Handle(h),
                    MailKind::Message,
                    "stale direct mail".into(),
                )
                .await
        }
    });
    entered.notified().await;

    use crate::store_bridge::SessionStoreExt as _;
    sup.end_member(root, &handle).await.unwrap();
    assert!(
        !primary
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    release.notify_one();
    assert!(matches!(
        send.await.unwrap(),
        Err(CoreError::MailRevivalRequired { sender, channel, target })
            if sender == "main" && channel.starts_with("DM-") && target == handle
    ));
    assert!(
        !primary
            .replay(root)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(
                hya_tool::extension::ExtensionEvent::from_event(&e.event),
                Some(hya_tool::extension::ExtensionEvent::MailSent { .. })
            ))
    );
}
