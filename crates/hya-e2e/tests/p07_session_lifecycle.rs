//! T1.11 — session list + resume (second prompt on same session).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step};
use hya_proto::Event;

#[tokio::test]
async fn t1_11_session_list_and_resume_prompt() {
    // Compat /session hides empty_unnamed sessions until they have activity.
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            text_step("SESSION_FIRST"),
            text_step("SESSION_B_TURN"),
            text_step("SESSION_RESUMED"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session_a = env.create_session().await.expect("session a");
    let session_b = env.create_session().await.expect("session b");
    assert_ne!(session_a.to_string(), session_b.to_string());

    let _ = env
        .prompt(session_a, "first turn")
        .await
        .expect("first prompt");
    let _ = env
        .prompt(session_b, "second session")
        .await
        .expect("session b prompt");

    let listed = env.list_sessions_compat().await.expect("list sessions");
    let listed_text = listed.to_string();
    assert!(
        listed_text.contains(&session_a.to_string()),
        "session A must appear in list; listed={listed}; {}",
        env.diagnostics()
    );
    assert!(
        listed_text.contains(&session_b.to_string()),
        "session B must appear in list after activity; listed={listed}; {}",
        env.diagnostics()
    );

    let _ = env
        .prompt(session_a, "resume turn")
        .await
        .expect("resume prompt");

    let events = env.events(session_a, None).await.expect("events");
    let mut text = String::new();
    for env_evt in events {
        match env_evt.event {
            Event::TextDelta { delta, .. } => text.push_str(&delta),
            Event::TextReplace { text: t, .. } => text.push_str(&t),
            _ => {}
        }
    }
    assert!(
        text.contains("SESSION_FIRST"),
        "first turn text missing; text={text:?}; {}",
        env.diagnostics()
    );
    assert!(
        text.contains("SESSION_RESUMED"),
        "resume turn text missing; text={text:?}; {}",
        env.diagnostics()
    );
    assert!(
        env.fake.requests().unwrap().len() >= 3,
        "three prompts expected; {}",
        env.diagnostics()
    );
}

/// T1.11 — the production backend titles a root session in the background
/// after its first prompt (fixed `hya-title` agent), once: the title call never
/// consumes the turn's scripted steps, and a second prompt does not retitle.
#[tokio::test]
async fn t1_11_first_prompt_titles_the_session_once() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            text_step("TITLE_TURN_ONE"),
            text_step("TITLE_TURN_TWO"),
        ])
        .build()
        .await
        .expect("e2e env");
    env.fake
        .set_title_reply("Widget audit")
        .expect("title reply");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "audit the widget module")
        .await
        .expect("first prompt");
    let url = format!("{}/v1/sessions/{session}", env.backend.url);
    let mut title = serde_json::Value::Null;
    for _ in 0..200 {
        let info: serde_json::Value = reqwest::get(&url)
            .await
            .expect("get session")
            .json()
            .await
            .expect("session json");
        title = info["title"].clone();
        if !title.is_null() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(
        title,
        serde_json::json!("Widget audit"),
        "{}",
        env.diagnostics()
    );

    let _ = env
        .prompt(session, "now the gadget module")
        .await
        .expect("second prompt");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(env.fake.title_requests().expect("title requests").len(), 1);
    assert_eq!(
        env.fake.remaining_scripts().expect("remaining"),
        0,
        "both turns consumed their own steps"
    );
    let titled = env
        .events(session, None)
        .await
        .expect("events")
        .into_iter()
        .filter(|envelope| matches!(envelope.event, Event::SessionTitled { .. }))
        .count();
    assert_eq!(titled, 1, "{}", env.diagnostics());
}

/// A daemon started by the test must also stop during panic unwinding.
struct RestartDaemon<'a>(&'a hya_e2e::BackendProcess);

