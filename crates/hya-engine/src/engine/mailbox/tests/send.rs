use super::*;

#[tokio::test]
async fn mail_auth_non_member_cannot_send_to_dm_through_engine_and_plane() {
    let engine = Arc::new(engine().await);
    let org = org(&engine).await;
    mint_dm(&engine, org.root, "main", &org.lead_1.path).await;
    let channel = dm_channel_between(
        &engine.read_projection(org.root).await.unwrap(),
        "main",
        &org.lead_1.path,
    )
    .unwrap();
    let before = engine.replay(org.root).await.unwrap().len();
    assert_mail_rejected(
        engine
            .mail_send(
                org.lead_2.session,
                MailEndpoint::Channel(channel.clone()),
                MailKind::Message,
                "intrusion".into(),
            )
            .await,
        &org.lead_2.path,
        &channel,
    );
    let (plane, rx) = hya_tool::MailboxPlane::new();
    let service = tokio::spawn(crate::run_mailbox_service(
        engine.clone(),
        rx,
        tokio_util::sync::CancellationToken::new(),
    ));
    let result = plane
        .for_session(org.lead_2.session)
        .with_channel_policy(engine.session_channel_policy(org.lead_2.session).unwrap())
        .send(
            MailEndpoint::Channel(channel.clone()),
            MailKind::Message,
            "intrusion".into(),
        )
        .await;
    drop(plane);
    service.await.unwrap();
    assert!(
        matches!(result, Err(hya_tool::MailboxError::Rejected(ref message))
        if message.contains(&org.lead_2.path) && message.contains(&channel)),
        "{result:?}"
    );

    let (plane, rx) = hya_tool::MailboxPlane::new();
    let service = tokio::spawn(crate::run_mailbox_service(
        engine.clone(),
        rx,
        tokio_util::sync::CancellationToken::new(),
    ));
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let ctx = hya_tool::ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        permission,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        workflows: hya_tool::WorkflowPlane::disconnected(),
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: plane
            .for_session(org.lead_2.session)
            .with_channel_policy(engine.session_channel_policy(org.lead_2.session).unwrap()),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(org.lead_2.session),
        parent_session: Some(org.root),
        skills: Default::default(),
        artifacts: Default::default(),
        agents: Default::default(),
        websearch: Default::default(),
        lsp: Default::default(),
        formatter: Default::default(),
        workdir: PathBuf::from("."),
        roots: vec![PathBuf::from(".")],
        cancel: tokio_util::sync::CancellationToken::new(),
    };
    let result = ToolRegistry::builtins()
        .get("send")
        .unwrap()
        .execute(
            &ctx,
            serde_json::json!({"channel": channel, "body": "intrusion"}),
        )
        .await;
    drop(ctx);
    drop(plane);
    service.await.unwrap();
    assert!(
        matches!(result, Err(hya_tool::ToolError::Input(ref message))
        if message.contains(&org.lead_2.path) && message.contains(&channel)),
        "{result:?}"
    );
    assert_eq!(engine.replay(org.root).await.unwrap().len(), before);
}

#[tokio::test]
async fn mail_auth_missing_channel_is_typed_through_engine() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    engine.ensure_root_registered(root).await.unwrap();
    let before = engine.replay(root).await.unwrap().len();
    assert_mail_rejected(
        engine
            .mail_send(
                root,
                MailEndpoint::Channel("DM-missing1".into()),
                MailKind::Message,
                "hello".into(),
            )
            .await,
        "main",
        "DM-missing1",
    );
    assert_eq!(engine.replay(root).await.unwrap().len(), before);
}

