//! Harness-only rejection fields; agent send regressions run through the engine.
#![allow(clippy::unwrap_used)]

use hya_proto::{AgentName, Event, SessionId, SubagentMode};
use hya_store::{SessionStore, StoreError};

#[tokio::test]
async fn harness_rejections_preserve_sender_and_address() {
    let store = SessionStore::connect_memory().await.unwrap();
    let root = SessionId::new();
    let child = SessionId::new();
    store
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: child,
                handle: "transient-1".into(),
                parent: Some("main".into()),
                agent_type: AgentName::new("worker"),
                mode: SubagentMode::Transient,
            },
        )
        .await
        .unwrap();
    let before = store.replay(root).await.unwrap().len();
    for handle in ["main/missing-1", "main/transient-1"] {
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