impl Drop for RestartDaemon<'_> {
    fn drop(&mut self) {
        let db = self.0.db.to_str().unwrap();
        let _ = self
            .0
            .cli(&["serve", "stop", "--force", "--timeout", "5", "--db", db]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_start_restores_a_turn_interrupted_in_a_tool() {
    check_stop_start_restore(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_start_cancels_an_unanswered_permission_without_tool_success() {
    check_stop_start_restore(true).await;
}

async fn check_stop_start_restore(unanswered_permission: bool) {
    use hya_e2e::{tool_step, wait_until};
    use hya_proto::{FinishCause, Role};
    use serde_json::json;
    use std::time::Duration;

    let mut env = E2eEnvBuilder::new()
        .yolo(!unanswered_permission)
        .permission_model("default")
        .scripts(vec![
            tool_step("bash", json!({"command": "printf started > restart-started.txt; sleep 600; printf finished > restart-finished.txt"})),
            text_step("RESTORE_COMPLETED"),
        ])
        .build().await.unwrap();
    let session = env.create_session().await.unwrap();
    env.client
        .create_turn(session, "work until complete")
        .await
        .unwrap();
    let permission = if unanswered_permission {
        Some(
            env.wait_permission_id(Duration::from_secs(30))
                .await
                .unwrap(),
        )
    } else {
        wait_until("tool starts", Duration::from_secs(30), || async {
            Ok(env.project_path("restart-started.txt").exists())
        })
        .await
        .unwrap();
        None
    };
    let db = env.backend.db.to_str().unwrap().to_string();
    let stopped = env.backend.cli(&["serve", "stop", "--db", &db]).unwrap();
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(env.backend.shutdown().unwrap().success());
    let _daemon = RestartDaemon(&env.backend);
    let mut args = vec![
        "serve",
        "start",
        "--db",
        &db,
        "--model",
        "fake/model",
        "--json",
    ];
    if !unanswered_permission {
        args.push("--yolo");
    }
    let started = env.backend.cli(&args).unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let ready: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let url = ready["url"].as_str().unwrap();
    let client = hya_client::Client::new(url);
    let restored = wait_until(
        "restored turn completes",
        Duration::from_secs(30),
        || async {
            Ok(client.events(session, None).await.unwrap().iter().any(|e| {
                matches!(&e.event,
            Event::TextDelta { delta, .. } | Event::TextReplace { text: delta, .. } if delta.contains("RESTORE_COMPLETED"))
            }))
        },
    )
    .await;
    if let Err(error) = restored {
        panic!(
            "{error}; events={:?}; request_count={}; daemon log={}",
            client.events(session, None).await.unwrap(),
            env.fake_requests().unwrap().len(),
            std::fs::read_to_string(format!("{db}.server.log")).unwrap()
        );
    }
    let events = client.events(session, None).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        e.event,
        Event::MessageFinished {
            role: Role::Assistant,
            cause: Some(FinishCause::Shutdown),
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, Event::ToolResult { .. }))
    );
    let restores: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::TextDelta { delta, .. }
                if delta.contains("Your previous turn was interrupted by a restart.") =>
            {
                Some(delta)
            }
            _ => None,
        })
        .collect();
    assert_eq!(restores.len(), 1);
    assert!(restores[0].contains("bash (call "));
    assert!(env.fake_requests().unwrap().iter().any(|request| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "user"
                    && message["content"].as_str() == Some(restores[0].as_str())
            })
    }));
    assert!(!env.project_path("restart-finished.txt").exists());
    if let Some(permission) = permission {
        assert!(!env.project_path("restart-started.txt").exists());
        assert!(client.list_interactions(None).await.unwrap().is_empty());
        let late = env
            .http
            .post(format!("{url}/v1/interactions/{permission}/respond"))
            .json(&json!({"permission": {"allowed": true}}))
            .send()
            .await
            .unwrap();
        assert_eq!(late.status(), reqwest::StatusCode::NOT_FOUND);
    }
    // Starting an already running daemon must not inject another restore.
    let again = env.backend.cli(&args).unwrap();
    assert!(again.status.success());
    assert_eq!(client.events(session, None).await.unwrap().iter().filter(|e| matches!(&e.event,
        Event::TextDelta { delta, .. } if delta.contains("Your previous turn was interrupted by a restart."))).count(), 1);
}
