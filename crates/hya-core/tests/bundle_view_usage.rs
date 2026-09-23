//! `session.usage` reports fold each session log's projection and merge the
//! spawn tree.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_core::bundle_views::session_usage_report;
use hya_core::{HostSessionReads, StoreSessionReads, UsageScope};
use hya_proto::{AgentName, Event, MemberId, ModelRef, SessionId, TokenUsage, UsagePurpose};
use hya_store::SessionStore;
use serde_json::json;

async fn create(store: &SessionStore, session: SessionId, parent: Option<SessionId>, agent: &str) {
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent,
                agent: AgentName::new(agent),
                model: ModelRef::new("fake/model"),
                workdir: "/tmp".into(),
            },
        )
        .await
        .unwrap();
}

async fn spawned(store: &SessionStore, parent: SessionId, child: SessionId) {
    store
        .append_event(
            parent,
            &Event::MemberSpawned {
                session: parent,
                member: MemberId::new(),
                child: Some(child),
                subagent_type: AgentName::new("explore"),
                description: "look".into(),
                depth: 1,
                directive: String::new(),
                tool_call: None,
            },
        )
        .await
        .unwrap();
}

async fn used(store: &SessionStore, session: SessionId, model: &str, tokens: TokenUsage) {
    store
        .append_event(
            session,
            &Event::UsageRecorded {
                session,
                message: None,
                step: None,
                model: ModelRef::new(model),
                purpose: UsagePurpose::Turn,
                tokens,
            },
        )
        .await
        .unwrap();
}

fn tokens(input: u64, output: u64, reasoning: u64, unknown: bool) -> TokenUsage {
    TokenUsage {
        input,
        output,
        reasoning,
        reasoning_unknown: unknown,
        ..TokenUsage::default()
    }
}

#[tokio::test]
async fn tree_scope_folds_every_descendant_and_merges_the_total() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, child, grandchild) = (SessionId::new(), SessionId::new(), SessionId::new());
    create(&store, root, None, "build").await;
    create(&store, child, Some(root), "explore").await;
    create(&store, grandchild, Some(child), "explore").await;
    spawned(&store, root, child).await;
    spawned(&store, child, grandchild).await;
    used(&store, root, "fake/model", tokens(10, 5, 2, false)).await;
    used(&store, child, "fake/model", tokens(7, 3, 0, true)).await;
    used(&store, grandchild, "other/model", tokens(1, 1, 1, false)).await;

    let report = session_usage_report(&store, root, UsageScope::Tree)
        .await
        .unwrap();
    assert_eq!(report.root, root);
    let rows = report
        .sessions
        .iter()
        .map(|row| row.session)
        .collect::<Vec<_>>();
    assert_eq!(rows, [root, child, grandchild], "breadth-first, root first");
    assert_eq!(report.sessions[1].parent, Some(root));

    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["scope"], "tree");
    let fake = &json["total"]["by_model"]["fake/model"];
    assert_eq!(fake["input"], 17);
    assert_eq!(fake["output"], 8);
    assert_eq!(fake["rounds"], 2);
    assert_eq!(
        fake["split"],
        json!({"thinking": 2, "visible": 3, "unknown": 3}),
        "known and unknown thinking splits survive the merge"
    );
    assert_eq!(json["total"]["by_model"]["other/model"]["rounds"], 1);
    assert_eq!(json["total"]["total"]["output"], 9);
    assert_eq!(json["total"]["by_purpose"]["turn"]["rounds"], 3);
    assert_eq!(json["sessions"][0]["agent"], "build");
    assert_eq!(json["sessions"][0]["usage"]["total"]["input"], 10);
    assert!(json.get("truncated").is_none());

    let single = StoreSessionReads::new(store.clone())
        .session_usage(child, UsageScope::Session)
        .await
        .unwrap();
    assert_eq!(single.sessions.len(), 1);
    assert_eq!(single.total.total.totals.input, 7);

    let from_leaf = session_usage_report(&store, grandchild, UsageScope::Root)
        .await
        .unwrap();
    assert_eq!(from_leaf.root, root);
    assert_eq!(from_leaf.session, grandchild);
    assert_eq!(from_leaf.sessions.len(), 3);

    let subtree = session_usage_report(&store, child, UsageScope::Tree)
        .await
        .unwrap();
    assert_eq!(subtree.sessions.len(), 2, "tree is rooted at the session");

    assert!(
        session_usage_report(&store, SessionId::new(), UsageScope::Tree)
            .await
            .is_err(),
        "an unknown session is an error, not an empty report"
    );
}
