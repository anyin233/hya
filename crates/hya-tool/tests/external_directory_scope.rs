//! ADR-0026: "allow always" on an `ExternalDirectory` ask remembers the
//! concrete `<dir>/*` pattern, scoped to the session's Project (or to the
//! session itself when it has no Project), never the action-wide `*`. A
//! remembered grant covers exactly that one directory: it is compared by
//! equality, never as a glob, so it neither reaches subdirectories nor widens
//! through a literal `*` in a path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use hya_proto::SessionId;
use hya_tool::{
    Action, AskRequest, Decision, GrantScope, Invocation, InvocationPolicy, Mode,
    PermissionInterceptor, PermissionModel, PermissionPlane, PermissionRules, RememberScope,
    Resource,
};
use tokio::sync::mpsc::UnboundedReceiver;

const DIR_A: &str = "/outside/a/*";
const DIR_B: &str = "/outside/b/*";

fn path(pattern: &str) -> Resource {
    Resource::Path(pattern.to_string())
}

fn project(id: &str) -> GrantScope {
    GrantScope::Project(id.to_string())
}

/// Assert `ExternalDirectory` for `pattern` on `plane`; when it asks, answer
/// with `reply` and return the ask's remember scope. `None` = no ask.
async fn assert_external(
    plane: &PermissionPlane,
    rx: &mut UnboundedReceiver<AskRequest>,
    pattern: &str,
    reply: Decision,
) -> Option<RememberScope> {
    let task = {
        let plane = plane.clone();
        let resource = path(pattern);
        tokio::spawn(async move { plane.assert(Action::ExternalDirectory, resource).await })
    };
    match tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
        Ok(Some(req)) => {
            assert_eq!(req.action, Action::ExternalDirectory);
            let remember = req.remember.clone();
            req.reply.send(reply).expect("send reply");
            let _result = task.await.expect("join");
            Some(remember)
        }
        _ => {
            task.await.expect("join").expect("allowed without an ask");
            None
        }
    }
}

#[tokio::test]
async fn allow_always_remembers_the_concrete_directory_for_the_project() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let session = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));

    let remember = assert_external(&session, &mut rx, DIR_A, Decision::AllowAlways).await;
    assert_eq!(
        remember,
        Some(RememberScope::Scoped {
            pattern: DIR_A.to_string(),
            scope: Some(project("prj_a")),
        })
    );
    assert_eq!(remember.unwrap().pattern(), DIR_A, "never the `*` pattern");

    assert_eq!(
        assert_external(&session, &mut rx, DIR_A, Decision::AllowOnce).await,
        None,
        "the same directory is remembered"
    );
    assert!(
        assert_external(
            &session,
            &mut rx,
            "/outside/a/nested/*",
            Decision::AllowOnce
        )
        .await
        .is_some(),
        "a subdirectory of the remembered directory asks again"
    );
    assert!(
        assert_external(&session, &mut rx, DIR_B, Decision::AllowOnce)
            .await
            .is_some(),
        "another directory still asks"
    );

    let sibling = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    assert_eq!(
        assert_external(&sibling, &mut rx, DIR_A, Decision::AllowOnce).await,
        None,
        "another session of the same Project shares the grant"
    );
}

