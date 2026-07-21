#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::stream;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{
    AgentName, Event, FinishReason, Message, MessageId, MessageProjection, ModelRef, Part, PartId,
    PartProjection, Projection, Role, SessionId, SessionProjection,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

struct DelayedDeltaProvider {
    request: Arc<Mutex<Option<CompletionRequest>>>,
}

#[async_trait]
impl Provider for DelayedDeltaProvider {
    fn id(&self) -> &str {
        "delayed"
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
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        *self.request.lock().expect("request capture") = Some(req);
        let part = PartId::new();
        let events = vec![
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: "hello".to_string(),
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: " world".to_string(),
            },
            Event::TextEnd {
                session,
                message,
                part,
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
            },
        ];
        Ok(Box::pin(stream::unfold(
            (events.into_iter(), 0usize),
            |(mut events, index)| async move {
                let event = events.next()?;
                if index == 2 {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Some((Ok(event), (events, index + 1)))
            },
        )))
    }
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-stream-round-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

fn agent(workdir: &Path) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: workdir.to_path_buf(),
        reasoning: None,
    }
}

async fn next_text_delta(
    live: &mut broadcast::Receiver<hya_proto::Envelope>,
    expected: &str,
) -> String {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let envelope = live.recv().await.expect("live event");
            if let Event::TextDelta { delta, .. } = envelope.event
                && delta == expected
            {
                return delta;
            }
        }
    })
    .await
    .expect("live delta")
}

#[tokio::test]
async fn stream_round_deltas_are_live_but_replay_commits_final_text_once() {
    let workdir = tempdir();
    let router = ProviderRouter::new().with(Arc::new(DelayedDeltaProvider {
        request: Arc::new(Mutex::new(None)),
    }));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let bus = EventBus::default();
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.expect("store"),
        Arc::new(router),
        tools,
        permission,
        bus,
    ));
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .expect("create session");
    engine
        .admit_user_prompt(session, "stream please".to_string())
        .await
        .expect("admit prompt");
    let mut live = engine.bus().subscribe();
    let run_engine = engine.clone();
    let run_agent = agent(&workdir);
    let turn = tokio::spawn(async move {
        run_engine
            .run_turn(session, &run_agent, CancellationToken::new())
            .await
    });

    assert_eq!(next_text_delta(&mut live, "hello").await, "hello");
    assert!(
        !turn.is_finished(),
        "first text delta must be live before completion"
    );
    assert_eq!(turn.await.expect("join").expect("turn"), FinishReason::Stop);
    assert_eq!(next_text_delta(&mut live, " world").await, " world");

    let replay = engine.store().replay(session).await.expect("replay");
    let assistant = replay.iter().find_map(|envelope| match envelope.event {
        Event::MessageStarted {
            message,
            role: Role::Assistant,
            ..
        } => Some(message),
        _ => None,
    });
    let durable_projection = hya_proto::Projection::from_events(&replay);
    let assistant_text = durable_projection
        .session
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .and_then(|message| {
            message.parts.iter().find_map(|part| match part {
                PartProjection::Text { text, .. } => Some(text.as_str()),
                PartProjection::Reasoning { .. } | PartProjection::Tool { .. } => None,
            })
        });
    let durable_assistant_deltas = replay
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            Event::TextDelta { message, delta, .. } if Some(message) == assistant => Some(delta),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(assistant_text, Some("hello world"));
    assert!(
        durable_assistant_deltas.len() <= 1,
        "durable replay must not commit every streamed token delta: {durable_assistant_deltas:?}"
    );
}

#[tokio::test]
async fn forked_reasoning_provider_data_reaches_next_request() {
    let workdir = tempdir();
    let request = Arc::new(Mutex::new(None));
    let router = ProviderRouter::new().with(Arc::new(DelayedDeltaProvider {
        request: request.clone(),
    }));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.expect("store"),
        Arc::new(router),
        tools,
        permission,
        EventBus::default(),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .expect("create session");
    let provider_data = serde_json::json!({
        "type": "reasoning",
        "id": "rs_123",
        "encrypted_content": "opaque",
    });
    let source = Projection {
        session: SessionProjection {
            messages: vec![MessageProjection {
                id: MessageId::new(),
                role: Role::Assistant,
                finish: Some(FinishReason::Stop),
                tokens: None,
                files: Vec::new(),
                agents: Vec::new(),
                parts: vec![PartProjection::Reasoning {
                    id: PartId::new(),
                    text: "visible summary".to_string(),
                    provider_data: Some(provider_data.clone()),
                }],
            }],
            ..SessionProjection::default()
        },
        ..Projection::default()
    };

    engine
        .copy_messages_to_session(session, &source, None)
        .await
        .expect("copy messages");
    let replay = engine.store().replay(session).await.expect("replay");
    let forked = Projection::from_events(&replay);
    let PartProjection::Reasoning {
        text,
        provider_data: forked_data,
        ..
    } = &forked.session.messages[0].parts[0]
    else {
        panic!("forked reasoning part");
    };
    assert_eq!(text, "visible summary");
    assert_eq!(forked_data.as_ref(), Some(&provider_data));

    engine
        .admit_user_prompt(session, "continue".to_string())
        .await
        .expect("admit prompt");
    engine
        .run_turn(session, &agent(&workdir), CancellationToken::new())
        .await
        .expect("run turn");
    let request = request.lock().expect("request capture");
    let request = request.as_ref().expect("completion request");
    let sent = request.messages.iter().find_map(|message| match message {
        Message::Assistant { parts, .. } => parts.iter().find_map(|part| match part {
            Part::Reasoning { provider_data, .. } => provider_data.as_ref(),
            _ => None,
        }),
        _ => None,
    });
    assert_eq!(sent, Some(&provider_data));
}
