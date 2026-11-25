//! Permission plane, `allow` model, semantic identity, `ExternalDirectory`
//! scoping, and multi-root Project boundary enforcement (ADR-0026).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::SessionId;
use hya_tool::{
    Action, AskRequest, Decision, GrantScope, Invocation, InvocationPolicy, InvocationRule, Mode,
    PermissionError, PermissionInterceptor, PermissionModel, PermissionPlane, PermissionRules,
    PermissionTarget, RememberScope, Resource, Rule, ToolError,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::common::{TempDirGuard, allow, ctx_with, deny, tempdir};

const DIR_A: &str = "/outside/a/*";
const DIR_B: &str = "/outside/b/*";
const FILE_TOOLS: [&str; 7] = ["read", "write", "edit", "glob", "find", "grep", "ls"];

fn path(pattern: &str) -> Resource {
    Resource::Path(pattern.to_string())
}

fn project(id: &str) -> GrantScope {
    GrantScope::Project(id.to_string())
}

async fn assert_external(
    plane: &PermissionPlane,
    rx: &mut UnboundedReceiver<AskRequest>,
    pattern: &str,
    reply: Decision,
) -> Option<RememberScope> {
    let mut task = {
        let plane = plane.clone();
        let resource = path(pattern);
        tokio::spawn(async move { plane.assert(Action::ExternalDirectory, resource).await })
    };
    tokio::select! {
        res = &mut task => {
            res.expect("join").expect("allowed without ask");
            None
        }
        req = rx.recv() => {
            let req = req.expect("ask channel open");
            assert_eq!(req.action, Action::ExternalDirectory);
            let remember = req.remember.clone();
            req.reply.send(reply).expect("send reply");
            let _ = task.await.expect("join");
            Some(remember)
        }
    }
}

#[tokio::test]
async fn rules_last_match_child_deny_and_ask_lifecycle() {
    let rules = PermissionRules::new(vec![
        allow(Action::Read, "/**"),
        deny(Action::Read, "/etc/*"),
    ]);
    assert_eq!(
        rules.evaluate(Action::Read, &path("/etc/passwd")),
        Mode::Deny
    );
    assert_eq!(rules.evaluate(Action::Read, &path("/tmp/a")), Mode::Allow);
    assert_eq!(
        PermissionRules::default().evaluate(Action::Bash, &Resource::Command("ls".into())),
        Mode::Ask
    );

    let child = PermissionRules::new(vec![allow(Action::Read, "/**")])
        .derive_child(vec![deny(Action::Read, "/etc/*")]);
    assert_eq!(
        child.evaluate(Action::Read, &path("/etc/shadow")),
        Mode::Deny
    );
    assert_eq!(child.evaluate(Action::Read, &path("/home/x")), Mode::Allow);

    let (deny_plane, _rx) =
        PermissionPlane::new(PermissionRules::new(vec![deny(Action::Bash, "*")]));
    assert!(
        deny_plane
            .assert(Action::Bash, Resource::Command("rm -rf /".into()))
            .await
            .is_err()
    );

    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    for (decision, expect_ok) in [
        (Some(Decision::AllowOnce), true),
        (Some(Decision::Reject { feedback: None }), false),
        (None, false),
    ] {
        let p = plane.clone();
        let h =
            tokio::spawn(
                async move { p.assert(Action::Bash, Resource::Command("ls".into())).await },
            );
        let req = rx.recv().await.unwrap();
        let is_unavailable = decision.is_none();
        match decision {
            Some(d) => req.reply.send(d).unwrap(),
            None => drop(req),
        }
        let outcome = h.await.unwrap();
        if expect_ok {
            assert!(outcome.is_ok());
        } else if is_unavailable {
            assert!(matches!(outcome, Err(PermissionError::Unavailable)));
        } else {
            assert!(outcome.is_err());
        }
    }
}

#[tokio::test]
async fn allow_model_skips_prompts_and_honors_snapshot_deny() {
    let policy = InvocationPolicy::compile(PermissionModel::Allow, Vec::new()).unwrap();
    let (plane, mut rx) = PermissionPlane::new_with_policy(
        PermissionRules::new(vec![
            allow(Action::Read, "*"),
            allow(Action::Glob, "*"),
            allow(Action::Grep, "*"),
            deny(Action::Edit, "/etc/*"),
        ]),
        policy,
    );

    let auth_write = plane
        .authorize(&Invocation::tool("write", Mode::Ask))
        .await
        .unwrap();
    auth_write
        .assert(Action::Edit, path("foo.rs"))
        .await
        .unwrap();
    let denied = auth_write
        .assert(Action::Edit, path("/etc/passwd"))
        .await
        .unwrap_err();
    assert!(matches!(denied, PermissionError::Denied { .. }));

    let auth_bash = plane
        .authorize(&Invocation::command("bash", "echo hi"))
        .await
        .unwrap();
    auth_bash
        .assert(Action::Bash, Resource::Command("echo hi".into()))
        .await
        .unwrap();
    auth_bash
        .assert(Action::ExternalDirectory, path("/tmp/*"))
        .await
        .unwrap();

    let auth_mcp = plane
        .authorize(&Invocation::mcp("mcp__codegraph__explore"))
        .await
        .unwrap();
    auth_mcp
        .assert(
            Action::Mcp,
            Resource::Command("mcp__codegraph__explore".into()),
        )
        .await
        .unwrap();

    // Direct assert without prior authorize also skips ask under model=allow.
    plane.assert(Action::Edit, path("x.rs")).await.unwrap();
    plane
        .assert(Action::Bash, Resource::Command("echo".into()))
        .await
        .unwrap();
    assert!(rx.try_recv().is_err(), "model=allow must never prompt");
}

struct StubInterceptor(Option<[u8; 32]>);

#[async_trait]
impl PermissionInterceptor for StubInterceptor {
    async fn intercept(&self, _: Option<SessionId>, _: Action, _: &Resource) -> Option<Decision> {
        None
    }

    fn semantic_identity_v1(&self) -> Option<[u8; 32]> {
        self.0
    }
}

#[test]
fn permission_semantic_identity_tracks_policy_and_interceptors() {
    let build = |model, read_pat, selector| {
        let rules = PermissionRules::new(vec![
            allow(Action::Read, read_pat),
            Rule::new(Action::Bash, "git status", Mode::Ask),
        ]);
        let policy = InvocationPolicy::compile(
            model,
            vec![
                InvocationRule::new(PermissionTarget::Tool, selector, Mode::Ask),
                InvocationRule::new(PermissionTarget::Command, "^git status$", Mode::Deny),
            ],
        )
        .unwrap();
        PermissionPlane::new_with_policy(rules, policy).0
    };

    let base = build(PermissionModel::Default, "/workspace/**", "^write$");
    let id = base.semantic_identity_v1().expect("representable");
    assert_ne!(id, [0; 32]);
    assert_eq!(
        build(PermissionModel::Default, "/workspace/**", "^write$").semantic_identity_v1(),
        Some(id)
    );
    assert_ne!(
        build(PermissionModel::Default, "/other/**", "^write$").semantic_identity_v1(),
        Some(id)
    );
    assert_ne!(
        build(PermissionModel::Allow, "/workspace/**", "^write$").semantic_identity_v1(),
        Some(id)
    );
    assert_ne!(
        build(PermissionModel::Default, "/workspace/**", "^edit$").semantic_identity_v1(),
        Some(id)
    );

    assert_eq!(
        base.clone()
            .with_interceptor(Arc::new(StubInterceptor(None)))
            .semantic_identity_v1(),
        None
    );
    let id1 = base
        .clone()
        .with_interceptor(Arc::new(StubInterceptor(Some([1; 32]))))
        .semantic_identity_v1()
        .expect("identified");
    let id1_same = base
        .clone()
        .with_interceptor(Arc::new(StubInterceptor(Some([1; 32]))))
        .semantic_identity_v1();
    let id2 = base
        .with_interceptor(Arc::new(StubInterceptor(Some([2; 32]))))
        .semantic_identity_v1();
    assert_eq!(Some(id1), id1_same);
    assert_ne!(Some(id1), id2);
}

struct RecordingApprover {
    seen: Mutex<Vec<(Action, Resource)>>,
    decision: Decision,
}

#[async_trait]
impl PermissionInterceptor for RecordingApprover {
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
async fn external_directory_project_session_and_unscoped_remember_scoping() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let a = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    let a_sibling = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    let b = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_b"));

    // AllowAlways remembers exact directory scoped to project A.
    let remember = assert_external(&a, &mut rx, DIR_A, Decision::AllowAlways).await;
    assert_eq!(
        remember,
        Some(RememberScope::Scoped {
            pattern: DIR_A.to_string(),
            scope: Some(project("prj_a")),
        })
    );
    assert_eq!(
        assert_external(&a, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );
    assert_eq!(
        assert_external(&a_sibling, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );
    assert!(
        assert_external(&a, &mut rx, "/outside/a/nested/*", Decision::AllowOnce)
            .await
            .is_some()
    );
    assert!(
        assert_external(&a, &mut rx, DIR_B, Decision::AllowOnce)
            .await
            .is_some()
    );
    assert!(
        assert_external(&b, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some()
    );
    assert!(
        assert_external(&plane, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some()
    );

    // Session without project remembers for that session only.
    let temp_id = SessionId::new();
    let temp = plane.for_session(temp_id);
    assert_eq!(temp.grant_scope(), Some(GrantScope::Session(temp_id)));
    assert_eq!(
        assert_external(&temp, &mut rx, DIR_A, Decision::AllowAlways).await,
        Some(RememberScope::Scoped {
            pattern: DIR_A.to_string(),
            scope: Some(GrantScope::Session(temp_id)),
        })
    );
    assert_eq!(
        assert_external(&temp, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );
    let other_temp = plane.for_session(SessionId::new());
    assert!(
        assert_external(&other_temp, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some()
    );

    // Unscoped plane remembers plane-wide for that exact directory only.
    let (unscoped, mut urx) = PermissionPlane::new(PermissionRules::default());
    assert_eq!(
        assert_external(&unscoped, &mut urx, DIR_A, Decision::AllowAlways).await,
        Some(RememberScope::Scoped {
            pattern: DIR_A.to_string(),
            scope: None,
        })
    );
    assert_eq!(
        assert_external(&unscoped, &mut urx, DIR_A, Decision::AllowOnce).await,
        None
    );
    assert!(
        assert_external(
            &unscoped,
            &mut urx,
            "/outside/a/nested/*",
            Decision::AllowOnce
        )
        .await
        .is_some()
    );

    // Other actions keep action-wide LegacyAction remember scope.
    let task = {
        let a = a.clone();
        tokio::spawn(async move {
            a.assert(Action::WebFetch, Resource::Url("https://x".into()))
                .await
        })
    };
    let req = rx.recv().await.unwrap();
    assert_eq!(req.remember, RememberScope::LegacyAction);
    req.reply.send(Decision::AllowOnce).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn external_directory_saved_rules_yolo_call_grant_and_approver() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let a = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    let b = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_b"));

    // Global saved `*` applies to every project until revoked.
    plane.grant_saved(Action::ExternalDirectory, "*").await;
    assert_eq!(
        assert_external(&a, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );
    assert_eq!(
        assert_external(&b, &mut rx, DIR_B, Decision::AllowOnce).await,
        None
    );
    plane.revoke_saved(Action::ExternalDirectory, "*").await;
    assert!(
        assert_external(&a, &mut rx, DIR_A, Decision::AllowOnce)
            .await
            .is_some()
    );

    // Scoped saved rules restore and revoke per project.
    plane
        .grant_scoped(project("prj_a"), Action::ExternalDirectory, DIR_A)
        .await;
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
            .is_some()
    );

    // Yolo (Danger) bypasses ExternalDirectory.
    let yolo = plane
        .with_invocation_model(PermissionModel::Danger)
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    assert_eq!(
        assert_external(&yolo, &mut rx, DIR_A, Decision::AllowOnce).await,
        None
    );

    // A call grant for `read` does not satisfy ExternalDirectory.
    let policy = InvocationPolicy::compile(PermissionModel::Default, Vec::new()).unwrap();
    let (p2, mut rx2) = PermissionPlane::new_with_policy(PermissionRules::default(), policy);
    let auth = p2
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"))
        .authorize(&Invocation::tool("read", Mode::Allow))
        .await
        .unwrap();
    assert!(
        assert_external(&auth, &mut rx2, DIR_A, Decision::AllowOnce)
            .await
            .is_some()
    );

    // Approver interceptor's AllowAlways stays scoped to project A.
    let (p3, mut rx3) = PermissionPlane::new(PermissionRules::default());
    let approver = Arc::new(RecordingApprover {
        seen: Mutex::default(),
        decision: Decision::AllowAlways,
    });
    let a3 = p3
        .clone()
        .append_interceptor(approver.clone())
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    a3.assert(Action::ExternalDirectory, path(DIR_A))
        .await
        .unwrap();
    assert_eq!(
        approver.seen.lock().unwrap().as_slice(),
        &[(Action::ExternalDirectory, path(DIR_A))]
    );
    assert!(rx3.try_recv().is_err());
    let b3 = p3
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_b"));
    assert!(
        assert_external(&b3, &mut rx3, DIR_A, Decision::AllowOnce)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn external_directory_exact_matching_literal_star_legacy_and_configured_globs() {
    let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
    let session = plane
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));

    // Home directory grant covers only that directory, never subdirectories or parents.
    assert!(
        assert_external(&session, &mut rx, "/home/u/*", Decision::AllowAlways)
            .await
            .is_some()
    );
    assert_eq!(
        assert_external(&session, &mut rx, "/home/u/*", Decision::AllowOnce).await,
        None
    );
    for deeper in ["/home/u/.ssh/*", "/home/u/.config/gh/*", "/home/*", "/*"] {
        assert!(
            assert_external(&session, &mut rx, deeper, Decision::AllowOnce)
                .await
                .is_some(),
            "{deeper}"
        );
    }

    // Literal `*` in a directory name does not widen the grant.
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
            "{other}"
        );
    }

    // Legacy `<dir>/*`, bare `<dir>`, and global non-`*` rows grant only that directory.
    plane
        .grant_scoped(project("prj_a"), Action::ExternalDirectory, "/outside/bare")
        .await;
    plane
        .grant_saved(Action::ExternalDirectory, "/outside/legacy/*")
        .await;
    assert_eq!(
        assert_external(&session, &mut rx, "/outside/bare/*", Decision::AllowOnce).await,
        None
    );
    assert_eq!(
        assert_external(&session, &mut rx, "/outside/legacy/*", Decision::AllowOnce).await,
        None
    );
    assert!(
        assert_external(
            &session,
            &mut rx,
            "/outside/bare/sub/*",
            Decision::AllowOnce
        )
        .await
        .is_some()
    );
    assert!(
        assert_external(
            &session,
            &mut rx,
            "/outside/legacy/sub/*",
            Decision::AllowOnce
        )
        .await
        .is_some()
    );

    // Scoped `*` allows every directory.
    let (p_star, mut rx_star) = PermissionPlane::new(PermissionRules::default());
    p_star
        .grant_scoped(project("prj_a"), Action::ExternalDirectory, "*")
        .await;
    let a_star = p_star
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    for any in [DIR_A, "/outside/a/nested/*", "/*"] {
        assert_eq!(
            assert_external(&a_star, &mut rx_star, any, Decision::AllowOnce).await,
            None
        );
    }

    // Configured snapshot rules retain glob semantics.
    let (p_cfg, mut rx_cfg) = PermissionPlane::new(PermissionRules::new(vec![allow(
        Action::ExternalDirectory,
        "/outside/a/*",
    )]));
    let s_cfg = p_cfg
        .for_session(SessionId::new())
        .with_grant_scope(project("prj_a"));
    assert_eq!(
        assert_external(
            &s_cfg,
            &mut rx_cfg,
            "/outside/a/nested/*",
            Decision::AllowOnce
        )
        .await,
        None
    );
    assert!(
        assert_external(&s_cfg, &mut rx_cfg, DIR_B, Decision::AllowOnce)
            .await
            .is_some()
    );
}

struct RootsLayout {
    one: PathBuf,
    two: PathBuf,
    outside: PathBuf,
    _guards: [TempDirGuard; 3],
}

fn roots_layout() -> RootsLayout {
    let (one, two, outside) = (tempdir("root1"), tempdir("root2"), tempdir("outside"));
    for dir in [&one, &two, &outside] {
        std::fs::write(dir.join("file.txt"), "alpha\nneedle\n").unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, one.join("escape")).unwrap();
    RootsLayout {
        _guards: [
            TempDirGuard::new(&one),
            TempDirGuard::new(&two),
            TempDirGuard::new(&outside),
        ],
        one,
        two,
        outside,
    }
}

async fn run_in_roots(
    layout: &RootsLayout,
    tool: &str,
    input: Value,
    model: Option<PermissionModel>,
) -> (Result<Value, ToolError>, Vec<(Action, String)>) {
    let rules = PermissionRules::new(
        [Action::Read, Action::Edit, Action::Glob, Action::Grep]
            .into_iter()
            .map(|a| allow(a, "*"))
            .collect(),
    );
    let (permission, mut rx) = PermissionPlane::new(rules);
    let permission = match model {
        Some(m) => permission.with_invocation_model(m),
        None => permission,
    };
    let asks: Arc<Mutex<Vec<(Action, String)>>> = Arc::default();
    let recorder = Arc::clone(&asks);
    let drain = tokio::spawn(async move {
        while let Some(ask) = rx.recv().await {
            let resource = match &ask.resource {
                Resource::Path(p) => p.clone(),
                other => format!("{other:?}"),
            };
            recorder.lock().unwrap().push((ask.action, resource));
            let _ = ask.reply.send(Decision::Reject { feedback: None });
        }
    });
    let mut ctx = ctx_with(vec![], layout.one.clone());
    ctx.permission = permission;
    ctx.roots = vec![layout.one.clone(), layout.two.clone()];
    let tool = hya_tool_loader::builtins().get(tool).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), tool.execute(&ctx, input))
        .await
        .expect("tool call timed out");
    drop(ctx);
    drain.abort();
    let recorded = asks.lock().unwrap().clone();
    (result, recorded)
}

fn input_for_tool(tool: &str, target: &Path) -> Value {
    let file = target.join("file.txt");
    let s = |p: &Path| p.to_string_lossy().into_owned();
    match tool {
        "read" => json!({ "path": s(&file) }),
        "write" => json!({ "path": s(&target.join("new.txt")), "content": "x\n" }),
        "edit" => json!({ "path": s(&file), "edits": [{ "op": "append", "lines": ["omega"] }] }),
        "glob" | "find" => json!({ "pattern": "*.txt", "path": s(target) }),
        "grep" => json!({ "pattern": "needle", "path": s(target) }),
        "ls" => json!({ "path": s(target) }),
        other => panic!("unexpected {other}"),
    }
}

#[tokio::test]
async fn project_roots_enforce_second_root_outside_symlink_and_yolo() {
    let ext = |asks: &[(Action, String)]| -> Vec<String> {
        asks.iter()
            .filter(|(a, _)| *a == Action::ExternalDirectory)
            .map(|(_, r)| r.clone())
            .collect()
    };

    for tool in FILE_TOOLS {
        let layout = roots_layout();
        // Second root runs without ExternalDirectory ask.
        let (ok, asks) = run_in_roots(&layout, tool, input_for_tool(tool, &layout.two), None).await;
        assert!(ok.is_ok(), "{tool}: {ok:?}");
        assert!(ext(&asks).is_empty(), "{tool}");

        // Outside every root asks for canonical ExternalDirectory.
        let expected_outside = match tool {
            "glob" | "grep" => format!("{}/*", layout.outside.parent().unwrap().to_string_lossy()),
            _ => format!("{}/*", layout.outside.to_string_lossy()),
        };
        let (denied, asks) =
            run_in_roots(&layout, tool, input_for_tool(tool, &layout.outside), None).await;
        assert_eq!(ext(&asks), vec![expected_outside.clone()], "{tool}");
        assert!(matches!(denied, Err(ToolError::Permission(_))), "{tool}");

        // Symlink escape asks for canonical outside directory without touching it.
        #[cfg(unix)]
        {
            let through_link = layout.one.join("escape");
            let (escaped, asks) =
                run_in_roots(&layout, tool, input_for_tool(tool, &through_link), None).await;
            assert_eq!(ext(&asks), vec![expected_outside], "{tool}");
            assert!(matches!(escaped, Err(ToolError::Permission(_))), "{tool}");
            assert_eq!(
                std::fs::read_to_string(layout.outside.join("file.txt")).unwrap(),
                "alpha\nneedle\n"
            );
            assert!(!layout.outside.join("new.txt").exists());
        }

        // Yolo mode runs outside without asking.
        let (yolo_ok, asks) = run_in_roots(
            &layout,
            tool,
            input_for_tool(tool, &layout.outside),
            Some(PermissionModel::Danger),
        )
        .await;
        assert!(yolo_ok.is_ok(), "{tool} yolo: {yolo_ok:?}");
        assert!(asks.is_empty(), "{tool} yolo");
    }

    // apply_patch accepts second root and rejects outside, traversal, and symlink escape.
    let layout = roots_layout();
    let patch_for = |p: &str| json!({ "patchText": format!("*** Begin Patch\n*** Add File: {p}\n+added\n*** End Patch") });
    let in_two = layout.two.join("added.txt");
    let (ok, asks) = run_in_roots(
        &layout,
        "apply_patch",
        patch_for(&in_two.to_string_lossy()),
        None,
    )
    .await;
    assert!(ok.is_ok() && ext(&asks).is_empty());
    assert_eq!(std::fs::read_to_string(&in_two).unwrap(), "added\n");

    for bad in [
        layout
            .outside
            .join("added.txt")
            .to_string_lossy()
            .into_owned(),
        layout
            .one
            .join("../escaped.txt")
            .to_string_lossy()
            .into_owned(),
        "escape/added.txt".to_string(),
    ] {
        let (err, _) = run_in_roots(&layout, "apply_patch", patch_for(&bad), None).await;
        assert!(matches!(err, Err(ToolError::Input(_))), "{bad}: {err:?}");
    }
    assert!(!layout.outside.join("added.txt").exists());
}