#[tokio::test]
async fn a_projects_allow_always_does_not_apply_to_another_project() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let a = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    let b = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_b"));

    assert!(
        assert_external(&a, &mut rx, DIR_A, Decision::AllowAlways)
            .await
            .is_some()
    );
    assert!(
        assert_external(&b, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some(),
        "project B must ask again"
    );
    assert!(
        assert_external(&plane, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some(),
        "a plane without a scope must ask again"
    );
}

#[tokio::test]
async fn global_rules_apply_to_every_project() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    plane.grant_saved(Action::ExternalDirectory, "*").await;
    let a = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    let temporary = plane.for_session(SessionId::new());

    assert_eq!(
        assert_external(&a, &mut rx, DIR_A, Decision::AllowOnce).await,
        None,
        "a legacy global `*` rule still applies"
    );
    assert_eq!(
        assert_external(&temporary, &mut rx, DIR_B, Decision::AllowOnce).await,
        None
    );

    plane.revoke_saved(Action::ExternalDirectory, "*").await;
    assert!(
        assert_external(&a, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn saved_project_rules_restore_and_revoke_per_project() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    plane
        .grant_scoped(project("prj_a"), Action::ExternalDirectory, DIR_A)
        .await;
    let a = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    let b = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_b"));

    assert_eq!(
        assert_external(&a, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );
    assert!(
        assert_external(&b, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some()
    );

    plane
        .revoke_scoped(&project("prj_a"), Action::ExternalDirectory, DIR_A)
        .await;
    assert!(
        assert_external(&a, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some(),
        "a revoked project rule asks again"
    );
}

#[tokio::test]
async fn a_session_without_a_project_remembers_for_that_session_only() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let id = SessionId::new();
    let temporary = plane.for_session(id);
    assert_eq!(temporary.grant_scope(), Some(GrantScope::Session(id)));

    let remember = assert_external(&temporary, &mut rx, DIR_A, Decision::AllowAlways).await;
    assert_eq!(
        remember,
        Some(RememberScope::Scoped {
            pattern: DIR_A.to_string(),
            scope: Some(GrantScope::Session(id)),
        })
    );
    assert_eq!(
        assert_external(&temporary, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );

    let other = plane.for_session(SessionId::new());
    assert!(
        assert_external(&other, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some(),
        "the grant does not leak to another session"
    );
}

#[tokio::test]
async fn other_actions_keep_the_action_wide_remember_scope() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let session = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    let task = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .assert(Action::WebFetch, Resource::Url("https://x".to_string()))
                .await
        })
    };
    let req = rx.recv().await.expect("webfetch asks");
    assert_eq!(req.remember, RememberScope::LegacyAction);
    req.reply.send(Decision::AllowOnce).expect("send reply");
    task.await.expect("join").expect("allowed");
}

#[tokio::test]
async fn yolo_bypasses_external_directory() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let yolo = plane
        .with_invocation_model(PermissionModel::Danger)
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    assert_eq!(
        assert_external(&yolo, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );
}

#[tokio::test]
async fn a_call_grant_does_not_satisfy_external_directory() {
    let policy = InvocationPolicy::compile(PermissionModel::Default, Vec::new()).unwrap();
    let (plane, mut rx) = PermissionPlane::new_with_policy(PermissionRules::default(), policy);
    let authorized = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"))
        .authorize(&Invocation::tool("read", Mode::Allow))
        .await
        .expect("read is allowed");
    assert!(
        assert_external(&authorized, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some()
    );
}

struct Recording {
    seen: std::sync::Mutex<Vec<(Action, Resource)>>,
    decision: Decision,
}

#[async_trait::async_trait]
impl PermissionInterceptor for Recording {
    async fn intercept(
        &self,
        _session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        self.seen.lock().unwrap().push((action, resource.clone()));
        Some(self.decision.clone())
    }
}

#[tokio::test]
async fn an_approver_receives_external_directory_asks_and_its_always_is_scoped() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let approver = Arc::new(Recording {
        seen: std::sync::Mutex::default(),
        decision: Decision::AllowAlways,
    });
    let a = plane
        .clone()
        .append_interceptor(approver.clone())
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    a.assert(Action::ExternalDirectory, path(DIR_A))
        .await
        .expect("approver allows");
    assert_eq!(
        approver.seen.lock().unwrap().as_slice(),
        &[(Action::ExternalDirectory, path(DIR_A))]
    );
    assert!(rx.try_recv().is_err(), "the approver answered");

    let b = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_b"));
    assert!(
        assert_external(&b, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some(),
        "the approver's allow-always stays in project A"
    );
}

#[tokio::test]
async fn approving_a_home_file_does_not_allow_the_rest_of_home() {
    // Approving `/home/u/notes.txt` asks for (and remembers) `/home/u/*`.
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let session = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    assert!(
        assert_external(&session, &mut rx, "/home/u/*", Decision::AllowAlways)
            .await
            .is_some()
    );
    assert_eq!(
        assert_external(&session, &mut rx, "/home/u/*", Decision::AllowOnce).await,
        None,
        "another file of the approved directory is covered"
    );
    for deeper in ["/home/u/.ssh/*", "/home/u/.config/gh/*", "/home/*", "/*"] {
        assert!(
            assert_external(&session, &mut rx, deeper, Decision::AllowOnce)
                .await
                .is_some(),
            "{deeper} must ask again"
        );
    }
}

#[tokio::test]
async fn a_literal_star_in_a_directory_name_does_not_widen_the_grant() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let session = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    assert!(
        assert_external(&session, &mut rx, "/outside/a*/*", Decision::AllowAlways)
            .await
            .is_some()
    );
    assert_eq!(
        assert_external(&session, &mut rx, "/outside/a*/*", Decision::AllowOnce).await,
        None
    );
    for other in ["/outside/abc/*", "/outside/a/*", "/outside/a*/sub/*"] {
        assert!(
            assert_external(&session, &mut rx, other, Decision::AllowOnce)
                .await
                .is_some(),
            "{other} must ask again"
        );
    }
}

#[tokio::test]
async fn legacy_saved_directory_rows_grant_exactly_that_directory() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    // A Project row saved before this change (`<dir>/*`) and a global
    // non-`*` row both mean one directory, never a subtree.
    plane
        .grant_scoped(project("prj_a"), Action::ExternalDirectory, DIR_A)
        .await;
    plane
        .grant_saved(Action::ExternalDirectory, "/outside/legacy/*")
        .await;
    let a = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));

    for granted in [DIR_A, "/outside/legacy/*"] {
        assert_eq!(
            assert_external(&a, &mut rx, granted, Decision::AllowOnce).await,
            None,
            "{granted} is granted"
        );
    }
    for other in [
        "/outside/a/nested/*",
        "/outside/legacy/deeper/*",
        "/outside/*",
    ] {
        assert!(
            assert_external(&a, &mut rx, other, Decision::AllowOnce)
                .await
                .is_some(),
            "{other} must ask"
        );
    }
}

