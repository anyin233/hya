//! Integration tests for `hya-core`: the one-shot model probe behind
//! `POST /v1/providers/{provider_id}/test`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use hya_core::{EventBus, SessionEngine};
use hya_proto::{Event, FinishReason, MessageId, ModelRef, Part, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

/// Records the probe request and replays a fixed stream outcome.
struct RecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    fail: bool,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "probe"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "probe/m").then(|| Capabilities {
            streaming_tool_calls: true,
            reasoning_request: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        if self.fail {
            return Ok(Box::pin(stream::once(async {
                Err(ProviderError::HttpStatus {
                    status: 401,
                    message: "bad key".to_string(),
                    retry_after: None,
                })
            })));
        }
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("h".to_string()),
                FakeStep::Finish(FinishReason::Length),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, _>),
        )))
    }
}

async fn engine(provider: RecordingProvider) -> SessionEngine {
    let store = SessionStore::connect_memory().await.unwrap();
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        store,
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
}

#[tokio::test]
async fn probe_sends_one_hi_message_without_tools_or_reasoning_and_reports_length_as_a_reply() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let engine = engine(RecordingProvider {
        requests: Arc::clone(&requests),
        fail: false,
    })
    .await;

    let reply = engine
        .probe_model(&ModelRef::new("probe/m"), 1)
        .await
        .expect("a length finish is a normal reply");
    assert_eq!(reply.text, "h");
    assert_eq!(reply.finish, Some(FinishReason::Length));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.max_output_tokens, Some(1));
    assert!(request.tools.is_empty());
    assert_eq!(request.reasoning, None);
    assert_eq!(request.system, None);
    assert_eq!(request.messages.len(), 1);
    match &request.messages[0] {
        hya_proto::Message::User { parts, .. } => match parts.as_slice() {
            [Part::Text { text, .. }] => assert_eq!(text, "hi"),
            other => panic!("unexpected parts {other:?}"),
        },
        other => panic!("unexpected message {other:?}"),
    }
}

#[tokio::test]
async fn probe_surfaces_stream_errors_and_unknown_models() {
    let engine = engine(RecordingProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        fail: true,
    })
    .await;
    let error = engine
        .probe_model(&ModelRef::new("probe/m"), 1)
        .await
        .expect_err("a stream error fails the probe");
    assert!(matches!(
        error,
        ProviderError::HttpStatus { status: 401, .. }
    ));

    let missing = engine
        .probe_model(&ModelRef::new("probe/missing"), 1)
        .await
        .expect_err("an unrouted model fails the probe");
    assert!(matches!(missing, ProviderError::UnknownModel(_)));
}
