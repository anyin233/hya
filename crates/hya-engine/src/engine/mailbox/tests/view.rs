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
    let eng = Arc::new(engine().await);
    let o = org(&eng).await;
    let caller = o.lead_1.session;
    let owner = hya_proto::OwnerRunId::new();
    eng.store().claim_runtime_owner(owner).unwrap();
    let old = eng
        .store()
        .recover_claim(caller, owner)
        .await
        .unwrap()
        .claim;
    eng.mail_send(
        o.root,
        MailEndpoint::Handle(o.lead_1.path.clone()),
        MailKind::Message,
        "owed to replacement".into(),
    )
    .await
    .unwrap();
    let ch = dm_channel_between(
        &eng.read_projection(o.root).await.unwrap(),
        "main",
        &o.lead_1.path,
    )
    .unwrap();
    eng.store().recover_claim(caller, owner).await.unwrap();
    let before = eng.replay(o.root).await.unwrap();
    let (plane, rx) = hya_tool::MailboxPlane::new();
    let stop = tokio_util::sync::CancellationToken::new();
    let svc = tokio::spawn(crate::run_mailbox_service(eng.clone(), rx, stop.clone()));
    let res = plane
        .for_session_with_actor(caller, Some(old))
        .read_channel(ch, Some(10))
        .await;
    stop.cancel();
    svc.await.unwrap();
    assert!(res.is_err());
    assert_eq!(eng.replay(o.root).await.unwrap(), before);
}

#[tokio::test]
async fn cursor_cleanup_marks_only_incoming_work_for_wait_and_read() {
    for read in [false, true] {
        let eng = Arc::new(engine().await);
        let o = org(&eng).await;
        let sup = ResidentSupervisor::start(eng.clone());
        let obs = Arc::new(MailObserver::default());
        let erased: Arc<dyn crate::engine::TurnBoundaryObserver> = obs.clone();
        eng.set_turn_observer(Arc::downgrade(&erased));
        for (sender, target, incoming) in [
            (o.root, o.lead_1.path.clone(), false),
            (o.lead_1.session, "main".into(), true),
        ] {
            eng.mail_send(
                sender,
                MailEndpoint::Handle(target),
                MailKind::Message,
                "work".into(),
            )
            .await
            .unwrap();
            if read {
                let ch = dm_channel_between(
                    &eng.read_projection(o.root).await.unwrap(),
                    "main",
                    &o.lead_1.path,
                )
                .unwrap();
                eng.read_channel_history(None, o.root, &ch, Some(10))
                    .await
                    .unwrap();
            } else {
                crate::member_wait::wait_for_members(
                    None,
                    &eng,
                    &sup,
                    o.root,
                    hya_tool::WaitSpec::parse(&serde_json::json!({"timeout_secs": 0}), true)
                        .unwrap(),
                    None,
                )
                .await
                .unwrap();
            }
            let p = eng.read_projection(o.root).await.unwrap();
            assert_eq!(
                p.team.roster["main"].resident_cursor,
                p.team.inboxes["main"].len() as u64
            );
            assert_eq!(
                *obs.0.lock().unwrap(),
                if incoming { vec![(o.root, 7)] } else { vec![] }
            );
        }
        sup.drain(
            hya_proto::FinishCause::Shutdown,
            std::time::Duration::from_secs(2),
        )
        .await;
    }
}

#[tokio::test]
async fn steer_denies_archived_group_mail_and_acks_incoming_only() {
    let eng = engine().await;
    let o = org(&eng).await;
    eng.mail_announce(o.lead_1.session, "group mail before archive".into())
        .await
        .unwrap();
    let mut steer = eng
        .steer_mailbox_snapshot_with_policy(
            o.worker_1.session,
            eng.session_channel_policy(o.worker_1.session),
        )
        .await;
    eng.store()
        .append_event(
            o.root,
            &Event::AgentArchived {
                session: o.root,
                handle: o.worker_1.path,
                child: o.worker_1.session,
                reason: hya_proto::ArchiveReason::ArchivedByParent,
            },
        )
        .await
        .unwrap();
    assert!(steer.drain(&eng).await.unwrap().is_none() && steer.take_acknowledgement().unwrap().1);

    let eng2 = engine().await;
    let o2 = org(&eng2).await;
    eng2.mail_send(
        o2.root,
        MailEndpoint::Handle(o2.lead_1.path.clone()),
        MailKind::Message,
        "outgoing directive".into(),
    )
    .await
    .unwrap();
    for (session, handle, incoming) in [
        (o2.root, "main", false),
        (o2.lead_1.session, o2.lead_1.path.as_str(), true),
    ] {
        let mut s = eng2
            .steer_mailbox_snapshot_with_policy(
                session,
                Some(hya_tool::ChannelPolicySnapshot::default()),
            )
            .await;
        assert!(s.drain(&eng2).await.unwrap().is_none());
        let (ack, consumed_incoming) = s.take_acknowledgement().unwrap();
        assert!(
            matches!(ack, Event::MailConsumed { handle: target, through: 1, .. } if target == handle)
        );
        assert_eq!(consumed_incoming, incoming);
    }
}
