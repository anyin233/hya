//! T2.32–T2.34 — ADR-0026 multi-root Project permission scope, end to end
//! against a real backend + FakeLlm:
//!
//! - a session may touch every root of its Project without an ask;
//! - a path outside every root asks `ExternalDirectory` for the concrete
//!   `<dir>/*` pattern; "allow always" remembers it for the session's
//!   Project only, so a sibling session of the same Project is covered but a
//!   session of a different Project asks again;
//! - `yolo` mode auto-approves the same outside read without ever asking;
//! - `bash`'s `cwd` never raises `ExternalDirectory`, in or out of yolo.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hya_api::v1 as pb;
use hya_e2e::{E2eEnv, E2eEnvBuilder, E2eError, fake_requests_from, text_step, tool_step};
use hya_proto::SessionId;
use serde_json::{Value, json};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh, empty absolute directory, independent of the backend's own
/// isolation root, so it can stand in for a Project root or an outside
/// directory that no Project claims.
fn fixture_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("hya-scope-e2e-{}-{label}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

fn write_file(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write fixture file");
    path
}

async fn create_project(env: &E2eEnv, name: &str, roots: &[PathBuf]) -> pb::ProjectInfo {
    env.client
        .create_project(&pb::CreateProjectRequest {
            name: name.to_string(),
            roots: roots.iter().map(|r| r.display().to_string()).collect(),
        })
        .await
        .expect("create project")
}

/// Create a session bound to `project`. `workdir` unset defaults to the
/// Project's primary root (ADR-0024).
async fn create_session_in_project(
    env: &E2eEnv,
    project: &str,
    workdir: Option<&Path>,
) -> SessionId {
    let resp = env
        .client
        .create_session(&pb::CreateSessionRequest {
            agent: env.agent.clone(),
            model: env.model.clone(),
            workdir: workdir.map(|w| w.display().to_string()),
            project_id: project.to_string(),
            ..Default::default()
        })
        .await
        .expect("create session in project");
    resp.session
        .and_then(|s| s.id.parse().ok())
        .expect("session id in create-session response")
}

/// Run `prompt`, replying `reply` to the first permission ask seen while it
/// runs and returning that ask's interaction row. `None` when the turn never
/// asks at all (the tool call and the FakeLlm text step still run through).
async fn prompt_capturing_ask(
    env: &E2eEnv,
    session: SessionId,
    prompt_text: &str,
    reply: &str,
) -> (Result<pb::TurnInfo, E2eError>, Option<Value>) {
    let watch = async {
        let id = env.wait_permission_id(Duration::from_secs(10)).await.ok()?;
        let pending = env.list_permissions().await.ok()?;
        let row = pending
            .as_array()?
            .iter()
            .find(|row| row["id"] == json!(id))?
            .clone();
        env.reply_permission(&id, reply).await.ok()?;
        Some(row)
    };
    tokio::join!(env.prompt(session, prompt_text), watch)
}

fn assert_no_pending_permissions(env: &E2eEnv, pending: &Value, context: &str) {
    assert!(
        pending.as_array().is_none_or(Vec::is_empty),
        "{context}; diagnostics={}",
        env.diagnostics()
    );
}

#[tokio::test]
async fn t2_32_manual_mode_project_scoped_external_directory_ask() {
    let root_a = fixture_dir("root-a");
    let root_b = fixture_dir("root-b");
    let root_d = fixture_dir("root-d");
    let outside = fixture_dir("outside");
    let in_b = write_file(&root_b, "in-b.txt", "READ_B_MARKER");
    let foo = write_file(&outside, "foo.txt", "READ_C_FOO_MARKER");
    let bar = write_file(&outside, "bar.txt", "READ_C_BAR_MARKER");

    let env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("default")
        .scripts(vec![
            tool_step("read", json!({ "path": in_b.display().to_string() })),
            text_step("READ_B_DONE"),
            tool_step("read", json!({ "path": foo.display().to_string() })),
            text_step("READ_C_FOO_DONE"),
            tool_step("read", json!({ "path": bar.display().to_string() })),
            text_step("READ_C_BAR_DONE"),
            tool_step("read", json!({ "path": foo.display().to_string() })),
            text_step("READ_C_FOO_AGAIN_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let project1 = create_project(&env, "proj-scope-1", &[root_a.clone(), root_b.clone()]).await;
    let session1 = create_session_in_project(&env, &project1.id, None).await;

    // 1. A read inside the Project's second root never asks.
    let before = env.fake_requests().expect("requests").len();
    env.prompt(session1, "read the file in root b")
        .await
        .expect("read in root b must not block on a permission ask");
    assert_no_pending_permissions(
        &env,
        &env.list_permissions().await.expect("list permissions"),
        "a read inside a Project root must never ask",
    );
    let requests = env.fake_requests().expect("requests");
    assert!(
        fake_requests_from(&requests, before).contains("READ_B_MARKER"),
        "follow-up request must carry root-b's content"
    );

    // 2. A read outside every root asks `ExternalDirectory` for the
    //    concrete `<dir>/*` pattern of the outside directory.
    let before = env.fake_requests().expect("requests").len();
    let (turn, ask) =
        prompt_capturing_ask(&env, session1, "read foo outside the project", "always").await;
    turn.expect("read outside the project, answered allow-always");
    let ask = ask.expect("a read outside every root must ask ExternalDirectory");
    assert_eq!(ask["payload"]["action"], json!("externaldirectory"));
    assert_eq!(
        ask["payload"]["resource"],
        json!(format!("{}/*", outside.display())),
        "the ask names the concrete outside directory, not a global `*`"
    );
    let requests = env.fake_requests().expect("requests");
    assert!(fake_requests_from(&requests, before).contains("READ_C_FOO_MARKER"));

    // 3. A second session of the *same* Project reads a sibling file in the
    //    same outside directory without asking again: allow-always covers
    //    the concrete directory for the Project.
    let session2 = create_session_in_project(&env, &project1.id, None).await;
    let before = env.fake_requests().expect("requests").len();
    env.prompt(session2, "read bar outside the project")
        .await
        .expect("read bar must not block on a permission ask");
    assert_no_pending_permissions(
        &env,
        &env.list_permissions().await.expect("list permissions"),
        "allow-always on the directory must cover a sibling file for the same Project",
    );
    let requests = env.fake_requests().expect("requests");
    assert!(fake_requests_from(&requests, before).contains("READ_C_BAR_MARKER"));

    // 4. A session of a *different* Project asks again for the very same
    //    outside directory: the remembered grant is scoped to the Project.
    let project2 = create_project(&env, "proj-scope-2", std::slice::from_ref(&root_d)).await;
    let session3 = create_session_in_project(&env, &project2.id, None).await;
    let (turn, ask) = prompt_capturing_ask(
        &env,
        session3,
        "read foo again from a different project",
        "once",
    )
    .await;
    turn.expect("read outside from a different project, answered once");
    assert!(
        ask.is_some(),
        "a different Project must ask again for the same outside directory"
    );
}

#[tokio::test]
async fn t2_33_yolo_mode_bypasses_the_external_directory_ask() {
    let root = fixture_dir("yolo-root");
    let outside = fixture_dir("yolo-outside");
    let target = write_file(&outside, "yolo-target.txt", "READ_YOLO_MARKER");

    // Default builder is `yolo(true)`.
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step("read", json!({ "path": target.display().to_string() })),
            text_step("READ_YOLO_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let project = create_project(&env, "proj-yolo", std::slice::from_ref(&root)).await;
    let session = create_session_in_project(&env, &project.id, None).await;

    let before = env.fake_requests().expect("requests").len();
    env.prompt(session, "read a file outside the project in yolo mode")
        .await
        .expect("yolo read outside the project must not block");
    assert_no_pending_permissions(
        &env,
        &env.list_permissions().await.expect("list permissions"),
        "yolo mode must auto-approve ExternalDirectory without ever asking",
    );
    let requests = env.fake_requests().expect("requests");
    assert!(fake_requests_from(&requests, before).contains("READ_YOLO_MARKER"));
}

#[tokio::test]
async fn t2_34_bash_cwd_outside_the_roots_never_asks_external_directory() {
    let root = fixture_dir("bash-root");
    let outside = fixture_dir("bash-outside");

    let env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("default")
        .scripts(vec![
            tool_step(
                "bash",
                json!({
                    "command": "printf bash-cwd-ok > marker.txt",
                    "cwd": outside.display().to_string(),
                }),
            ),
            text_step("BASH_CWD_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let project = create_project(&env, "proj-bash-cwd", std::slice::from_ref(&root)).await;
    let session = create_session_in_project(&env, &project.id, None).await;

    // `bash` still asks `Action::Bash` for the command itself in manual mode
    // (T1.7); drain any such asks with "once" while asserting none of them
    // is `ExternalDirectory` for the outside `cwd`.
    let mut seen_actions = Vec::new();
    let drain = async {
        loop {
            if let Ok(pending) = env.list_permissions().await
                && let Some(rows) = pending.as_array()
                && let Some(row) = rows.first()
            {
                let id = row["id"].as_str().unwrap_or_default().to_string();
                let action = row["payload"]["action"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                seen_actions.push(action);
                if !id.is_empty() {
                    let _ = env.reply_permission(&id, "once").await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };

    tokio::select! {
        result = env.prompt(session, "run bash with an outside cwd") => {
            result.expect("bash with an outside cwd must complete");
        }
        () = drain => {
            unreachable!("drain loop never returns");
        }
    }

    assert!(
        !seen_actions.iter().any(|a| a == "externaldirectory"),
        "bash's cwd must never raise ExternalDirectory; saw actions={seen_actions:?}; diagnostics={}",
        env.diagnostics()
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("marker.txt")).expect("bash side effect file"),
        "bash-cwd-ok",
        "bash must be free to write into a cwd outside every Project root"
    );
}
