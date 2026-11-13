//! Runtime construction through the public application API.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_app::{OfflineNotice, builtin_agent_catalog};
use std::path::PathBuf;

#[test]
fn builtin_agent_catalog_includes_first_party_workflow_bundle() {
    let catalog = builtin_agent_catalog().expect("builtin agent catalog must build");
    assert!(
        catalog.bundles().bundles().len() >= 2,
        "fresh process includes the immutable first-party bundles"
    );
    assert!(
        catalog
            .bundles()
            .resolve_workflow("plan-impl-review")
            .is_some(),
        "first-party Workflow must be cataloged"
    );
    let goal_loop = catalog
        .bundles()
        .bundles()
        .iter()
        .find(|bundle| bundle.identity().id == "hya/goal-loop")
        .expect("goal-loop first-party bundle");
    assert_eq!(goal_loop.kind(), hya_bundle::PreparedBundleKind::Bundle);
    assert_eq!(goal_loop.agents().len(), 2);
    for skill in [
        "goal-contract",
        "guided-goal",
        "evaluator-prompt",
        "loop-verifier-prompt",
        "loop-planner-prompt",
    ] {
        assert!(
            goal_loop
                .skills()
                .iter()
                .any(|resource| resource.local_id == skill)
        );
    }
    for id in [
        "hya-main",
        "hya-plan",
        "hya-scout",
        "hya-task",
        "hya-reviewer",
    ] {
        assert!(
            catalog.resolve(id).is_some(),
            "builtin `{id}` must resolve without any installed bundle"
        );
    }
}

#[test]
fn offline_notice_names_path_offline_mode_and_the_fix() {
    let notice = OfflineNotice {
        config_path: PathBuf::from("/home/u/.config/hya/config.yaml"),
    };
    let text = notice.render();
    // (a) where the missing config is expected,
    assert!(text.contains("/home/u/.config/hya/config.yaml"));
    // (b) that we are in offline/echo mode,
    assert!(text.contains("OFFLINE"));
    assert!(text.contains("echoes"));
    // (c) how to fix it.
    assert!(text.contains("hya login"));
    assert!(text.contains("docs/configuration.md"));
}