#[tokio::test]
async fn mail_auth_dm_membership_and_direction_through_engine() {
    let engine = engine().await;
    let org = org(&engine).await;
    mint_dm(&engine, org.root, "main", &org.lead_1.path).await;
    let channel = dm_channel_between(
        &engine.read_projection(org.root).await.unwrap(),
        "main",
        &org.lead_1.path,
    )
    .unwrap();
    let receipt = engine
        .mail_send(
            org.root,
            MailEndpoint::Channel(channel.clone()),
            MailKind::Message,
            "hello".into(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.recipients, 2);
    assert_eq!(inbox(&engine, org.root, &org.lead_1.path).await, ["hello"]);
    engine.update_session_channel_policy(
        org.root,
        hya_tool::ChannelPolicySnapshot {
            dm_parent: 0,
            ..Default::default()
        },
    );
    let before = engine.replay(org.root).await.unwrap().len();
    assert_mail_rejected(
        engine
            .mail_send(
                org.root,
                MailEndpoint::Channel(channel.clone()),
                MailKind::Message,
                "blocked".into(),
            )
            .await,
        "main",
        &channel,
    );
    assert_eq!(engine.replay(org.root).await.unwrap().len(), before);
}

#[tokio::test]
async fn mail_auth_group_member_cannot_announce_as_leader_through_engine() {
    let engine = engine().await;
    let org = org(&engine).await;
    let channel = engine
        .read_projection(org.root)
        .await
        .unwrap()
        .team
        .channels
        .into_iter()
        .find(|(_, state)| state.unit.as_deref() == Some("main"))
        .unwrap()
        .0;
    let before = engine.replay(org.root).await.unwrap().len();
    assert_mail_rejected(
        engine
            .mail_send(
                org.lead_1.session,
                MailEndpoint::Channel(channel.clone()),
                MailKind::Announcement,
                "impersonation".into(),
            )
            .await,
        &org.lead_1.path,
        &channel,
    );
    assert_eq!(engine.replay(org.root).await.unwrap().len(), before);
}

#[tokio::test]
async fn mail_auth_relative_parent_cannot_bypass_child_policy() {
    let engine = engine().await;
    let org = org(&engine).await;
    engine.update_session_channel_policy(
        org.worker_1.session,
        hya_tool::ChannelPolicySnapshot {
            dm_child: 0,
            dm_parent: 1,
            ..Default::default()
        },
    );
    let before = engine.replay(org.root).await.unwrap().len();
    assert_mail_rejected(
        engine
            .mail_send(
                org.worker_1.session,
                MailEndpoint::Handle("lead-1".into()),
                MailKind::Message,
                "blocked".into(),
            )
            .await,
        &org.worker_1.path,
        &engine
            .read_projection(org.root)
            .await
            .unwrap()
            .team
            .resolve_dm(&org.worker_1.path, "lead-1")
            .unwrap(),
    );
    assert_eq!(engine.replay(org.root).await.unwrap().len(), before);
}

#[tokio::test]
async fn mail_auth_relative_parent_uses_child_policy() {
    let engine = engine().await;
    let org = org(&engine).await;
    engine.update_session_channel_policy(
        org.worker_1.session,
        hya_tool::ChannelPolicySnapshot {
            dm_child: 1,
            dm_parent: 0,
            ..Default::default()
        },
    );
    engine
        .mail_send(
            org.worker_1.session,
            MailEndpoint::Handle("lead-1".into()),
            MailKind::Message,
            "status up".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        inbox(&engine, org.root, &org.lead_1.path).await,
        ["status up"]
    );
}

#[tokio::test]
async fn mail_auth_group_survives_released_claim_and_count_matches_replay() {
    let engine = engine().await;
    let org = org(&engine).await;
    engine
        .store()
        .release_claim(&org.lead_2._claim)
        .await
        .unwrap();
    let receipt = engine
        .mail_announce(org.root, "all hands".into())
        .await
        .unwrap();
    let replay = hya_proto::Projection::from_events(&engine.replay(org.root).await.unwrap());
    let delivered = replay
        .team
        .inboxes
        .values()
        .filter(|inbox| {
            inbox
                .iter()
                .any(|index| replay.team.mail[*index].body == "all hands")
        })
        .count();
    assert_eq!(receipt.recipients, delivered);
    assert_eq!(receipt.recipients, 3);
    assert_eq!(
        inbox(&engine, org.root, &org.lead_1.path).await,
        ["all hands"]
    );
    assert_eq!(
        inbox(&engine, org.root, &org.lead_2.path).await,
        ["all hands"]
    );

    mint_dm(&engine, org.root, "main", &org.lead_2.path).await;
    let channel = dm_channel_between(
        &engine.read_projection(org.root).await.unwrap(),
        "main",
        &org.lead_2.path,
    )
    .unwrap();
    assert_mail_rejected(
        engine
            .mail_send(
                org.root,
                MailEndpoint::Channel(channel.clone()),
                MailKind::Message,
                "private".into(),
            )
            .await,
        "main",
        &channel,
    );
    assert_mail_rejected(
        engine
            .mail_send(
                org.root,
                MailEndpoint::Handle(org.lead_2.path.clone()),
                MailKind::Message,
                "private".into(),
            )
            .await,
        "main",
        &channel,
    );
}

#[tokio::test]
async fn mail_auth_terminal_member_is_skipped_by_group_but_rejected_by_dm() {
    let engine = engine().await;
    let org = org(&engine).await;
    engine
        .record_agent_activity(
            org.root,
            org.lead_2.path.clone(),
            RosterStatus::Failed,
            None,
        )
        .await
        .unwrap();
    let receipt = engine
        .mail_announce(org.root, "healthy recipients".into())
        .await
        .unwrap();
    let replay = hya_proto::Projection::from_events(&engine.replay(org.root).await.unwrap());
    assert_eq!(
        receipt.recipients,
        replay
            .team
            .inboxes
            .values()
            .filter(|inbox| {
                inbox
                    .iter()
                    .any(|index| replay.team.mail[*index].body == "healthy recipients")
            })
            .count()
    );
    assert_eq!(receipt.recipients, 2);
    assert!(inbox(&engine, org.root, &org.lead_2.path).await.is_empty());
    mint_dm(&engine, org.root, "main", &org.lead_2.path).await;
    let channel = dm_channel_between(
        &engine.read_projection(org.root).await.unwrap(),
        "main",
        &org.lead_2.path,
    )
    .unwrap();
    assert_mail_rejected(
        engine
            .mail_send(
                org.root,
                MailEndpoint::Channel(channel.clone()),
                MailKind::Message,
                "private".into(),
            )
            .await,
        "main",
        &channel,
    );
}

/// A session that was never spawned/registered cannot use the mailbox — its
/// send is rejected rather than silently delivered under a bogus handle.
#[tokio::test]
async fn unregistered_non_root_sender_is_rejected() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let stranger = engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("reviewer"),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let result = engine
        .mail_send(
            stranger,
            MailEndpoint::Handle("main".to_string()),
            MailKind::Message,
            "hi".to_string(),
        )
        .await;
    assert!(
        matches!(result, Err(CoreError::Invalid(ref message)) if message.contains("channel policy snapshot missing")),
        "a session without an admitted binding must fail closed: {result:?}"
    );
}

