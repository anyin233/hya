//! Integration tests for `hya-core`: prompt attachments.

#![allow(clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, Message, ModelRef, Part, Role};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, HttpProvider, Provider, ProviderError,
    ProviderKind, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-core-prompt-attachments-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct RecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
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
            },
        )])))
    }
}

#[tokio::test]
async fn compat_prompt_files_are_replayed_as_media_parts() {
    let dir = tempdir();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider {
        requests: requests.clone(),
    })));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let message = engine
        .admit_user_prompt(session, "inspect attached image".to_string())
        .await
        .unwrap();
    engine
        .record_user_prompt_context(
            session,
            message,
            vec![json!({
                "uri": "data:image/png;base64,aGVsbG8=",
                "mime": "image/png",
                "name": "pixel.png",
                "description": "tiny fixture",
            })],
            Vec::new(),
        )
        .await
        .unwrap();

    engine
        .run_turn(
            session,
            &AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new("fake"),
                system_prompt: "x".to_string(),
                workdir: dir,
                reasoning: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let requests = requests.lock().unwrap();
    let Message::User { parts, .. } = &requests[0].messages[0] else {
        panic!("expected user message");
    };
    assert!(parts.iter().any(|part| {
        matches!(
            part,
            Part::Media { media_type, data, filename, .. }
                if media_type == "image/png"
                    && data == "data:image/png;base64,aGVsbG8="
                    && filename.as_deref() == Some("pixel.png")
        )
    }));
}

#[tokio::test]
async fn text_file_reference_stays_file_context_with_native_guidance() {
    let dir = tempdir();
    let text_path = dir.join("notes.txt");
    std::fs::write(&text_path, "reference body").unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider {
        requests: requests.clone(),
    })));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "base system".to_string(),
        workdir: dir.clone(),
        reasoning: None,
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let message = engine
        .admit_user_prompt(session, "Read @notes.txt".to_string())
        .await
        .unwrap();
    let file_url = format!("file://{}", text_path.to_string_lossy());
    engine
        .record_user_prompt_context(
            session,
            message,
            vec![json!({
                "type": "file",
                "mime": "text/plain",
                "filename": "notes.txt",
                "url": file_url,
                "source": {
                    "type": "file",
                    "path": text_path.to_string_lossy(),
                    "text": {"value": "@notes.txt", "start": 5, "end": 15}
                }
            }), json!({
                "type": "file",
                "mime": "application/x-directory",
                "filename": "reference",
                "url": format!("file://{}", dir.display()),
                "source": {"type": "file", "path": dir.to_string_lossy()}
            }), json!({
                "type": "file",
                "mime": "application/json",
                "filename": "resource",
                "url": "luna://test/resource",
                "source": {"type": "resource", "clientName": "fixture", "uri": "luna://test/resource"}
            })],
            Vec::new(),
        )
        .await
        .unwrap();

    engine
        .run_turn_with_external_dirs_and_guidance(
            session,
            &agent,
            CancellationToken::new(),
            std::slice::from_ref(&dir),
            Some(Arc::<str>::from("NATIVE_GUIDANCE_MARKER")),
            None,
        )
        .await
        .unwrap();

    let requests = requests.lock().unwrap();
    assert!(
        requests[0]
            .system
            .as_deref()
            .is_some_and(|system| system.contains("NATIVE_GUIDANCE_MARKER"))
    );
    let Message::User { parts, .. } = &requests[0].messages[0] else {
        panic!("expected user message");
    };
    assert!(parts.iter().all(|part| !matches!(part, Part::Media { .. })));
    assert!(
        parts
            .iter()
            .any(|part| { matches!(part, Part::Text { text, .. } if text == "Read @notes.txt") })
    );
}

async fn start_responses_capture_server(
    request_count: usize,
) -> (String, Arc<AsyncMutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(AsyncMutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    tokio::spawn(async move {
        for _ in 0..request_count {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0, "provider closed before request headers");
                bytes.extend_from_slice(&chunk[..read]);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            while bytes.len() < header_end + content_length {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0, "provider closed before request body");
                bytes.extend_from_slice(&chunk[..read]);
            }
            let body =
                serde_json::from_slice::<Value>(&bytes[header_end..header_end + content_length])
                    .unwrap();
            captured.lock().await.push(body);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\ndata: [DONE]\n\n",
                )
                .await
                .unwrap();
        }
    });
    (format!("http://{addr}/v1"), requests)
}

#[tokio::test]
async fn image_attachment_reaches_responses_and_replays_in_order() {
    let dir = tempdir();
    let (base_url, requests) = start_responses_capture_server(2).await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiResponse,
        &base_url,
        Some("test-token".to_string()),
        ["gpt-5.6-luna".to_string()],
    )
    .unwrap();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("openai/gpt-5.6-luna"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let first = engine
        .admit_user_prompt(session, "inspect attached image".to_string())
        .await
        .unwrap();
    let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=";
    engine
        .record_user_prompt_context(
            session,
            first,
            vec![json!({
                "type": "file",
                "url": image,
                "mime": "image/png",
                "filename": "red.png",
            })],
            Vec::new(),
        )
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    engine
        .admit_user_prompt(session, "follow up".to_string())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    let expected_content = json!([
        {"type": "input_text", "text": "inspect attached image"},
        {"type": "input_image", "image_url": image},
    ]);
    assert_eq!(requests[0]["input"][0]["role"], "user");
    assert_eq!(requests[0]["input"][0]["content"], expected_content);
    assert_eq!(requests[1]["input"][0]["content"], expected_content);
    assert_eq!(
        requests[1]["input"][1],
        json!({
            "role": "user",
            "content": "follow up",
        })
    );
}
