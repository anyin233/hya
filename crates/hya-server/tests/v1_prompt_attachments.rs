//! Image attachments on `/v1` prompt turns: the bytes reach the provider as
//! an image part, the transcript lists an `AttachmentPart` (without the
//! bytes), the stream carries `partsAdded`, forks keep the images, and bad
//! attachments or a model without image input are refused before admission.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use base64::Engine as _;
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, Message, ModelRef, Part, Role};
use hya_provider::ProviderRouter;
use hya_provider::{Capabilities, CompletionRequest, EventStream, Provider, ProviderError};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A PNG signature plus a few bytes: enough for the server's type check.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRpixel";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIFpixel";

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Records every request; replies with a finished assistant message.
struct RecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    image_input: Option<bool>,
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            image_input: self.image_input,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )])))
    }
}

struct Fixture {
    state: AppState,
    app: axum::Router,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn fixture(label: &str, image_input: Option<bool>) -> Fixture {
    let dir = support::tempdir(label).canonicalize().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (perm, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider {
            requests: Arc::clone(&requests),
            image_input,
        }))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    ));
    let state = AppState::new(
        engine,
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: dir.clone(),
            reasoning: None,
        }),
    );
    Fixture {
        app: router(state.clone()),
        state,
        requests,
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

    async fn create_turn(&self, session: &str, prompt: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            Method::POST,
            &format!("/v1/sessions/{session}/turns"),
            json!({ "prompt": prompt }),
        )
        .await
    }

    async fn wait_idle(&self, session: &str, turn: &str) {
        let (status, waited) = call(
            &self.app,
            Method::POST,
            &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{waited}");
        assert_eq!(waited["state"], json!("TURN_STATE_FINISHED"), "{waited}");
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

    async fn prompt(&self, session: &str, prompt: Value) -> String {
        let (status, created) = self.create_turn(session, prompt).await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let turn = created["turn"]["id"].as_str().unwrap().to_owned();
        self.wait_idle(session, &turn).await;
        turn
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

    /// Image parts of the user messages in the `index`-th provider request.
    fn media(&self, index: usize) -> Vec<(String, String, Option<String>)> {
        let requests = self.requests.lock().unwrap();
        requests[index]
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::User { parts, .. } => Some(parts),
                _ => None,
            })
            .flatten()
            .filter_map(|part| match part {
                Part::Media {
                    media_type,
                    data,
                    filename,
                    ..
                } => Some((media_type.clone(), data.clone(), filename.clone())),
                _ => None,
            })
            .collect()
    }
}