#[tokio::test]
async fn bundle_channel_policy_can_restrict_real_send_without_granting() {
    let runtime = tests_support::runtime_with_channel_restriction(
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }",
    );
    let engine = engine_with_runtime(runtime).await;
    let org = org(&engine).await;
    let binding = engine.bind_runtime(std::path::Path::new(".")).unwrap();
    let policy = crate::ChannelPolicy::from_binding(&binding)
        .unwrap()
        .snapshot_for("reviewer");
    let result = engine
        .mail_send_for_actor_with_policy(
            org.worker_1.session,
            MailEndpoint::Handle(org.lead_1.path),
            MailKind::Message,
            "blocked".to_string(),
            None,
            Some(policy),
        )
        .await;
    assert!(matches!(
        result,
        Err(CoreError::Store(
            hya_store::StoreError::MailAuthorizationRejected { .. }
        ))
    ));
}

#[tokio::test]
async fn direct_mail_to_unknown_handle_is_rejected_before_append() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    engine.ensure_root_registered(root).await.unwrap();

    let before_len = engine.replay(root).await.unwrap().len();
    let result = engine
        .mail_send(
            root,
            MailEndpoint::Handle("missing-1".to_string()),
            MailKind::Message,
            "hi".to_string(),
        )
        .await;
    assert_mail_rejected(result, "main", "direct:missing-1");

    let after_len = engine.replay(root).await.unwrap().len();
    assert_eq!(after_len, before_len);
    let projection = engine.read_projection(root).await.unwrap();
    assert!(!projection.team.inboxes.contains_key("missing-1"));
}