#[tokio::test]
async fn a_bare_directory_row_grants_that_directory() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    plane
        .grant_scoped(project("prj_a"), Action::ExternalDirectory, "/outside/a")
        .await;
    let a = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    assert_eq!(
        assert_external(&a, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );
    assert!(
        assert_external(&a, &mut rx, "/outside/a/nested/*", Decision::AllowOnce)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn an_unscoped_planes_allow_always_covers_exactly_that_directory() {
    // A plane with neither a Project nor a session remembers plane-wide.
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let remember = assert_external(&plane, &mut rx, DIR_A, Decision::AllowAlways).await;
    assert_eq!(
        remember,
        Some(RememberScope::Scoped {
            pattern: DIR_A.to_string(),
            scope: None,
        })
    );
    assert_eq!(
        assert_external(&plane, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );
    assert!(
        assert_external(&plane, &mut rx, "/outside/a/nested/*", Decision::AllowOnce)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn configured_rules_keep_glob_semantics() {
    // Rules a user writes in configuration (snapshot rules) are globs; only
    // remembered grants are exact.
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::new(vec![hya_tool::Rule::new(
        Action::ExternalDirectory,
        "/outside/a/*",
        Mode::Allow,
    )]));
    let session = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    assert_eq!(
        assert_external(
            &session,
            &mut rx,
            "/outside/a/nested/*",
            Decision::AllowOnce
        )
        .await,
        None
    );
    assert!(
        assert_external(&session, &mut rx, DIR_B, Decision::AllowOnce)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn a_global_star_rule_still_allows_every_directory() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    plane
        .grant_scoped(project("prj_a"), Action::ExternalDirectory, "*")
        .await;
    let a = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    for any in [DIR_A, "/outside/a/nested/*", "/*"] {
        assert_eq!(
            assert_external(&a, &mut rx, any, Decision::AllowOnce).await,
            None,
            "{any}"
        );
    }
}
