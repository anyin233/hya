//! v1 interaction stream frames: pending permission/question requests and
//! their resolutions arrive on the live event streams, and an "always"
//! permission reply persists a saved rule.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::PermissionRequestId;
use hya_proto::{AgentName, ModelRef, QuestionRequestId, SessionId};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::interaction::{QuestionKind, QuestionRequest};
use hya_tool::permission::{Action, AskRequest, RememberScope, Resource};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tower::ServiceExt;

async fn base_state() -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
}

async fn respond(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
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

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
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

/// Collect SSE frames until `predicate` matches or the deadline passes.
async fn frames_until(
    app: &axum::Router,
    uri: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Vec<Value> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let chunk = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
        let Ok(Some(Ok(bytes))) = chunk else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            if let Ok(frame) = serde_json::from_str::<Value>(data.trim()) {
                if predicate(&frame) {
                    return {
                        seen.push(frame);
                        seen
                    };
                }
                seen.push(frame);
            }
        }
    }
    seen
}

#[tokio::test]
async fn permission_asks_and_resolutions_stream_as_interaction_frames() {
    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskRequest>();
    let state = base_state().await.with_permission_requests(ask_rx);
    let app = router(state);

    // Open the global stream first, then push an ask through the bridge.
    let stream_app = app.clone();
    let collector = tokio::spawn(async move {
        frames_until(&stream_app, "/v1/events/stream", |frame| {
            frame.get("event").is_some_and(|event| {
                event
                    .get("permissionRequested")
                    .is_some_and(|request| !request["request"].as_str().unwrap_or("").is_empty())
            })
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let request_id = PermissionRequestId::new();
    let id = request_id.to_string();
    ask_tx
        .send(AskRequest {
            id: request_id,
            session: Some(SessionId::new()),
            message_id: None,
            call_id: None,
            action: Action::Bash,
            resource: Resource::Command("printf hi > file".to_string()),
            remember: RememberScope::LegacyAction,
            reply: reply_tx,
        })
        .expect("ask should flow through the bridge");

    let frames = collector.await.expect("collector task");
    let asked = frames
        .iter()
        .find_map(|frame| frame["event"]["permissionRequested"].as_object().cloned())
        .expect("a permissionRequested frame must arrive");
    assert_eq!(asked["request"].as_str().unwrap(), id);
    let interaction = &asked["interaction"];
    assert_eq!(interaction["type"], json!("INTERACTION_TYPE_PERMISSION"));
    assert!(
        interaction["title"]
            .as_str()
            .is_some_and(|title| title.contains("bash")),
        "title should carry the action: {interaction}"
    );

    // Subscribe for the resolution before replying (broadcast is
    // subscriber-live only), then answer "always".
    let resolved_app = app.clone();
    let resolved_id = id.clone();
    let resolved = tokio::spawn(async move {
        frames_until(&resolved_app, "/v1/events/stream", |frame| {
            frame["event"]
                .get("interactionResolved")
                .is_some_and(Value::is_object)
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, body) = respond(
        &app,
        &format!("/v1/interactions/{id}/respond"),
        json!({"permission": {"allowed": true, "persist": true}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], json!(true));
    let _ = reply_rx.await;
    let resolved = resolved.await.expect("resolved collector");
    assert!(
        resolved
            .iter()
            .any(|frame| frame["event"]["interactionResolved"]["request"] == json!(resolved_id)),
        "an interactionResolved frame must fire for {resolved_id}: {resolved:?}"
    );

    let (status, rules) = get_json(&app, "/v1/permissions/rules").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        rules["rules"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"].as_str().is_some())),
        "the always reply must persist a saved rule: {rules}"
    );
}

#[tokio::test]
async fn question_asks_stream_and_reject_resolves() {
    let (question_tx, question_rx) = mpsc::unbounded_channel::<QuestionRequest>();
    let state = base_state().await.with_question_requests(question_rx);
    let app = router(state);

    let stream_app = app.clone();
    let collector = tokio::spawn(async move {
        frames_until(&stream_app, "/v1/events/stream", |frame| {
            frame["event"]
                .get("questionRequested")
                .is_some_and(Value::is_object)
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
    let id = QuestionRequestId::new();
    let id_str = id.to_string();
    question_tx
        .send(QuestionRequest {
            id,
            session: Some(SessionId::new()),
            prompt: "Continue?".to_string(),
            info: hya_tool::interaction::QuestionInfo {
                question: "Continue?".to_string(),
                header: "Confirm".to_string(),
                options: vec![],
                multiple: false,
                custom: None,
            },
            kind: QuestionKind::FreeText { default: None },
            questions: vec![hya_tool::interaction::QuestionPrompt::new(
                hya_tool::interaction::QuestionInfo {
                    question: "Continue?".to_string(),
                    header: "Confirm".to_string(),
                    options: vec![],
                    multiple: false,
                    custom: None,
                },
                QuestionKind::FreeText { default: None },
            )],
            reply: hya_tool::QuestionReply::Many(reply_tx),
        })
        .expect("question should flow through the bridge");

    let frames = collector.await.expect("collector task");
    let asked = frames
        .iter()
        .find_map(|frame| frame["event"]["questionRequested"].as_object().cloned())
        .expect("a questionRequested frame must arrive");
    assert_eq!(asked["request"].as_str().unwrap(), id_str);
    assert_eq!(
        asked["interaction"]["type"],
        json!("INTERACTION_TYPE_QUESTION")
    );
    assert_eq!(asked["interaction"]["title"], json!("Continue?"));

    let resolved_app = app.clone();
    let resolved_id = id_str.clone();
    let resolved = tokio::spawn(async move {
        frames_until(&resolved_app, "/v1/events/stream", |frame| {
            frame["event"]
                .get("interactionResolved")
                .is_some_and(Value::is_object)
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, body) = respond(
        &app,
        &format!("/v1/interactions/{id_str}/respond"),
        json!({"question": {"rejected": true}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let resolved = resolved.await.expect("resolved collector");
    assert!(
        resolved
            .iter()
            .any(|frame| frame["event"]["interactionResolved"]["request"] == json!(resolved_id)),
        "rejection must resolve: {resolved:?}"
    );
}

/// `GET /v1/interactions` without a `type` filter lists every pending
/// interaction (unspecified means all types), over HTTP and gRPC.
#[tokio::test]
async fn list_interactions_without_a_type_filter_returns_every_type() {
    use hya_api::v1 as pb;

    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskRequest>();
    let state = base_state().await.with_permission_requests(ask_rx);
    let app = router(state.clone());
    let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
    let request_id = PermissionRequestId::new();
    ask_tx
        .send(AskRequest {
            id: request_id,
            session: Some(SessionId::new()),
            message_id: None,
            call_id: None,
            action: Action::Bash,
            resource: Resource::Command("ls".to_string()),
            remember: RememberScope::LegacyAction,
            reply: reply_tx,
        })
        .expect("ask should flow through the bridge");
    let id = request_id.to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (status, typed) =
            get_json(&app, "/v1/interactions?type=INTERACTION_TYPE_PERMISSION").await;
        assert_eq!(status, StatusCode::OK, "{typed}");
        if typed["interactions"][0]["id"] == json!(id) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "ask never arrived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (status, body) = get_json(&app, "/v1/interactions").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["interactions"][0]["id"], json!(id), "{body}");
    assert_eq!(
        body["interactions"][0]["type"],
        json!("INTERACTION_TYPE_PERMISSION")
    );
    let (status, body) = get_json(&app, "/v1/interactions?type=INTERACTION_TYPE_QUESTION").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("interactions").is_none(), "{body}");

    // gRPC: the unspecified type (0) is unfiltered too.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = tonic::transport::Server::builder()
        .add_service(pb::interactions_server::InteractionsServer::new(
            hya_server::V1Grpc::new(state),
        ))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let listed = pb::interactions_client::InteractionsClient::new(channel)
        .list_interactions(pb::ListInteractionsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.interactions.len(), 1, "{listed:?}");
    assert_eq!(listed.interactions[0].id, id);
}

fn select_question(
    id: QuestionRequestId,
    session: SessionId,
) -> (
    QuestionRequest,
    tokio::sync::oneshot::Receiver<Vec<hya_tool::interaction::QuestionAnswer>>,
) {
    let info = hya_tool::interaction::QuestionInfo {
        question: "Which branch?".to_string(),
        header: "Branch".to_string(),
        options: vec![
            hya_tool::interaction::QuestionOption {
                label: "main".to_string(),
                description: "the default branch".to_string(),
            },
            hya_tool::interaction::QuestionOption {
                label: "dev".to_string(),
                description: String::new(),
            },
        ],
        multiple: true,
        custom: Some(false),
    };
    let kind = QuestionKind::Select {
        options: vec!["main".to_string(), "dev".to_string()],
        allow_custom: false,
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    (
        QuestionRequest {
            id,
            session: Some(session),
            prompt: "Which branch?".to_string(),
            info: info.clone(),
            kind: kind.clone(),
            questions: vec![hya_tool::interaction::QuestionPrompt::new(info, kind)],
            reply: hya_tool::QuestionReply::Many(reply_tx),
        },
        reply_rx,
    )
}

async fn grpc_channel(state: AppState) -> tonic::transport::Channel {
    use hya_api::v1 as pb;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let grpc = hya_server::V1Grpc::new(state);
    let serve = tonic::transport::Server::builder()
        .add_service(pb::interactions_server::InteractionsServer::new(
            grpc.clone(),
        ))
        .add_service(pb::events_server::EventsServer::new(grpc))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

/// A pending question lists with the same interaction the live
/// `questionRequested` frame carried: options, header as `detail`, and the
/// full questions as `payload` — over HTTP and gRPC.
#[tokio::test]
async fn question_listing_matches_the_question_requested_frame() {
    use hya_api::v1 as pb;

    let (question_tx, question_rx) = mpsc::unbounded_channel::<QuestionRequest>();
    let state = base_state().await.with_question_requests(question_rx);
    let app = router(state.clone());
    let stream_app = app.clone();
    let collector = tokio::spawn(async move {
        frames_until(&stream_app, "/v1/events/stream", |frame| {
            frame["event"]
                .get("questionRequested")
                .is_some_and(Value::is_object)
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let id = QuestionRequestId::new();
    let session = SessionId::new();
    let (request, _reply_rx) = select_question(id, session);
    question_tx.send(request).unwrap();
    let frames = collector.await.unwrap();
    let streamed = frames
        .iter()
        .find_map(|frame| frame["event"]["questionRequested"]["interaction"].as_object())
        .cloned()
        .map(Value::Object)
        .expect("a questionRequested frame must arrive");
    assert_eq!(streamed["options"], json!(["main", "dev"]), "{streamed}");
    assert_eq!(streamed["detail"], json!("Branch"));
    assert_eq!(
        streamed["payload"],
        json!({"questions": [{
            "question": "Which branch?",
            "header": "Branch",
            "options": [
                {"label": "main", "description": "the default branch"},
                {"label": "dev", "description": ""},
            ],
            "multiple": true,
            "custom": false,
        }]}),
        "{streamed}"
    );

    let (status, body) = get_json(&app, "/v1/interactions?type=INTERACTION_TYPE_QUESTION").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["interactions"][0], streamed, "{body}");

    let listed = pb::interactions_client::InteractionsClient::new(grpc_channel(state).await)
        .list_interactions(pb::ListInteractionsRequest::default())
        .await
        .unwrap()
        .into_inner();
    let question = &listed.interactions[0];
    assert_eq!(question.id, id.to_string());
    assert_eq!(question.session, session.to_string());
    assert_eq!(
        question.options,
        vec!["main".to_string(), "dev".to_string()]
    );
    assert_eq!(question.detail, "Branch");
    assert!(question.payload.is_some(), "{question:?}");
}

async fn session_tree(state: &AppState) -> (SessionId, SessionId, SessionId) {
    let create = |parent| hya_core::CreateSession {
        parent,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: std::env::temp_dir().to_string_lossy().into_owned(),
        project: None,
        kind: hya_proto::SessionKind::Project,
    };
    let root = state.engine.create(create(None)).await.unwrap();
    let child = state.engine.create(create(Some(root))).await.unwrap();
    let grandchild = state.engine.create(create(Some(child))).await.unwrap();
    (root, child, grandchild)
}

/// With `includeDescendants=true`, a subagent's ask and its resolution arrive
/// on an ancestor's session stream tagged with the asking session; without
/// the opt-in the ancestor's stream stays per session.
#[tokio::test]
async fn descendant_interactions_reach_an_ancestor_stream_on_opt_in() {
    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskRequest>();
    let state = base_state().await.with_permission_requests(ask_rx);
    let (root, _child, grandchild) = session_tree(&state).await;
    let app = router(state);

    let opted = app.clone();
    let opted_uri = format!("/v1/sessions/{root}/events/stream?includeDescendants=true");
    let with_descendants = tokio::spawn(async move {
        frames_until(&opted, &opted_uri, |frame| {
            frame["event"]
                .get("interactionResolved")
                .is_some_and(Value::is_object)
        })
        .await
    });
    let plain = app.clone();
    let plain_uri = format!("/v1/sessions/{root}/events/stream");
    let without = tokio::spawn(async move {
        frames_until(&plain, &plain_uri, |frame| {
            frame["event"].get("permissionRequested").is_some()
                || frame["event"].get("interactionResolved").is_some()
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let request_id = PermissionRequestId::new();
    let id = request_id.to_string();
    ask_tx
        .send(AskRequest {
            id: request_id,
            session: Some(grandchild),
            message_id: None,
            call_id: None,
            action: Action::Bash,
            resource: Resource::Command("ls".to_string()),
            remember: RememberScope::LegacyAction,
            reply: reply_tx,
        })
        .unwrap();
    // Answer once the ask is pending.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (_, listed) = get_json(&app, "/v1/interactions").await;
        if listed["interactions"][0]["id"] == json!(id) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "ask never arrived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, body) = respond(
        &app,
        &format!("/v1/interactions/{id}/respond"),
        json!({"permission": {"allowed": true}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let _ = reply_rx.await;

    let frames = with_descendants.await.unwrap();
    let asked = frames
        .iter()
        .find(|frame| frame["event"]["permissionRequested"]["request"] == json!(id))
        .unwrap_or_else(|| panic!("the descendant ask must arrive: {frames:#?}"));
    assert_eq!(asked["event"]["session"], json!(grandchild.to_string()));
    assert_eq!(
        asked["event"]["permissionRequested"]["interaction"]["session"],
        json!(grandchild.to_string())
    );
    let resolved = frames
        .iter()
        .find(|frame| frame["event"]["interactionResolved"]["request"] == json!(id))
        .unwrap_or_else(|| panic!("the resolution must arrive: {frames:#?}"));
    assert_eq!(resolved["event"]["session"], json!(grandchild.to_string()));

    let plain_frames = without.await.unwrap();
    assert!(
        plain_frames.iter().all(|frame| {
            frame["event"].get("permissionRequested").is_none()
                && frame["event"].get("interactionResolved").is_none()
        }),
        "without the opt-in the root stream stays per session: {plain_frames:#?}"
    );
}

/// gRPC `StreamSessionEvents { include_descendants: true }` delivers a child
/// session's question to its parent's stream.
#[tokio::test]
async fn grpc_session_stream_delivers_descendant_questions_on_opt_in() {
    use hya_api::v1 as pb;

    let (question_tx, question_rx) = mpsc::unbounded_channel::<QuestionRequest>();
    let state = base_state().await.with_question_requests(question_rx);
    let (root, child, _grandchild) = session_tree(&state).await;
    let channel = grpc_channel(state).await;
    let mut stream = pb::events_client::EventsClient::new(channel)
        .stream_session_events(pb::StreamSessionEventsRequest {
            session: root.to_string(),
            include_descendants: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let id = QuestionRequestId::new();
    let (request, _reply_rx) = select_question(id, child);
    question_tx.send(request).unwrap();
    let asked = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = stream.next().await {
            if let Some(pb::stream_frame::Frame::Event(event)) = frame.unwrap().frame
                && let Some(pb::stream_event::Payload::QuestionRequested(asked)) = event.payload
            {
                return (event.session, asked);
            }
        }
        panic!("stream ended");
    })
    .await
    .expect("the child's question must reach the parent stream");
    assert_eq!(asked.0, child.to_string());
    assert_eq!(asked.1.request, id.to_string());
    assert_eq!(
        asked.1.interaction.unwrap().options,
        vec!["main".to_string(), "dev".to_string()]
    );
}

/// `interactionsOnly` on the global stream drops every session's engine
/// events except root-session list frames (`sessionStarted` here), and keeps
/// the live interaction frames (and process notices such as
/// `catalogUpdated`), over HTTP and gRPC.
#[tokio::test]
async fn global_stream_interactions_only_skips_session_events() {
    use hya_api::v1 as pb;

    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskRequest>();
    let state = base_state().await.with_permission_requests(ask_rx);
    let app = router(state.clone());

    let is_ask = |frame: &Value| frame["event"]["permissionRequested"].is_object();
    let filtered_app = app.clone();
    let filtered = tokio::spawn(async move {
        frames_until(
            &filtered_app,
            "/v1/events/stream?interactionsOnly=true",
            is_ask,
        )
        .await
    });
    let unfiltered_app = app.clone();
    let unfiltered =
        tokio::spawn(
            async move { frames_until(&unfiltered_app, "/v1/events/stream", is_ask).await },
        );
    let mut grpc = pb::events_client::EventsClient::new(grpc_channel(state.clone()).await)
        .stream_global_events(pb::StreamGlobalEventsRequest {
            interactions_only: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A session event on the bus, a catalog notice, then an ask.
    let (status, _body) = respond(&app, "/v1/sessions", json!({"agent": "build", "model": "fake", "workdir": std::env::temp_dir().to_string_lossy()})).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(100)).await;
    state.notify_catalog_updated();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
    ask_tx
        .send(AskRequest {
            id: PermissionRequestId::new(),
            session: Some(SessionId::new()),
            message_id: None,
            call_id: None,
            action: Action::Bash,
            resource: Resource::Command("ls".to_string()),
            remember: RememberScope::LegacyAction,
            reply: reply_tx,
        })
        .unwrap();

    let unfiltered = unfiltered.await.unwrap();
    assert!(
        unfiltered
            .iter()
            .any(|frame| frame["event"]["sessionStarted"].is_object()),
        "the unfiltered stream carries session events: {unfiltered:#?}"
    );
    let filtered = filtered.await.unwrap();
    let kinds: Vec<String> = filtered
        .iter()
        .filter_map(|frame| frame["event"].as_object())
        .flat_map(|event| event.keys().cloned().collect::<Vec<_>>())
        // Creating the session also changed the Project list.
        .filter(|key| {
            !matches!(
                key.as_str(),
                "seq" | "session" | "timeRecorded" | "projectsUpdated"
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "sessionStarted".to_owned(),
            "catalogUpdated".to_owned(),
            "permissionRequested".to_owned()
        ],
        "{filtered:#?}"
    );

    let mut grpc_kinds = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = grpc.next().await {
            let Some(pb::stream_frame::Frame::Event(event)) = frame.unwrap().frame else {
                grpc_kinds.push("resync");
                continue;
            };
            match event.payload {
                Some(pb::stream_event::Payload::SessionStarted(_)) => {
                    grpc_kinds.push("sessionStarted");
                }
                Some(pb::stream_event::Payload::CatalogUpdated(_)) => {
                    grpc_kinds.push("catalogUpdated");
                }
                Some(pb::stream_event::Payload::ProjectsUpdated(_)) => {}
                Some(pb::stream_event::Payload::PermissionRequested(_)) => {
                    grpc_kinds.push("permissionRequested");
                    break;
                }
                _ => grpc_kinds.push("other"),
            }
        }
    })
    .await
    .expect("gRPC ask frame");
    assert_eq!(
        grpc_kinds,
        vec!["sessionStarted", "catalogUpdated", "permissionRequested"]
    );
}

/// A provider catalog change reaches v1 session streams too, as a live-only
/// `catalogUpdated` frame.
#[tokio::test]
async fn catalog_updated_reaches_session_streams() {
    let state = base_state().await;
    let app = router(state.clone());
    let (status, body) = respond(&app, "/v1/sessions", json!({"agent": "build", "model": "fake", "workdir": std::env::temp_dir().to_string_lossy()})).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let session = body["session"]["id"].as_str().unwrap().to_owned();
    let stream_app = app.clone();
    let collector = tokio::spawn(async move {
        frames_until(
            &stream_app,
            &format!("/v1/sessions/{session}/events/stream"),
            |frame| frame["event"]["catalogUpdated"].is_object(),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    state.notify_catalog_updated();
    let frames = collector.await.unwrap();
    let last = frames.last().expect("a frame");
    assert!(last["event"]["catalogUpdated"].is_object(), "{frames:#?}");
    assert!(last["event"].get("seq").is_none(), "live-only: {last}");
}