/// A sibling is reachable; an agent in another unit is not — and the refused
/// send leaves NOTHING in the log (AC1).    /// Skip-level is closed in both directions: a worker cannot reach the root,
/// and the root cannot reach a grandchild.
#[tokio::test]
async fn grandparent_and_grandchild_are_both_refused() {
    let engine = engine().await;
    let org = org(&engine).await;

    let up = engine
        .mail_send(
            org.worker_1.session,
            MailEndpoint::Handle("main".to_string()),
            MailKind::Message,
            "skipping levels".to_string(),
        )
        .await;
    assert!(
        up.is_err(),
        "a worker must not reach the team root directly"
    );

    let down = engine
        .mail_send(
            org.root,
            MailEndpoint::Handle(org.worker_1.path.clone()),
            MailKind::Message,
            "reaching past lead-1".to_string(),
        )
        .await;
    assert!(down.is_err(), "the root must not reach a grandchild");
}

/// The `^parent` sentinel resolves the DM channel with the direct parent
/// instead of the parent *handle*, so a lazily registered root (the run-2
/// bounce: "main is not a teammate you can message") still receives the
/// mail through the channel that exists since registration time.
#[tokio::test]
async fn parent_sentinel_routes_through_the_dm_channel_when_root_is_lazy() {
    let engine = engine().await;
    // Deliberately NO ensure_root_registered here: the roster has no `main`.
    let root = root_team(&engine).await;
    let child = register_child(&engine, root, root, "main", "general-1").await;
    mint_dm(&engine, root, "main", &child.path).await;

    let receipt = engine
        .mail_send(
            child.session,
            MailEndpoint::Handle("^parent".to_string()),
            MailKind::Message,
            "report from the field".to_string(),
        )
        .await
        .expect("the DM channel with the parent must absorb the sentinel");

    assert!(
        matches!(receipt.to, MailEndpoint::Channel(ref channel) if channel.starts_with("DM-")),
        "the sentinel resolved to the DM channel, got {:?}",
        receipt.to
    );
    assert_eq!(
        inbox(&engine, root, "main").await,
        vec!["report from the field".to_string()],
        "the parent's inbox receives the mail"
    );
}

/// At depth ≥ 2 the sentinel addresses the DIRECT parent's DM — never the
/// root — and still succeeds even though the root is registered.
#[tokio::test]
async fn parent_sentinel_at_depth_two_targets_the_direct_parent_dm() {
    let engine = engine().await;
    let org = org(&engine).await;
    mint_dm(&engine, org.root, &org.lead_1.path, &org.worker_1.path).await;

    let receipt = engine
        .mail_send(
            org.worker_1.session,
            MailEndpoint::Handle("^".to_string()),
            MailKind::Message,
            "status up".to_string(),
        )
        .await
        .expect("depth-two sentinel delivery reaches the direct parent");

    assert!(matches!(receipt.to, MailEndpoint::Channel(_)));
    assert_eq!(
        inbox(&engine, org.root, &org.lead_1.path).await,
        vec!["status up".to_string()],
        "lead-1 (the direct parent) receives it; main does not"
    );
    assert!(inbox(&engine, org.root, "main").await.is_empty());
}

/// A relative leaf and the full canonical path name the same agent (AC2).    /// A relative leaf resolves inside the sender's own unit even when another
/// unit holds an agent with the same leaf.    /// Announce reaches DIRECT reports and stops (AC6). A grandchild hears it
/// only after the intermediate leader announces in turn.
#[tokio::test]
async fn announce_reaches_direct_reports_only() {
    let engine = engine().await;
    let org = org(&engine).await;

    let receipt = engine
        .mail_announce(org.root, "all hands".to_string())
        .await
        .unwrap();
    assert!(
        receipt.recipients >= 2,
        "the root leads exactly two agents: {receipt:?}"
    );

    assert_eq!(
        inbox(&engine, org.root, &org.lead_1.path).await,
        vec!["all hands".to_string()]
    );
    assert_eq!(
        inbox(&engine, org.root, &org.lead_2.path).await,
        vec!["all hands".to_string()]
    );
    for grandchild in [&org.worker_1, &org.worker_2, &org.other_worker] {
        assert!(
            inbox(&engine, org.root, &grandchild.path).await.is_empty(),
            "{} must NOT hear the root's announcement",
            grandchild.path
        );
    }

    // The relay: lead-1 passes it down, and only ITS unit hears it.
    engine
        .mail_announce(org.lead_1.session, "all hands".to_string())
        .await
        .unwrap();
    assert_eq!(
        inbox(&engine, org.root, &org.worker_1.path).await,
        vec!["all hands".to_string()]
    );
    assert_eq!(
        inbox(&engine, org.root, &org.worker_2.path).await,
        vec!["all hands".to_string()],
        "every sibling in lead-1's unit receives the announcement"
    );
    assert!(
        inbox(&engine, org.root, &org.other_worker.path)
            .await
            .is_empty(),
        "lead-2's unit is not reached by lead-1's announcement"
    );
}

