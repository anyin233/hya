use super::*;

#[derive(Default)]
struct MailObserver(std::sync::Mutex<Vec<(SessionId, u64)>>);

impl crate::engine::TurnBoundaryObserver for MailObserver {
    fn mail_mark(&self, _: SessionId) -> Option<u64> {
        Some(7)
    }
    fn mail_consumed(&self, session: SessionId, mark: u64) {
        self.0.lock().unwrap().push((session, mark));
    }
    fn turn_released(&self, _: SessionId) {}
}

#[tokio::test]
async fn superseded_channel_reader_cannot_advance_the_inbox_cursor() {
    let engine = Arc::new(engine().await);
    let org = org(&engine).await;
    let caller = org.lead_1.session;
    let owner = hya_proto::OwnerRunId::new();
    engine.store().claim_runtime_owner(owner).unwrap();
    let old = engine
        .store()
        .recover_claim(caller, owner)
        .await
        .unwrap()
        .claim;
    engine
        .mail_send(
            org.root,
            MailEndpoint::Handle(org.lead_1.path.clone()),
            MailKind::Message,
            "owed to replacement".into(),
        )
        .await
        .unwrap();
    let channel = dm_channel_between(
        &engine.read_projection(org.root).await.unwrap(),
        "main",
        &org.lead_1.path,
    )
    .unwrap();
    engine.store().recover_claim(caller, owner).await.unwrap();
    let before = engine.replay(org.root).await.unwrap();
    let (plane, requests) = hya_tool::MailboxPlane::new();
    let stop = tokio_util::sync::CancellationToken::new();
    let service = tokio::spawn(crate::run_mailbox_service(
        engine.clone(),
        requests,
        stop.clone(),
    ));
    let result = plane
        .for_session_with_actor(caller, Some(old))
        .read_channel(channel, Some(10))
        .await;
    stop.cancel();
    service.await.unwrap();
    assert!(result.is_err(), "a superseded read consumed mail");
    assert_eq!(
        engine.replay(org.root).await.unwrap(),
        before,
        "the superseded read changed the replacement's inbox cursor"
    );
}

async fn cursor_cleanup_marks_only_incoming_work(read: bool) {
    let engine = Arc::new(engine().await);
    let org = org(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let observer = Arc::new(MailObserver::default());
    let erased: Arc<dyn crate::engine::TurnBoundaryObserver> = observer.clone();
    engine.set_turn_observer(Arc::downgrade(&erased));
    for (sender, target, incoming) in [
        (org.root, org.lead_1.path.clone(), false),
        (org.lead_1.session, "main".into(), true),
    ] {
        engine
            .mail_send(
                sender,
                MailEndpoint::Handle(target),
                MailKind::Message,
                "work".into(),
            )
            .await
            .unwrap();
        if read {
            let channel = dm_channel_between(
                &engine.read_projection(org.root).await.unwrap(),
                "main",
                &org.lead_1.path,
            )
            .unwrap();
            engine
                .read_channel_history(None, org.root, &channel, Some(10))
                .await
                .unwrap();
        } else {
            crate::member_wait::wait_for_members(
                None,
                &engine,
                &supervisor,
                org.root,
                hya_tool::WaitSpec::parse(&serde_json::json!({"timeout_secs": 0}), true).unwrap(),
                None,
            )
            .await
            .unwrap();
        }
        let projection = engine.read_projection(org.root).await.unwrap();
        assert_eq!(
            projection.team.roster["main"].resident_cursor,
            projection.team.inboxes["main"].len() as u64,
            "raw cursor must advance"
        );
        assert_eq!(
            *observer.0.lock().unwrap(),
            if incoming {
                vec![(org.root, 7)]
            } else {
                vec![]
            },
            "outgoing history must not acknowledge unrelated team work"
        );
    }
    supervisor
        .drain(
            hya_proto::FinishCause::Shutdown,
            std::time::Duration::from_secs(2),
        )
        .await;
}

#[tokio::test]
async fn wait_cursor_cleanup_marks_only_incoming_work() {
    cursor_cleanup_marks_only_incoming_work(false).await;
}

#[tokio::test]
async fn read_cursor_cleanup_marks_only_incoming_work() {
    cursor_cleanup_marks_only_incoming_work(true).await;
}

#[tokio::test]
async fn steer_denies_group_mail_after_member_archive() {
    let engine = engine().await;
    let org = org(&engine).await;
    engine
        .mail_announce(org.lead_1.session, "group mail before archive".into())
        .await
        .unwrap();
    let mut steer = engine
        .steer_mailbox_snapshot_with_policy(
            org.worker_1.session,
            engine.session_channel_policy(org.worker_1.session),
        )
        .await;
    engine
        .store()
        .append_event(
            org.root,
            &Event::AgentArchived {
                session: org.root,
                handle: org.worker_1.path,
                child: org.worker_1.session,
                reason: hya_proto::ArchiveReason::ArchivedByParent,
            },
        )
        .await
        .unwrap();
    assert!(steer.drain(&engine).await.unwrap().is_none());
    assert!(
        steer.take_acknowledgement().unwrap().1,
        "filtered incoming mail is acknowledged"
    );
}

#[tokio::test]
async fn steer_cursor_ack_marks_work_only_for_incoming_mail() {
    let engine = engine().await;
    let org = org(&engine).await;
    engine
        .mail_send(
            org.root,
            MailEndpoint::Handle(org.lead_1.path.clone()),
            MailKind::Message,
            "outgoing directive".into(),
        )
        .await
        .unwrap();
    // Both snapshots hide their notice, but only the child's cursor covers
    // incoming work. The parent's outgoing history must not settle work.
    for (session, handle, incoming) in [
        (org.root, "main", false),
        (org.lead_1.session, org.lead_1.path.as_str(), true),
    ] {
        let mut steer = engine
            .steer_mailbox_snapshot_with_policy(
                session,
                Some(hya_tool::ChannelPolicySnapshot::default()),
            )
            .await;
        assert!(steer.drain(&engine).await.unwrap().is_none());
        let (ack, consumed_incoming) = steer.take_acknowledgement().unwrap();
        assert!(
            matches!(ack, Event::MailConsumed { handle: target, through: 1, .. } if target == handle)
        );
        assert_eq!(consumed_incoming, incoming);
    }
}
