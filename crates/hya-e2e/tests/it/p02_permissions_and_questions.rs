//! T1.7, T1.8, T2.32–T2.34 — permissions (`once`/`reject`/`always`/mid-session `yolo`),
//! ADR-0026 multi-root Project `ExternalDirectory` scope + `bash` cwd exemption,
//! `--yolo` bypass, and `ask_user` / legacy `question` interaction plane.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_api::v2 as pb;
use hya_e2e::{
    E2eEnv, E2eEnvBuilder, E2eError, fake_requests_from, text_step, tool_step, unique_temp_dir,
};
use hya_proto::{Event, SessionId};
use serde_json::{Value, json};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        Self(unique_temp_dir(label).expect("create fixture dir"))
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write_file(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, body).expect("write fixture file");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn create_project(env: &E2eEnv, name: &str, roots: &[&Path]) -> pb::ProjectInfo {
    env.client
        .create_project(&pb::CreateProjectRequest {
            name: name.to_string(),
            roots: roots.iter().map(|r| r.display().to_string()).collect(),
        })
        .await
        .expect("create project")
}

async fn create_session_in_project(env: &E2eEnv, project: &str) -> SessionId {
    let resp = env
        .client
        .create_session(&pb::CreateSessionRequest {
            agent: env.agent.clone(),
            model: env.model.clone(),
            workdir: None,
            project_id: project.to_string(),
            ..Default::default()
        })
        .await
        .expect("create session in project");
    resp.session
        .and_then(|s| s.id.parse().ok())
        .expect("session id")
}

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

fn assert_no_pending(env: &E2eEnv, pending: &Value, context: &str) {
    assert!(
        pending.as_array().is_none_or(Vec::is_empty),
        "{context}; pending={pending}; {}",
        env.diagnostics()
    );
}

fn collect_text(events: &[hya_proto::Envelope]) -> String {
    let mut out = String::new();
    for env_evt in events {
        match &env_evt.event {
            Event::TextDelta { delta, .. } => out.push_str(delta),
            Event::TextReplace { text, .. } => out.push_str(text),
            _ => {}
        }
    }
    out
}