/// `send` with no channel routes by role: a leader broadcasts on its unit
/// group channel; a subordinate DMs its parent; the delivery outcomes
/// match the explicit endpoint forms.
#[tokio::test]
async fn send_default_routes_by_role() {
    let engine = engine().await;
    let org = org(&engine).await;

    // The root leads a unit: default send behaves like the broadcast.
    let receipt = engine
        .mail_send_default_for_actor_with_policy(
            org.root,
            "default all hands".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        receipt.recipients >= 2,
        "root default reaches its unit: {receipt:?}"
    );
    assert!(
        matches!(receipt.to, MailEndpoint::Channel(_)),
        "a leader's default channel is its unit group pipe: {receipt:?}"
    );
    assert_eq!(
        inbox(&engine, org.root, &org.lead_1.path).await,
        vec!["default all hands".to_string()]
    );

    // A leaf defaults to its parent DM.
    let receipt = engine
        .mail_send_default_for_actor_with_policy(
            org.worker_1.session,
            "leaf default up".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(receipt.recipients, 2);
    assert!(
        matches!(receipt.to, MailEndpoint::Channel(ref id) if id.starts_with("DM-")),
        "a subordinate's default channel is its parent DM: {receipt:?}"
    );
    assert_eq!(
        inbox(&engine, org.root, &org.lead_1.path).await,
        vec![
            "default all hands".to_string(),
            "leaf default up".to_string()
        ]
    );
}

/// A posting on a group channel is stamped as an announcement by the
/// channel's nature, not by the caller's kind hint.
#[tokio::test]
async fn group_channel_posts_derive_announcement_kind() {
    let engine = engine().await;
    let org = org(&engine).await;

    let group = engine
        .read_projection(org.root)
        .await
        .unwrap()
        .team
        .channels
        .into_iter()
        .find(|(_, state)| {
            state.kind == hya_proto::ChannelKind::Group && state.unit.as_deref() == Some("main")
        })
        .unwrap()
        .0;
    let receipt = engine
        .mail_send(
            org.root,
            MailEndpoint::Channel(group),
            MailKind::Message,
            "typed as channel".into(),
        )
        .await
        .unwrap();
    assert!(receipt.recipients >= 1);
    assert!(
        matches!(receipt.to, MailEndpoint::Channel(_)),
        "group delivery reports the channel: {receipt:?}"
    );
}

/// An agent that leads nobody has nothing to announce to.
#[tokio::test]
async fn announce_from_a_leaf_agent_is_refused() {
    let engine = engine().await;
    let org = org(&engine).await;
    let result = engine
        .mail_announce(org.worker_1.session, "listen up".to_string())
        .await;
    assert!(result.is_err(), "a leaf agent leads no one: {result:?}");
}

/// R5 leaves a working path: relaying through the common ancestor delivers
/// a message the direct send refuses (AC9).
#[tokio::test]
async fn cross_unit_relay_through_the_common_ancestor_arrives() {
    let engine = engine().await;
    let org = org(&engine).await;

    // worker-1 -> lead-1 -> main -> lead-2 -> lead-2's worker-1.
    let hops = [
        (org.worker_1.session, "lead-1"),
        (org.lead_1.session, "main"),
        (org.root, "lead-2"),
        (org.lead_2.session, "worker-1"),
    ];
    for (from, to) in hops {
        engine
            .mail_send(
                from,
                MailEndpoint::Handle(to.to_string()),
                MailKind::Message,
                "relayed payload".to_string(),
            )
            .await
            .unwrap_or_else(|e| panic!("hop to {to} must be in scope: {e:?}"));
    }

    assert_eq!(
        inbox(&engine, org.root, &org.other_worker.path).await,
        vec!["relayed payload".to_string()],
        "the message crossed units, one in-scope hop at a time"
    );
}
