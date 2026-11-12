//! Transactional mail authorization regressions.
#![allow(clippy::unwrap_used)]
use hya_proto::{ChannelKind, Event, MailKind, SessionId};
use hya_store::SessionStore;

#[tokio::test]
async fn non_member_cannot_send_to_dm() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    store
        .append_event(
            root,
            &Event::ChannelCreated {
                session: root,
                channel: "DM-test1234".into(),
                kind: ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), "main/worker".into()],
            },
        )
        .await
        .unwrap();
    let result = store
        .append_channel_mail(
            root,
            "main/outsider".into(),
            "DM-test1234".into(),
            MailKind::Message,
            "intrusion".into(),
            None,
            None,
        )
        .await;
    assert!(
        result.is_err(),
        "non-member mail must be rejected: {result:?}"
    );
}

#[tokio::test]
async fn dm_member_can_send() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    store
        .append_event(
            root,
            &Event::ChannelCreated {
                session: root,
                channel: "DM-test1234".into(),
                kind: ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), "main/worker".into()],
            },
        )
        .await
        .unwrap();
    let (_, recipients) = store
        .append_channel_mail(
            root,
            "main".into(),
            "DM-test1234".into(),
            MailKind::Message,
            "hello".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(recipients, 2);
    let projection = store.read_projection(root).await.unwrap();
    assert_eq!(projection.team.inboxes["main/worker"][0].body, "hello");
}

#[tokio::test]
async fn missing_channel_is_rejected() {
    let store = SessionStore::connect_memory().await.unwrap();
    let result = store
        .append_channel_mail(
            SessionId::new(),
            "main".into(),
            "DM-missing1".into(),
            MailKind::Message,
            "hello".into(),
            None,
            None,
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn dm_policy_denies_direction() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    store
        .append_event(
            root,
            &Event::ChannelCreated {
                session: root,
                channel: "DM-test1234".into(),
                kind: ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), "main/worker".into()],
            },
        )
        .await
        .unwrap();
    let policy = hya_proto::mail::ChannelPolicySnapshot {
        dm_parent: 0,
        dm_child: 1,
        ..Default::default()
    };
    let result = store
        .append_channel_mail(
            root,
            "main".into(),
            "DM-test1234".into(),
            MailKind::Message,
            "hello".into(),
            None,
            Some(policy),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn group_member_cannot_announce_as_the_leader() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    store
        .append_event(
            root,
            &Event::ChannelCreated {
                session: root,
                channel: "announce-test1234".into(),
                kind: ChannelKind::Group,
                unit: Some("main".into()),
                members: vec!["main".into(), "main/worker".into()],
            },
        )
        .await
        .unwrap();
    let result = store
        .append_channel_mail(
            root,
            "main/worker".into(),
            "announce-test1234".into(),
            MailKind::Message,
            "hello".into(),
            None,
            None,
        )
        .await;
    assert!(
        matches!(result, Err(hya_store::StoreError::MailAuthorizationRejected { sender, channel, .. }) if sender == "main/worker" && channel == "announce-test1234")
    );
}
