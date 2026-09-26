//! `RevertSession` and `ForkSession` over `/v1`: revert hides the reverted
//! turns and restores their files, undo brings both back, the next prompt
//! commits; fork copies up to the head (the last message included), before a
//! user message, or up to a sequence, and records its source.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef, SessionId};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fixture {
    app: axum::Router,
    engine: Arc<SessionEngine>,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write_turn(content: &str) -> Vec<Vec<FakeStep>> {
    vec![
        vec![
            FakeStep::ToolCall {
                name: "write".to_string(),
                input: json!({"path": "a.txt", "content": content}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]
}

async fn fixture(label: &str, script: Vec<Vec<FakeStep>>) -> Fixture {
    let dir = support::tempdir(label).canonicalize().unwrap();
    let (perm, _asks) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Edit, "**", Mode::Allow),
        Rule::new(Action::Read, "**", Mode::Allow),
    ]));
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(script)))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    ));
    let state = AppState::new(
        Arc::clone(&engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: dir.clone(),
            reasoning: None,
        }),
    );
    Fixture {
        app: router(state),
        engine,
        dir,
    }
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

impl Fixture {
    async fn session(&self) -> String {
        let (status, created) = call(
            &self.app,
            Method::POST,
            "/v1/sessions",
            json!({"agent": "build", "model": "fake", "workdir": self.dir.to_string_lossy()}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        created["session"]["id"].as_str().unwrap().to_owned()
    }

    async fn prompt(&self, session: &str, text: &str) {
        let (status, created) = call(
            &self.app,
            Method::POST,
            &format!("/v1/sessions/{session}/turns"),
            json!({"prompt": {"text": text}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let turn = created["turn"]["id"].as_str().unwrap().to_owned();
        let (status, waited) = call(
            &self.app,
            Method::POST,
            &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{waited}");
        assert_eq!(waited["state"], json!("TURN_STATE_FINISHED"), "{waited}");
        // The admission slot is released after the turn task ends.
        for _ in 0..200 {
            let (_, info) = call(
                &self.app,
                Method::GET,
                &format!("/v1/sessions/{session}"),
                Value::Null,
            )
            .await;
            if info["busy"] != json!(true) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("session {session} stayed busy");
    }

    async fn messages(&self, session: &str) -> Vec<Value> {
        let (status, listed) = call(
            &self.app,
            Method::GET,
            &format!("/v1/sessions/{session}/messages"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        listed["messages"].as_array().cloned().unwrap_or_default()
    }

    async fn revert(&self, session: &str, body: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            Method::POST,
            &format!("/v1/sessions/{session}/revert"),
            body,
        )
        .await
    }

    async fn fork(&self, session: &str, body: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            Method::POST,
            &format!("/v1/sessions/{session}/fork"),
            body,
        )
        .await
    }
}

fn user_ids(messages: &[Value]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message["role"] == json!("ROLE_USER"))
        .map(|message| message["id"].as_str().unwrap().to_owned())
        .collect()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[tokio::test]
async fn revert_hides_the_turn_restores_files_and_undo_brings_both_back() {
    let fx = fixture("v1-revert", [write_turn("one"), write_turn("two")].concat()).await;
    let a = fx.dir.join("a.txt");
    let session = fx.session().await;
    fx.prompt(&session, "first").await;
    fx.prompt(&session, "second").await;
    let all = fx.messages(&session).await;
    assert_eq!(all.len(), 4);
    let second = user_ids(&all)[1].clone();

    let (status, reverted) = fx.revert(&session, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{reverted}");
    let revert = &reverted["session"]["revert"];
    assert_eq!(revert["messageId"], json!(second));
    assert_eq!(revert["text"], json!("second"));
    assert_eq!(revert["hiddenMessages"], json!(2));
    assert_eq!(revert["files"][0]["path"], json!(a.to_string_lossy()));
    assert_eq!(revert["files"][0]["action"], json!("restored"));
    assert_eq!(reverted["files"][0]["action"], json!("restored"));
    assert_eq!(read(&a).as_deref(), Some("one"));
    assert_eq!(fx.messages(&session).await.len(), 2);

    // The durable stream carries the revert.
    let (status, events) = call(
        &fx.app,
        Method::GET,
        &format!("/v1/sessions/{session}/events"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let frame = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|event| event.get("sessionReverted"))
        .expect("sessionReverted frame")
        .clone();
    assert_eq!(frame["messageId"], json!(second));
    assert_eq!(frame["files"][0]["action"], json!("restored"));

    let (status, undone) = fx.revert(&session, json!({"undo": true})).await;
    assert_eq!(status, StatusCode::OK, "{undone}");
    assert!(undone["session"].get("revert").is_none(), "{undone}");
    assert_eq!(read(&a).as_deref(), Some("two"));
    assert_eq!(fx.messages(&session).await.len(), 4);

    let (status, error) = fx.revert(&session, json!({"undo": true})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], json!("invalid_argument"));
}

#[tokio::test]
async fn the_next_prompt_commits_the_revert() {
    let fx = fixture(
        "v1-revert-commit",
        [write_turn("one"), write_turn("two"), write_turn("three")].concat(),
    )
    .await;
    let session = fx.session().await;
    fx.prompt(&session, "first").await;
    fx.prompt(&session, "second").await;
    let (status, _) = fx.revert(&session, json!({})).await;
    assert_eq!(status, StatusCode::OK);

    fx.prompt(&session, "third").await;

    let messages = fx.messages(&session).await;
    assert_eq!(messages.len(), 4);
    let (_, info) = call(
        &fx.app,
        Method::GET,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert!(info.get("revert").is_none(), "{info}");
    let (status, _) = fx.revert(&session, json!({"undo": true})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(read(&fx.dir.join("a.txt")).as_deref(), Some("three"));
}

#[tokio::test]
async fn revert_rejects_bad_targets_and_busy_sessions() {
    let fx = fixture("v1-revert-errors", write_turn("one")).await;
    let session = fx.session().await;
    let (status, error) = fx.revert(&session, json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");

    fx.prompt(&session, "first").await;
    let messages = fx.messages(&session).await;
    let assistant = messages[1]["id"].as_str().unwrap().to_owned();
    let (status, error) = fx.revert(&session, json!({"messageId": assistant})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    let (status, error) = fx
        .revert(
            &session,
            json!({"messageId": "msg_00000000-0000-7000-8000-000000000000"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"]["code"], json!("not_found"));
    let (status, error) = fx.revert(&session, json!({"untilSeq": "3"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");

    let id: SessionId = session.parse().unwrap();
    let lease = fx.engine.try_begin_turn(id).unwrap();
    let (status, error) = fx.revert(&session, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], json!("session_busy"));
    drop(lease);
    let (status, _) = fx.revert(&session, json!({})).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn fork_copies_the_head_including_the_last_message() {
    let fx = fixture(
        "v1-fork-head",
        [write_turn("one"), write_turn("two")].concat(),
    )
    .await;
    let session = fx.session().await;
    fx.prompt(&session, "first").await;
    fx.prompt(&session, "second").await;

    let (status, forked) = fx.fork(&session, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    let fork = forked["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(forked["session"]["forkedFrom"]["session"], json!(session));
    assert!(forked["session"]["forkedFrom"].get("messageId").is_none());
    assert!(forked.get("promptText").is_none());
    let copied = fx.messages(&fork).await;
    assert_eq!(copied.len(), 4);
    assert_eq!(copied[3]["role"], json!("ROLE_ASSISTANT"));
}

#[tokio::test]
async fn fork_at_a_user_message_or_a_sequence_cuts_before_it() {
    let fx = fixture(
        "v1-fork-cut",
        [write_turn("one"), write_turn("two")].concat(),
    )
    .await;
    let session = fx.session().await;
    fx.prompt(&session, "first").await;
    fx.prompt(&session, "second").await;
    let messages = fx.messages(&session).await;
    let second = user_ids(&messages)[1].clone();

    let (status, forked) = fx.fork(&session, json!({"messageId": second})).await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    assert_eq!(forked["promptText"], json!("second"));
    assert_eq!(forked["session"]["forkedFrom"]["messageId"], json!(second));
    let fork = forked["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(fx.messages(&fork).await.len(), 2);

    // `untilSeq` just before the second prompt started: the same cut.
    let (_, events) = call(
        &fx.app,
        Method::GET,
        &format!("/v1/sessions/{session}/events"),
        Value::Null,
    )
    .await;
    let started: u64 = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["messageStarted"]["message"] == json!(second))
        .and_then(|event| event["seq"].as_str())
        .unwrap()
        .parse()
        .unwrap();
    let (status, forked) = fx
        .fork(&session, json!({"untilSeq": (started - 1).to_string()}))
        .await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    let fork = forked["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(fx.messages(&fork).await.len(), 2);

    let assistant = messages[1]["id"].as_str().unwrap().to_owned();
    let (status, _) = fx.fork(&session, json!({"messageId": assistant})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = fx
        .fork(
            &session,
            json!({"messageId": "msg_00000000-0000-7000-8000-000000000000"}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_fork_of_a_reverted_session_copies_only_the_visible_messages() {
    let fx = fixture(
        "v1-fork-reverted",
        [write_turn("one"), write_turn("two")].concat(),
    )
    .await;
    let session = fx.session().await;
    fx.prompt(&session, "first").await;
    fx.prompt(&session, "second").await;
    let (status, _) = fx.revert(&session, json!({})).await;
    assert_eq!(status, StatusCode::OK);

    let (status, forked) = fx.fork(&session, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    let fork = forked["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(fx.messages(&fork).await.len(), 2);
}

/// A fork is titled after its source (`<title> (fork)`, the source id when
/// the source is untitled), never stacks the suffix, and is not renamed by
/// automatic titling later.
#[tokio::test]
async fn a_fork_is_titled_after_its_source_and_keeps_that_title() {
    let fx = fixture(
        "v1-fork-title",
        [write_turn("one"), write_turn("two")].concat(),
    )
    .await;
    let session = fx.session().await;
    fx.prompt(&session, "first").await;

    let (status, forked) = fx.fork(&session, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    assert_eq!(
        forked["session"]["title"],
        json!(format!("{session} (fork)")),
        "an untitled source falls back to its id"
    );

    let source: hya_proto::SessionId = session.parse().unwrap();
    fx.engine
        .set_title(source, "Plan the work".to_owned())
        .await
        .unwrap();
    let (status, forked) = fx.fork(&session, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    assert_eq!(forked["session"]["title"], json!("Plan the work (fork)"));
    let fork = forked["session"]["id"].as_str().unwrap().to_owned();

    let (status, again) = fx.fork(&fork, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        again["session"]["title"],
        json!("Plan the work (fork)"),
        "a fork of a fork does not stack the suffix"
    );

    let fork_id: hya_proto::SessionId = fork.parse().unwrap();
    assert!(
        !fx.engine
            .auto_title_session(fork_id, &ModelRef::new("fake"))
            .await
            .unwrap(),
        "automatic titling never renames a fork"
    );
}
