//! Harness-only rejection fields; agent send regressions run through the engine.
#![allow(clippy::unwrap_used)]

use hya_proto::{Event, SessionId};
use hya_store::{SessionStore, StoreError};

#[tokio::test]
async fn harness_rejections_preserve_sender_and_address() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    let before = store.replay(root).await.unwrap().len();
    for handle in ["main/missing-1", "main"] {
        let result = store
            .append_harness_mail(root, handle, "notice".into())
            .await;
        assert!(
            matches!(result,
            Err(StoreError::MailAuthorizationRejected { ref sender, ref channel, .. })
            if sender == "harness" && channel == &format!("direct:{handle}")),
            "{result:?}"
        );
    }
    assert_eq!(store.replay(root).await.unwrap().len(), before);
}

#[tokio::test]
async fn i21_store_rejects_agent_handle_persistence() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    let result = store
        .append_event(
            root,
            &Event::MailSent {
                session: root,
                from: "main".into(),
                to: hya_proto::MailEndpoint::Handle("main/worker".into()),
                kind: hya_proto::MailKind::Message,
                body: "must resolve before append".into(),
            },
        )
        .await;
    assert!(
        matches!(result, Err(StoreError::MailAuthorizationRejected { .. })),
        "{result:?}"
    );
    assert!(store.replay(root).await.unwrap().is_empty());
}
