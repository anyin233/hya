use super::*;

async fn send(
    engine: &SessionEngine,
    from: SessionId,
    to: MailEndpoint,
    body: &str,
) -> Result<MailReceipt, CoreError> {
    engine
        .mail_send(from, to, MailKind::Message, body.into())
        .await
}

async fn dm_id(engine: &SessionEngine, root: SessionId, a: &str, b: &str) -> String {
    dm_channel_between(&engine.read_projection(root).await.unwrap(), a, b).unwrap()
}

#[tokio::test]
async fn mail_auth_non_member_rejected_across_engine_plane_and_send_tool() {
    let eng = Arc::new(engine().await);
    let o = org(&eng).await;
    mint_dm(&eng, o.root, "main", &o.lead_1.path).await;
    let ch = dm_id(&eng, o.root, "main", &o.lead_1.path).await;
    let before = eng.replay(o.root).await.unwrap().len();
    assert_mail_rejected(
        send(
            &eng,
            o.lead_2.session,
            MailEndpoint::Channel(ch.clone()),
            "intrusion",
        )
        .await,
        &o.lead_2.path,
        &ch,
    );

    let (plane, rx) = hya_tool::MailboxPlane::new();
    let svc = tokio::spawn(crate::run_mailbox_service(
        eng.clone(),
        rx,
        tokio_util::sync::CancellationToken::new(),
    ));
    let sp = plane
        .for_session(o.lead_2.session)
        .with_channel_policy(eng.session_channel_policy(o.lead_2.session).unwrap());
    let plane_res = sp
        .send(
            MailEndpoint::Channel(ch.clone()),
            MailKind::Message,
            "intrusion".into(),
        )
        .await;
    assert!(
        matches!(plane_res, Err(hya_tool::MailboxError::Rejected(ref m)) if m.contains(&o.lead_2.path) && m.contains(&ch))
    );

    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let ctx = hya_tool::ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        permission,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        workflows: hya_tool::WorkflowPlane::disconnected(),
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: sp,
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(o.lead_2.session),
        parent_session: Some(o.root),
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
    let tool_res = hya_tool_loader::builtins()
        .get("send")
        .unwrap()
        .execute(
            &ctx,
            serde_json::json!({"channel": ch, "body": "intrusion"}),
        )
        .await;
    drop((ctx, plane));
    svc.await.unwrap();
    assert!(
        matches!(tool_res, Err(hya_tool::ToolError::Input(ref m)) if m.contains(&o.lead_2.path) && m.contains(&ch))
    );
    assert_eq!(eng.replay(o.root).await.unwrap().len(), before);
}