fn attachment_parts(message: &Value) -> Vec<Value> {
    message["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|part| part.get("attachment").is_some())
        .cloned()
        .collect()
}

#[tokio::test]
async fn png_attachment_reaches_the_provider_and_lists_as_an_attachment_part() {
    let fx = fixture("attach-png", None).await;
    let session = fx.session().await;
    let turn = fx
        .prompt(
            &session,
            json!({
                "text": "what is in this image?",
                "attachments": [
                    {"name": "pixel.png", "mime": "image/png", "data": b64(PNG), "path": "/tmp/pixel.png"}
                ]
            }),
        )
        .await;

    assert_eq!(
        fx.media(0),
        vec![(
            "image/png".to_string(),
            format!("data:image/png;base64,{}", b64(PNG)),
            Some("pixel.png".to_string()),
        )]
    );

    let messages = fx.messages(&session).await;
    let user = messages
        .iter()
        .find(|message| message["id"] == json!(turn))
        .expect("user message listed");
    assert_eq!(
        user["parts"][0]["text"]["text"],
        json!("what is in this image?")
    );
    let attachments = attachment_parts(user);
    assert_eq!(attachments.len(), 1, "{user}");
    let attachment = &attachments[0];
    assert!(attachment["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(attachment["attachment"]["name"], json!("pixel.png"));
    assert_eq!(attachment["attachment"]["mime"], json!("image/png"));
    assert_eq!(attachment["attachment"]["path"], json!("/tmp/pixel.png"));
    assert_eq!(
        attachment["attachment"]["size"],
        json!(PNG.len().to_string())
    );
    // Listings never carry the bytes.
    assert!(
        attachment["attachment"].get("data").is_none(),
        "{attachment}"
    );

    // The durable stream carries the attachments as one `partsAdded` frame.
    let (status, events) = call(
        &fx.app,
        Method::GET,
        &format!("/v1/sessions/{session}/events"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{events}");
    let added = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|event| event.get("partsAdded"))
        .expect("partsAdded event")
        .clone();
    assert_eq!(added["message"], json!(turn));
    assert_eq!(added["parts"][0]["attachment"]["name"], json!("pixel.png"));
    assert_eq!(added["parts"][0]["id"], attachment["id"]);
}

#[tokio::test]
async fn mime_is_sniffed_when_omitted_and_later_turns_resend_the_image() {
    let fx = fixture("attach-sniff", None).await;
    let session = fx.session().await;
    fx.prompt(
        &session,
        json!({"text": "look", "attachments": [{"name": "photo.jpg", "data": b64(JPEG)}]}),
    )
    .await;
    fx.prompt(&session, json!({"text": "and again?"})).await;
    let expected = vec![(
        "image/jpeg".to_string(),
        format!("data:image/jpeg;base64,{}", b64(JPEG)),
        Some("photo.jpg".to_string()),
    )];
    assert_eq!(fx.media(0), expected);
    // The history of the second turn still holds the image.
    assert_eq!(fx.media(1), expected);
}

#[tokio::test]
async fn a_five_mebibyte_image_is_accepted() {
    let fx = fixture("attach-large", None).await;
    let session = fx.session().await;
    let mut big = PNG.to_vec();
    big.resize(5 * 1024 * 1024, 7);
    fx.prompt(
        &session,
        json!({"text": "big", "attachments": [{"name": "big.png", "mime": "image/png", "data": b64(&big)}]}),
    )
    .await;
    let media = fx.media(0);
    assert_eq!(media.len(), 1);
    assert_eq!(
        media[0].1.len(),
        "data:image/png;base64,".len() + b64(&big).len()
    );
}

#[tokio::test]
async fn invalid_attachments_are_refused_before_admission() {
    let fx = fixture("attach-invalid", None).await;
    let session = fx.session().await;
    let mut eleven = PNG.to_vec();
    eleven.resize(11 * 1024 * 1024, 1);
    let mut seven = PNG.to_vec();
    seven.resize(7 * 1024 * 1024, 1);
    let cases = [
        json!({"text": "t", "attachments": [{"name": "a.txt", "mime": "text/plain", "data": b64(b"hello")}]}),
        json!({"text": "t", "attachments": [{"name": "a.svg", "mime": "image/svg+xml", "data": b64(b"<svg/>")}]}),
        // Declared type does not match the bytes.
        json!({"text": "t", "attachments": [{"name": "a.png", "mime": "image/png", "data": b64(JPEG)}]}),
        // Unrecognizable bytes with no declared type.
        json!({"text": "t", "attachments": [{"name": "a.bin", "data": b64(b"not an image")}]}),
        json!({"text": "t", "attachments": [{"name": "empty.png", "mime": "image/png", "data": ""}]}),
        json!({"text": "t", "attachments": [{"name": "", "mime": "image/png", "data": b64(PNG)}]}),
        json!({"text": "t", "attachments": [{"name": "huge.png", "mime": "image/png", "data": b64(&eleven)}]}),
        json!({"text": "t", "attachments": [
            {"name": "1.png", "mime": "image/png", "data": b64(&seven)},
            {"name": "2.png", "mime": "image/png", "data": b64(&seven)},
            {"name": "3.png", "mime": "image/png", "data": b64(&seven)}
        ]}),
    ];
    for case in cases {
        let (status, body) = fx.create_turn(&session, case.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case:.200}: {body}");
        assert_eq!(body["error"]["code"], json!("invalid_argument"), "{body}");
    }
    // Nothing was admitted and the session is free.
    assert!(fx.messages(&session).await.is_empty());
    fx.prompt(&session, json!({"text": "still usable"})).await;
}

#[tokio::test]
async fn a_model_without_image_input_refuses_attachments() {
    let fx = fixture("attach-no-vision", Some(false)).await;
    let session = fx.session().await;
    let (status, body) = fx
        .create_turn(
            &session,
            json!({"text": "t", "attachments": [{"name": "a.png", "mime": "image/png", "data": b64(PNG)}]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("invalid_argument"));
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("fake") && message.contains("image"),
        "{message}"
    );
    assert!(fx.messages(&session).await.is_empty());
    assert!(fx.requests.lock().unwrap().is_empty());
    // Text-only prompts still run on that model.
    fx.prompt(&session, json!({"text": "plain"})).await;
}

#[tokio::test]
async fn a_fork_keeps_the_attached_images() {
    let fx = fixture("attach-fork", None).await;
    let session = fx.session().await;
    fx.prompt(
        &session,
        json!({"text": "see", "attachments": [{"name": "pixel.png", "mime": "image/png", "data": b64(PNG)}]}),
    )
    .await;
    let (status, forked) = call(
        &fx.app,
        Method::POST,
        &format!("/v1/sessions/{session}/fork"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    let fork = forked["session"]["id"].as_str().unwrap().to_owned();
    let messages = fx.messages(&fork).await;
    let user = messages
        .iter()
        .find(|message| message["role"] == json!("ROLE_USER"))
        .unwrap();
    assert_eq!(attachment_parts(user).len(), 1, "{user}");
    fx.prompt(&fork, json!({"text": "in the fork"})).await;
    assert_eq!(fx.media(1), fx.media(0));
}

#[tokio::test]
async fn grpc_and_http_attachment_turns_match() {
    use tonic::transport::Server;
    let fx = fixture("attach-grpc", None).await;
    let grpc = V1Grpc::new(fx.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = Server::builder()
        .add_service(
            pb::turn_server::TurnServer::new(grpc.clone())
                .max_decoding_message_size(hya_server::MAX_TURN_GRPC_MESSAGE_BYTES),
        )
        .add_service(pb::messages_server::MessagesServer::new(grpc.clone()))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut turns = pb::turn_client::TurnClient::new(channel.clone())
        .max_encoding_message_size(hya_server::MAX_TURN_GRPC_MESSAGE_BYTES);
    let mut messages = pb::messages_client::MessagesClient::new(channel);

    let grpc_session = fx.session().await;
    let http_session = fx.session().await;
    // Over 4 MiB: gRPC must accept what HTTP accepts.
    let mut image = PNG.to_vec();
    image.resize(5 * 1024 * 1024, 3);
    let created = turns
        .create_turn(tonic::Request::new(pb::CreateTurnRequest {
            session: grpc_session.clone(),
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: "see".into(),
                attachments: vec![pb::PromptAttachment {
                    name: "pixel.png".into(),
                    mime: "image/png".into(),
                    data: image.clone(),
                    path: String::new(),
                }],
            })),
        }))
        .await
        .unwrap()
        .into_inner();
    let grpc_turn = created.turn.unwrap().id;
    fx.wait_idle(&grpc_session, &grpc_turn).await;
    fx.prompt(
        &http_session,
        json!({"text": "see", "attachments": [{"name": "pixel.png", "mime": "image/png", "data": b64(&image)}]}),
    )
    .await;
    assert_eq!(fx.media(0), fx.media(1));

    let grpc_list = messages
        .list_messages(tonic::Request::new(pb::ListMessagesRequest {
            session: grpc_session.clone(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    let grpc_user = serde_json::to_value(&grpc_list.messages[0]).unwrap();
    let http_user = fx.messages(&http_session).await[0].clone();
    assert_eq!(
        attachment_parts(&grpc_user)[0]["attachment"],
        attachment_parts(&http_user)[0]["attachment"]
    );

    let refused = turns
        .create_turn(tonic::Request::new(pb::CreateTurnRequest {
            session: grpc_session,
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: "t".into(),
                attachments: vec![pb::PromptAttachment {
                    name: "a.txt".into(),
                    mime: "text/plain".into(),
                    data: b"hello".to_vec(),
                    path: String::new(),
                }],
            })),
        }))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument);
}