/// T1.7, T2.32, T2.34 on one manual-mode (`yolo(false)`) backend.
#[tokio::test]
async fn manual_permissions_project_scope_and_yolo_switch() {
    let root_a = TempDir::new("scope-root-a");
    let root_b = TempDir::new("scope-root-b");
    let root_d = TempDir::new("scope-root-d");
    let outside = TempDir::new("scope-outside");
    let bash_root = TempDir::new("scope-bash-root");
    let bash_outside = TempDir::new("scope-bash-outside");

    let in_b = root_b.write_file("in-b.txt", "READ_B_MARKER");
    let foo = outside.write_file("foo.txt", "READ_C_FOO_MARKER");
    let bar = outside.write_file("bar.txt", "READ_C_BAR_MARKER");

    let env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("default")
        .build()
        .await
        .expect("e2e env");

    // 1. T1.7: `once` permits disk side effect; `reject` blocks disk side effect.
    let perm_session = env.create_session().await.expect("perm session");
    env.fake
        .push_scripts([
            tool_step(
                "bash",
                json!({ "command": "printf once-ok > e2e-perm-once.txt" }),
            ),
            text_step("PERM_ONCE_DONE"),
            tool_step(
                "bash",
                json!({ "command": "printf reject-leak > e2e-perm-reject.txt" }),
            ),
            text_step("PERM_REJECT_DONE"),
        ])
        .unwrap();
    env.prompt_with_permission_reply(perm_session, "run once", "once", Duration::from_secs(30))
        .await
        .expect("once turn");
    assert_eq!(
        env.read_project_file("e2e-perm-once.txt").unwrap(),
        "once-ok"
    );

    env.prompt_with_permission_reply(
        perm_session,
        "run reject",
        "reject",
        Duration::from_secs(30),
    )
    .await
    .expect("reject turn");
    assert!(
        !env.project_path("e2e-perm-reject.txt").exists(),
        "rejected shell must not write file"
    );

    // 3. T2.32: multi-root Project scope + concrete ExternalDirectory allow-always per Project.
    let project1 = create_project(&env, "proj-scope-1", &[root_a.path(), root_b.path()]).await;
    let session1 = create_session_in_project(&env, &project1.id).await;

    let before = env.fake_requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("read", json!({ "path": in_b.display().to_string() })),
            text_step("READ_B_DONE"),
        ])
        .unwrap();
    env.prompt(session1, "read in root b")
        .await
        .expect("in-root read must not ask");
    assert_no_pending(
        &env,
        &env.list_permissions().await.unwrap(),
        "root #2 read must never ask",
    );
    assert!(fake_requests_from(&env.fake_requests().unwrap(), before).contains("READ_B_MARKER"));

    let before = env.fake_requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("read", json!({ "path": foo.display().to_string() })),
            text_step("READ_C_FOO_DONE"),
        ])
        .unwrap();
    let (turn, ask) = prompt_capturing_ask(&env, session1, "read outside foo", "always").await;
    turn.expect("outside read answered always");
    let ask = ask.expect("outside read must ask ExternalDirectory");
    assert_eq!(ask["payload"]["action"], json!("externaldirectory"));
    assert_eq!(
        ask["payload"]["resource"],
        json!(format!(
            "{}/*",
            std::fs::canonicalize(outside.path()).unwrap().display()
        ))
    );
    assert!(
        fake_requests_from(&env.fake_requests().unwrap(), before).contains("READ_C_FOO_MARKER")
    );

    let session2 = create_session_in_project(&env, &project1.id).await;
    let before = env.fake_requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("read", json!({ "path": bar.display().to_string() })),
            text_step("READ_C_BAR_DONE"),
        ])
        .unwrap();
    env.prompt(session2, "read sibling bar")
        .await
        .expect("sibling session covered by project allow-always");
    assert_no_pending(
        &env,
        &env.list_permissions().await.unwrap(),
        "allow-always must cover sibling session of same Project",
    );
    assert!(
        fake_requests_from(&env.fake_requests().unwrap(), before).contains("READ_C_BAR_MARKER")
    );

    let project2 = create_project(&env, "proj-scope-2", &[root_d.path()]).await;
    let session3 = create_session_in_project(&env, &project2.id).await;
    env.fake
        .push_scripts([
            tool_step("read", json!({ "path": foo.display().to_string() })),
            text_step("READ_C_FOO_AGAIN_DONE"),
        ])
        .unwrap();
    let (turn, ask) =
        prompt_capturing_ask(&env, session3, "read foo from different project", "once").await;
    turn.expect("different project outside read");
    assert!(
        ask.is_some(),
        "a different Project must ask again for the same outside directory"
    );

    // 4. T2.34: `bash` with cwd outside every root asks Action::Bash, NEVER ExternalDirectory.
    let project3 = create_project(&env, "proj-bash-cwd", &[bash_root.path()]).await;
    let bash_session = create_session_in_project(&env, &project3.id).await;
    env.fake
        .push_scripts([
            tool_step(
                "bash",
                json!({
                    "command": "printf bash-cwd-ok > marker.txt",
                    "cwd": bash_outside.path().display().to_string(),
                }),
            ),
            text_step("BASH_CWD_DONE"),
        ])
        .unwrap();
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
        result = env.prompt(bash_session, "run bash with outside cwd") => {
            result.expect("bash with outside cwd");
        }
        () = drain => unreachable!(),
    }
    assert!(
        !seen_actions.iter().any(|a| a == "externaldirectory"),
        "bash cwd must never raise ExternalDirectory: {seen_actions:?}"
    );
    assert_eq!(
        std::fs::read_to_string(bash_outside.path().join("marker.txt")).unwrap(),
        "bash-cwd-ok"
    );

    // 5. T1.7: switching mid-session to `yolo` unblocks the pending ask and skips later asks.
    let yolo_session = env.create_session().await.expect("yolo session");
    let info = env
        .get_json(&format!("/v2/sessions/{yolo_session}"))
        .await
        .unwrap();
    assert_eq!(info["permissionMode"], json!("manual"));
    env.fake
        .push_scripts([
            tool_step(
                "bash",
                json!({ "command": "printf first > e2e-yolo-first.txt" }),
            ),
            text_step("YOLO_FIRST_DONE"),
            tool_step(
                "bash",
                json!({ "command": "printf second > e2e-yolo-second.txt" }),
            ),
            text_step("YOLO_SECOND_DONE"),
        ])
        .unwrap();
    let switch = async {
        let _ = env
            .wait_permission_id(Duration::from_secs(30))
            .await
            .expect("first shell asks");
        env.patch_json(
            &format!("/v2/sessions/{yolo_session}"),
            &json!({ "permissionMode": "yolo" }),
        )
        .await
        .expect("switch to yolo")
    };
    let (first, updated) = tokio::join!(env.prompt(yolo_session, "first shell"), switch);
    first.expect("first turn finishes after yolo switch");
    assert_eq!(updated["permissionMode"], json!("yolo"));
    assert_eq!(
        env.read_project_file("e2e-yolo-first.txt").unwrap(),
        "first"
    );

    env.prompt(yolo_session, "second shell")
        .await
        .expect("second turn runs without asking");
    assert_eq!(
        env.read_project_file("e2e-yolo-second.txt").unwrap(),
        "second"
    );
    assert_no_pending(
        &env,
        &env.list_permissions().await.unwrap(),
        "nothing pending after yolo switch",
    );
}