#[tokio::test]
async fn mail_auth_rejects_missing_unknown_skip_level_and_unregistered_senders() {
    let eng = engine().await;
    let o = org(&eng).await;
    let before = eng.replay(o.root).await.unwrap().len();

    for (ep, want_ch) in [
        (MailEndpoint::Channel("DM-missing1".into()), "DM-missing1"),
        (MailEndpoint::Handle("missing-1".into()), "direct:missing-1"),
    ] {
        assert_mail_rejected(send(&eng, o.root, ep, "hi").await, "main", want_ch);
    }
    assert!(
        send(
            &eng,
            o.worker_1.session,
            MailEndpoint::Handle("main".into()),
            "skip up"
        )
        .await
        .is_err()
    );
    assert!(
        send(
            &eng,
            o.root,
            MailEndpoint::Handle(o.worker_1.path.clone()),
            "skip down"
        )
        .await
        .is_err()
    );
    assert_eq!(eng.replay(o.root).await.unwrap().len(), before);
    assert!(
        !hya_tool::extension::team_projection(&eng.read_projection(o.root).await.unwrap())
            .inboxes
            .contains_key("missing-1")
    );

    let stranger = eng
        .create(CreateSession {
            parent: Some(o.root),
            agent: AgentName::new("reviewer"),
            model: ModelRef::new("fake"),
            workdir: ".".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    assert!(matches!(
        send(&eng, stranger, MailEndpoint::Handle("main".into()), "hi").await,
        Err(CoreError::Invalid(ref m)) if m.contains("channel policy snapshot missing")
    ));
}

#[tokio::test]
async fn mail_auth_dm_direction_relative_parent_and_bundle_policy_restrictions() {
    let eng = engine().await;
    let o = org(&eng).await;
    let dm = dm_id(&eng, o.root, "main", &o.lead_1.path).await;
    assert_eq!(
        send(&eng, o.root, MailEndpoint::Channel(dm.clone()), "hello")
            .await
            .unwrap()
            .recipients,
        2
    );
    assert_eq!(inbox(&eng, o.root, &o.lead_1.path).await, ["hello"]);

    eng.update_session_channel_policy(
        o.root,
        hya_tool::ChannelPolicySnapshot {
            dm_parent: 0,
            ..Default::default()
        },
    );
    let before = eng.replay(o.root).await.unwrap().len();
    assert_mail_rejected(
        send(&eng, o.root, MailEndpoint::Channel(dm.clone()), "blocked").await,
        "main",
        &dm,
    );
    assert_eq!(eng.replay(o.root).await.unwrap().len(), before);

    // Relative parent handle obeys child policy bit, not parent bit.
    let wdm = hya_tool::extension::team_projection(&eng.read_projection(o.root).await.unwrap())
        .resolve_dm(&o.worker_1.path, "lead-1")
        .unwrap();
    eng.update_session_channel_policy(
        o.worker_1.session,
        hya_tool::ChannelPolicySnapshot {
            dm_child: 0,
            dm_parent: 1,
            ..Default::default()
        },
    );
    assert_mail_rejected(
        send(
            &eng,
            o.worker_1.session,
            MailEndpoint::Handle("lead-1".into()),
            "blocked",
        )
        .await,
        &o.worker_1.path,
        &wdm,
    );
    eng.update_session_channel_policy(
        o.worker_1.session,
        hya_tool::ChannelPolicySnapshot {
            dm_child: 1,
            dm_parent: 0,
            ..Default::default()
        },
    );
    send(
        &eng,
        o.worker_1.session,
        MailEndpoint::Handle("lead-1".into()),
        "status up",
    )
    .await
    .unwrap();
    assert_eq!(
        inbox(&eng, o.root, &o.lead_1.path).await,
        ["hello", "status up"]
    );

    // Bundle channel restriction blocks real send without granting new capabilities.
    let restricted = engine_with_runtime(tests_support::runtime_with_channel_restriction(
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }",
    )).await;
    let ro = org(&restricted).await;
    let policy = crate::ChannelPolicy::from_binding(
        &restricted.bind_runtime(std::path::Path::new(".")).unwrap(),
    )
    .unwrap()
    .snapshot_for("reviewer");
    assert!(matches!(
        restricted
            .mail_send_for_actor_with_policy(
                ro.worker_1.session,
                MailEndpoint::Handle(ro.lead_1.path),
                MailKind::Message,
                "blocked".into(),
                None,
                Some(policy)
            )
            .await,
        Err(CoreError::MailAuthorizationRejected { .. })
    ));
}

#[tokio::test]
async fn mail_auth_group_vs_dm_with_released_claim_and_terminal_member() {
    // 1) Released claim still receives group announce, but DM (channel & handle) rejects.
    let eng = engine().await;
    let o = org(&eng).await;
    use crate::store_bridge::SessionStoreExt as _;
    eng.store().release_claim(&o.lead_2._claim).await.unwrap();
    let r = eng.mail_announce(o.root, "all hands".into()).await.unwrap();
    let replay = hya_proto::Projection::from_events_with(
        &eng.replay(o.root).await.unwrap(),
        &hya_tool::extension::projection_reducers(),
    );
    let team = hya_tool::extension::team_projection(&replay);
    let delivered = team
        .inboxes
        .values()
        .filter(|i| {
            i.iter()
                .any(|idx| team.mail[*idx].body == "all hands")
        })
        .count();
    assert_eq!((r.recipients, delivered), (3, 3));
    assert_eq!(inbox(&eng, o.root, &o.lead_2.path).await, ["all hands"]);
    let dm = dm_id(&eng, o.root, "main", &o.lead_2.path).await;
    for ep in [
        MailEndpoint::Channel(dm.clone()),
        MailEndpoint::Handle(o.lead_2.path.clone()),
    ] {
        assert_mail_rejected(send(&eng, o.root, ep, "private").await, "main", &dm);
    }

    // 2) Failed member is skipped by group announce AND rejected by DM.
    let eng2 = engine().await;
    let o2 = org(&eng2).await;
    eng2.record_agent_activity(o2.root, o2.lead_2.path.clone(), RosterStatus::Failed, None)
        .await
        .unwrap();
    assert_eq!(
        eng2.mail_announce(o2.root, "healthy".into())
            .await
            .unwrap()
            .recipients,
        2
    );
    assert!(inbox(&eng2, o2.root, &o2.lead_2.path).await.is_empty());
    let dm2 = dm_id(&eng2, o2.root, "main", &o2.lead_2.path).await;
    assert_mail_rejected(
        send(
            &eng2,
            o2.root,
            MailEndpoint::Channel(dm2.clone()),
            "private",
        )
        .await,
        "main",
        &dm2,
    );
}

#[tokio::test]
async fn parent_sentinel_routes_direct_parent_dm_for_lazy_root_and_depth_two() {
    let lazy = engine().await;
    let root = root_team(&lazy).await;
    let child = register_child(&lazy, root, root, "main", "general-1").await;
    let r1 = send(
        &lazy,
        child.session,
        MailEndpoint::Handle("^parent".into()),
        "report from field",
    )
    .await
    .unwrap();
    assert!(matches!(r1.to, MailEndpoint::Channel(ref id) if id.starts_with("DM-")));
    assert_eq!(inbox(&lazy, root, "main").await, ["report from field"]);

    let eng = engine().await;
    let o = org(&eng).await;
    let r2 = send(
        &eng,
        o.worker_1.session,
        MailEndpoint::Handle("^".into()),
        "status up",
    )
    .await
    .unwrap();
    assert!(matches!(r2.to, MailEndpoint::Channel(_)));
    assert_eq!(inbox(&eng, o.root, &o.lead_1.path).await, ["status up"]);
    assert!(inbox(&eng, o.root, "main").await.is_empty());
}

#[tokio::test]
async fn announce_and_group_posts_reach_direct_reports_and_reject_impersonation_or_leaf() {
    let eng = engine().await;
    let o = org(&eng).await;
    let group = hya_tool::extension::team_projection(&eng.read_projection(o.root).await.unwrap())
        .channels
        .into_iter()
        .find(|(_, s)| s.kind == hya_proto::ChannelKind::Group && s.unit.as_deref() == Some("main"))
        .unwrap()
        .0;

    let before = eng.replay(o.root).await.unwrap().len();
    assert_mail_rejected(
        eng.mail_send(
            o.lead_1.session,
            MailEndpoint::Channel(group.clone()),
            MailKind::Announcement,
            "impersonation".into(),
        )
        .await,
        &o.lead_1.path,
        &group,
    );
    assert_eq!(eng.replay(o.root).await.unwrap().len(), before);
    assert!(
        eng.mail_announce(o.worker_1.session, "listen up".into())
            .await
            .is_err()
    );

    let typed = send(&eng, o.root, MailEndpoint::Channel(group), "all hands")
        .await
        .unwrap();
    assert!(typed.recipients >= 2 && matches!(typed.to, MailEndpoint::Channel(_)));
    assert_eq!(inbox(&eng, o.root, &o.lead_1.path).await, ["all hands"]);
    assert_eq!(inbox(&eng, o.root, &o.lead_2.path).await, ["all hands"]);
    for gc in [&o.worker_1, &o.worker_2, &o.other_worker] {
        assert!(inbox(&eng, o.root, &gc.path).await.is_empty());
    }
    eng.mail_announce(o.lead_1.session, "unit relay".into())
        .await
        .unwrap();
    assert_eq!(inbox(&eng, o.root, &o.worker_1.path).await, ["unit relay"]);
    assert_eq!(inbox(&eng, o.root, &o.worker_2.path).await, ["unit relay"]);
    assert!(inbox(&eng, o.root, &o.other_worker.path).await.is_empty());
}

#[tokio::test]
async fn send_default_routes_by_role() {
    let eng = engine().await;
    let o = org(&eng).await;
    let lead = eng
        .mail_send_default_for_actor_with_policy(o.root, "default all hands".into(), None, None)
        .await
        .unwrap();
    assert!(lead.recipients >= 2 && matches!(lead.to, MailEndpoint::Channel(_)));
    assert_eq!(
        inbox(&eng, o.root, &o.lead_1.path).await,
        ["default all hands"]
    );

    let leaf = eng
        .mail_send_default_for_actor_with_policy(
            o.worker_1.session,
            "leaf default up".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(leaf.recipients, 2);
    assert!(matches!(leaf.to, MailEndpoint::Channel(ref id) if id.starts_with("DM-")));
    assert_eq!(
        inbox(&eng, o.root, &o.lead_1.path).await,
        ["default all hands", "leaf default up"]
    );
}

#[tokio::test]
async fn cross_unit_relay_through_common_ancestor_arrives() {
    let eng = engine().await;
    let o = org(&eng).await;
    for (from, to) in [
        (o.worker_1.session, "lead-1"),
        (o.lead_1.session, "main"),
        (o.root, "lead-2"),
        (o.lead_2.session, "worker-1"),
    ] {
        send(
            &eng,
            from,
            MailEndpoint::Handle(to.into()),
            "relayed payload",
        )
        .await
        .unwrap();
    }
    assert_eq!(
        inbox(&eng, o.root, &o.other_worker.path).await,
        ["relayed payload"]
    );
}
