//! Regression: `permission.model: allow` must not surface interactive permission
//! prompts for tool actions or external-directory checks (Deny still wins).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_tool::{
    Action, Invocation, InvocationPolicy, Mode, PermissionModel, PermissionPlane, PermissionRules,
    Resource, Rule,
};

#[tokio::test]
async fn allow_model_authorize_then_assert_does_not_ask() {
    let policy = InvocationPolicy::compile(PermissionModel::Allow, Vec::new()).unwrap();
    let (plane, mut rx) = PermissionPlane::new_with_policy(
        PermissionRules::new(vec![
            Rule::new(Action::Read, "*", Mode::Allow),
            Rule::new(Action::Glob, "*", Mode::Allow),
            Rule::new(Action::Grep, "*", Mode::Allow),
        ]),
        policy,
    );

    let authorized = plane
        .authorize(&Invocation::tool("write", Mode::Ask))
        .await
        .expect("allow model authorizes write");
    authorized
        .assert(Action::Edit, Resource::Path("foo.rs".into()))
        .await
        .expect("call grant should cover edit assert");

    let authorized = plane
        .authorize(&Invocation::command("shell", "echo hi"))
        .await
        .expect("allow model authorizes shell");
    authorized
        .assert(Action::Bash, Resource::Command("echo hi".into()))
        .await
        .expect("call grant should cover bash assert");

    let authorized = plane
        .authorize(&Invocation::mcp("mcp__codegraph__explore"))
        .await
        .expect("allow model authorizes mcp");
    authorized
        .assert(
            Action::Mcp,
            Resource::Command("mcp__codegraph__explore".into()),
        )
        .await
        .expect("call grant should cover mcp assert");

    assert!(
        rx.try_recv().is_err(),
        "permission ask channel must stay empty under model=allow"
    );
}

#[tokio::test]
async fn allow_model_does_not_ask_for_external_directory() {
    let policy = InvocationPolicy::compile(PermissionModel::Allow, Vec::new()).unwrap();
    let (plane, mut rx) = PermissionPlane::new_with_policy(PermissionRules::default(), policy);
    let authorized = plane
        .authorize(&Invocation::command("shell", "ls"))
        .await
        .expect("authorize");
    let authorized2 = authorized.clone();
    let task = tokio::spawn(async move {
        authorized2
            .assert(Action::ExternalDirectory, Resource::Path("/tmp/*".into()))
            .await
    });
    let finished = tokio::time::timeout(std::time::Duration::from_millis(200), task)
        .await
        .expect("allow model must not hang on external directory ask");
    finished
        .expect("join")
        .expect("allow model must not prompt for external directory");
    assert!(
        rx.try_recv().is_err(),
        "external directory must not ask under model=allow"
    );
}

#[tokio::test]
async fn allow_model_assert_without_authorize_still_skips_ask() {
    let policy = InvocationPolicy::compile(PermissionModel::Allow, Vec::new()).unwrap();
    let (plane, mut rx) = PermissionPlane::new_with_policy(PermissionRules::default(), policy);
    let plane2 = plane.clone();
    let task = tokio::spawn(async move {
        plane2
            .assert(Action::Edit, Resource::Path("x.rs".into()))
            .await?;
        plane2
            .assert(Action::Bash, Resource::Command("echo".into()))
            .await
    });
    let finished = tokio::time::timeout(std::time::Duration::from_millis(200), task)
        .await
        .expect("allow model must not hang on assert without authorize");
    finished.expect("join").expect("assert ok");
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn allow_model_still_honors_snapshot_deny() {
    let policy = InvocationPolicy::compile(PermissionModel::Allow, Vec::new()).unwrap();
    let (plane, mut rx) = PermissionPlane::new_with_policy(
        PermissionRules::new(vec![Rule::new(Action::Edit, "/etc/*", Mode::Deny)]),
        policy,
    );
    let authorized = plane
        .authorize(&Invocation::tool("write", Mode::Ask))
        .await
        .expect("authorize");
    let err = authorized
        .assert(Action::Edit, Resource::Path("/etc/passwd".into()))
        .await
        .expect_err("snapshot deny must still deny under allow");
    assert!(matches!(err, hya_tool::PermissionError::Denied { .. }));
    assert!(rx.try_recv().is_err(), "deny must not become an ask");
}