/// T1.8 (`ask_user` + legacy `question` alias) and T2.33 (`--yolo` bypasses
/// `ExternalDirectory` on reads outside every Project root).
#[tokio::test]
async fn yolo_external_directory_and_questions() {
    let root = TempDir::new("yolo-root");
    let outside = TempDir::new("yolo-outside");
    let target = outside.write_file("yolo-target.txt", "READ_YOLO_MARKER");

    let env = E2eEnvBuilder::new().build().await.expect("e2e env");

    // 1. T2.33: `--yolo` auto-approves reads outside every Project root without asking.
    let project = create_project(&env, "proj-yolo", &[root.path()]).await;
    let session = create_session_in_project(&env, &project.id).await;
    let before = env.fake_requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("read", json!({ "path": target.display().to_string() })),
            text_step("READ_YOLO_DONE"),
        ])
        .unwrap();
    env.prompt(session, "read outside file in yolo mode")
        .await
        .expect("yolo outside read");
    assert_no_pending(
        &env,
        &env.list_permissions().await.unwrap(),
        "yolo mode must not raise ExternalDirectory",
    );
    assert!(fake_requests_from(&env.fake_requests().unwrap(), before).contains("READ_YOLO_MARKER"));

    // 2. T1.8: `ask_user` canonical tool + `plan_exit` offered + legacy `question` alias.
    let q_session = env.create_session().await.expect("q session");
    let before_q = env.fake_requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step(
                "ask_user",
                json!({
                    "questions": [{
                        "question": "Ship it?",
                        "header": "confirm",
                        "options": [
                            { "label": "yes", "description": "proceed" },
                            { "label": "no", "description": "abort" }
                        ]
                    }]
                }),
            ),
            text_step("QUESTION_ANSWERED"),
            tool_step(
                "question",
                json!({
                    "questions": [{
                        "question": "Proceed?",
                        "header": "confirm",
                        "options": [{ "label": "yes", "description": "proceed" }]
                    }]
                }),
            ),
            text_step("ALIAS_ANSWERED"),
        ])
        .unwrap();
    env.prompt_with_question_reply(
        q_session,
        "ask the user",
        json!([["yes"]]),
        Duration::from_secs(30),
    )
    .await
    .expect("ask_user turn");
    let tools = env.fake.requests().unwrap()[before_q]["tools"]
        .as_array()
        .cloned()
        .expect("tools");
    for name in ["ask_user", "plan_exit"] {
        assert!(
            tools.iter().any(|t| t["function"]["name"] == name),
            "server must offer {name}"
        );
    }
    env.prompt_with_question_reply(
        q_session,
        "ask via legacy alias",
        json!([["yes"]]),
        Duration::from_secs(30),
    )
    .await
    .expect("question alias turn");
    let q_text = collect_text(&env.events(q_session, None).await.unwrap());
    assert!(
        q_text.contains("QUESTION_ANSWERED") && q_text.contains("ALIAS_ANSWERED"),
        "both ask_user and legacy question must complete: {q_text:?}"
    );
}
