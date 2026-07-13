#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef, SessionId};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{
    Action, InteractionPlane, PermissionPlane, PermissionRules, QuestionAnswer, QuestionKind,
    Resource, ToolRegistry,
};
use serde_json::{Value, json};
use tower::ServiceExt;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-server-permission-question-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn base_engine(
    permission: PermissionPlane,
    interaction: Option<InteractionPlane>,
    workdir: PathBuf,
) -> (Arc<SessionEngine>, Arc<AgentSpec>) {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let store = SessionStore::connect_memory().await.unwrap();
    let mut engine = SessionEngine::new(store, router, tools, permission, EventBus::default());
    if let Some(interaction) = interaction {
        engine = engine.with_interaction(interaction);
    }
    let engine = Arc::new(engine);
    let agent = Arc::new(AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir,
        reasoning: None,
    });
    (engine, agent)
}

async fn create_session(engine: &SessionEngine, agent: &AgentSpec) -> String {
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    session.to_string()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

async fn wait_for_data(app: axum::Router, uri: &str) -> Value {
    for _ in 0..100 {
        let resp = request(app.clone(), "GET", uri, None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        if body["data"].as_array().is_some_and(|data| !data.is_empty()) {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("pending request did not appear");
}

async fn read_sse_json(stream: &mut axum::body::BodyDataStream) -> Value {
    let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("event")
        .expect("body chunk")
        .expect("valid chunk");
    let frame = String::from_utf8(chunk.to_vec()).unwrap();
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("data line");
    serde_json::from_str(data).unwrap()
}

#[tokio::test]
async fn compat_permission_request_lists_and_replies_by_session() {
    let (permission, permission_rx) = PermissionPlane::new(PermissionRules::default());
    let scoped_permission = permission.clone();
    let (engine, agent) = base_engine(permission, None, tempdir()).await;
    let session = create_session(&engine, &agent).await;
    let other_session = create_session(&engine, &agent).await;
    let app = router(AppState::new(engine, agent).with_permission_requests(permission_rx));

    let task_session = session.parse().unwrap();
    let task = tokio::spawn(async move {
        scoped_permission
            .for_session(task_session)
            .assert(Action::Bash, Resource::Command("pwd".to_string()))
            .await
    });

    let listed = wait_for_data(app.clone(), &format!("/api/session/{session}/permission")).await;
    let request_id = listed["data"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(listed["data"][0]["sessionID"], session);
    assert_eq!(listed["data"][0]["action"], "bash");
    assert_eq!(listed["data"][0]["resources"], json!(["pwd"]));

    let global = request(app.clone(), "GET", "/api/permission/request", None).await;
    assert_eq!(global.status(), StatusCode::OK);
    assert_eq!(body_json(global).await["data"][0]["id"], request_id);

    let wrong_session = request(
        app.clone(),
        "POST",
        &format!("/api/session/{other_session}/permission/{request_id}/reply"),
        Some(json!({"reply": "once"})),
    )
    .await;
    assert_eq!(wrong_session.status(), StatusCode::NOT_FOUND);

    let reply = request(
        app.clone(),
        "POST",
        &format!("/api/session/{session}/permission/{request_id}/reply"),
        Some(json!({"reply": "once"})),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::NO_CONTENT);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn compat_legacy_session_permission_responds_by_session() {
    let (permission, permission_rx) = PermissionPlane::new(PermissionRules::default());
    let scoped_permission = permission.clone();
    let (engine, agent) = base_engine(permission, None, tempdir()).await;
    let session = create_session(&engine, &agent).await;
    let other_session = create_session(&engine, &agent).await;
    let app = router(AppState::new(engine, agent).with_permission_requests(permission_rx));

    let task_session = session.parse().unwrap();
    let task = tokio::spawn(async move {
        scoped_permission
            .for_session(task_session)
            .assert(Action::Bash, Resource::Command("pwd".to_string()))
            .await
    });

    let listed = wait_for_data(app.clone(), &format!("/api/session/{session}/permission")).await;
    let request_id = listed["data"][0]["id"].as_str().unwrap().to_string();

    let wrong_session = request(
        app.clone(),
        "POST",
        &format!("/session/{other_session}/permissions/{request_id}"),
        Some(json!({"response": "always"})),
    )
    .await;
    let wrong_status = wrong_session.status();
    let wrong_bytes = wrong_session
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let wrong_body = serde_json::from_slice::<Value>(&wrong_bytes).unwrap_or(Value::Null);
    assert_eq!(wrong_status, StatusCode::NOT_FOUND);
    assert_eq!(
        wrong_body,
        json!({
            "_tag": "PermissionNotFoundError",
            "requestID": request_id,
            "message": format!("Permission request not found: {request_id}"),
        })
    );

    let reply = request(
        app,
        "POST",
        &format!("/session/{session}/permissions/{request_id}"),
        Some(json!({"response": "always"})),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(body_json(reply).await, json!(true));
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn compat_legacy_permission_missing_session_returns_not_found() {
    let (permission, permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (engine, agent) = base_engine(permission, None, tempdir()).await;
    let app = router(AppState::new(engine, agent).with_permission_requests(permission_rx));
    let missing = SessionId::new().to_string();

    let resp = request(
        app,
        "POST",
        &format!("/session/{missing}/permissions/per_missing"),
        Some(json!({"response": "always"})),
    )
    .await;
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({
            "name": "NotFoundError",
            "data": { "message": format!("Session not found: {missing}") },
        })
    );
}

#[tokio::test]
async fn compat_question_request_lists_replies_and_rejects() {
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (interaction, question_rx) = InteractionPlane::new();
    let scoped_interaction = interaction.clone();
    let (engine, agent) = base_engine(permission, Some(interaction), tempdir()).await;
    let session = create_session(&engine, &agent).await;
    let app = router(AppState::new(engine, agent).with_question_requests(question_rx));

    let task_session = session.parse().unwrap();
    let reply_interaction = scoped_interaction.clone();
    let task = tokio::spawn(async move {
        reply_interaction
            .for_session(task_session)
            .ask(
                "Continue?".to_string(),
                QuestionKind::Select {
                    options: vec!["yes".to_string(), "no".to_string()],
                    allow_custom: false,
                },
            )
            .await
    });

    let listed = wait_for_data(app.clone(), &format!("/api/session/{session}/question")).await;
    let request_id = listed["data"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(listed["data"][0]["sessionID"], session);
    assert_eq!(listed["data"][0]["questions"][0]["question"], "Continue?");
    assert_eq!(
        listed["data"][0]["questions"][0]["options"][0]["label"],
        "yes"
    );

    let reply = request(
        app.clone(),
        "POST",
        &format!("/api/session/{session}/question/{request_id}/reply"),
        Some(json!({"answers": [["yes"]]})),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::NO_CONTENT);
    assert_eq!(task.await.unwrap().unwrap(), QuestionAnswer::Selected(0));

    let reject_task_session = session.parse().unwrap();
    let reject_interaction = scoped_interaction.clone();
    let reject_task = tokio::spawn(async move {
        reject_interaction
            .for_session(reject_task_session)
            .ask(
                "Name?".to_string(),
                QuestionKind::FreeText {
                    default: Some("Ada".to_string()),
                },
            )
            .await
    });
    let listed = wait_for_data(app.clone(), &format!("/api/session/{session}/question")).await;
    let reject_id = listed["data"][0]["id"].as_str().unwrap().to_string();
    let reject = request(
        app,
        "POST",
        &format!("/api/session/{session}/question/{reject_id}/reject"),
        None,
    )
    .await;
    assert_eq!(reject.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        reject_task.await.unwrap().unwrap(),
        QuestionAnswer::Cancelled
    );
}

#[tokio::test]
async fn compat_global_event_streams_question_lifecycle_once() {
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (interaction, question_rx) = InteractionPlane::new();
    let (engine, agent) = base_engine(permission, Some(interaction.clone()), tempdir()).await;
    let session = create_session(&engine, &agent).await;
    let other_session = create_session(&engine, &agent).await;
    let app = router(AppState::new(engine, agent).with_question_requests(question_rx));

    let response = request(app.clone(), "GET", "/global/event", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    assert_eq!(
        read_sse_json(&mut stream).await["payload"]["type"],
        "server.connected"
    );

    let ask_session = session.parse().unwrap();
    let ask = tokio::spawn({
        let interaction = interaction.clone();
        async move {
            interaction
                .for_session(ask_session)
                .ask(
                    "Continue?".to_string(),
                    QuestionKind::Select {
                        options: vec!["yes".to_string(), "no".to_string()],
                        allow_custom: false,
                    },
                )
                .await
        }
    });
    let asked = read_sse_json(&mut stream).await;
    assert_eq!(asked["payload"]["type"], "question.asked");
    let request_id = asked["payload"]["properties"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(request_id.starts_with("q_"));
    assert_eq!(asked["payload"]["properties"]["sessionID"], session);
    assert_eq!(
        asked["payload"]["properties"]["questions"][0]["options"],
        json!([
            {"label": "yes", "description": ""},
            {"label": "no", "description": ""}
        ])
    );

    let reply = request(
        app.clone(),
        "POST",
        &format!("/api/session/{session}/question/{request_id}/reply"),
        Some(json!({"answers": [["yes"]]})),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::NO_CONTENT);
    assert_eq!(ask.await.unwrap().unwrap(), QuestionAnswer::Selected(0));
    let replied = read_sse_json(&mut stream).await;
    assert_eq!(replied["payload"]["type"], "question.replied");
    assert_eq!(
        replied["payload"]["properties"],
        json!({"sessionID": session, "requestID": request_id, "answers": [["yes"]]})
    );

    let duplicate = request(
        app.clone(),
        "POST",
        &format!("/api/session/{session}/question/{request_id}/reply"),
        Some(json!({"answers": [["yes"]]})),
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::NOT_FOUND);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err(),
        "duplicate reply emitted an event"
    );

    let reject_session = session.parse().unwrap();
    let reject = tokio::spawn(async move {
        interaction
            .for_session(reject_session)
            .ask(
                "Name?".to_string(),
                QuestionKind::FreeText { default: None },
            )
            .await
    });
    let asked = read_sse_json(&mut stream).await;
    assert_eq!(asked["payload"]["type"], "question.asked");
    let reject_id = asked["payload"]["properties"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let wrong_session = request(
        app.clone(),
        "POST",
        &format!("/api/session/{other_session}/question/{reject_id}/reject"),
        None,
    )
    .await;
    assert_eq!(wrong_session.status(), StatusCode::NOT_FOUND);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err(),
        "wrong-session reject emitted an event"
    );

    let response = request(
        app.clone(),
        "POST",
        &format!("/api/session/{session}/question/{reject_id}/reject"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(reject.await.unwrap().unwrap(), QuestionAnswer::Cancelled);
    let rejected = read_sse_json(&mut stream).await;
    assert_eq!(rejected["payload"]["type"], "question.rejected");
    assert_eq!(
        rejected["payload"]["properties"],
        json!({"sessionID": session, "requestID": reject_id})
    );

    let duplicate = request(
        app,
        "POST",
        &format!("/api/session/{session}/question/{reject_id}/reject"),
        None,
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::NOT_FOUND);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err(),
        "duplicate reject emitted an event"
    );
}

#[tokio::test]
async fn compat_permission_always_resolves_matching_session_requests() {
    let (permission, permission_rx) = PermissionPlane::new(PermissionRules::default());
    let first_permission = permission.clone();
    let second_permission = permission.clone();
    let (engine, agent) = base_engine(permission, None, tempdir()).await;
    let session = create_session(&engine, &agent).await;
    let app = router(AppState::new(engine, agent).with_permission_requests(permission_rx));

    let first_session = session.parse().unwrap();
    let first = tokio::spawn(async move {
        first_permission
            .for_session(first_session)
            .assert(Action::Bash, Resource::Command("pwd".to_string()))
            .await
    });
    let second_session = session.parse().unwrap();
    let second = tokio::spawn(async move {
        second_permission
            .for_session(second_session)
            .assert(Action::Bash, Resource::Command("ls".to_string()))
            .await
    });

    let listed = wait_for_data(app.clone(), &format!("/api/session/{session}/permission")).await;
    let requests = listed["data"].as_array().unwrap();
    assert_eq!(requests.len(), 2);
    let request_id = requests[0]["id"].as_str().unwrap();

    let reply = request(
        app,
        "POST",
        &format!("/api/session/{session}/permission/{request_id}/reply"),
        Some(json!({"reply": "always"})),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::NO_CONTENT);

    tokio::time::timeout(std::time::Duration::from_secs(1), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
