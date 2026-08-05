//! Scripted OpenAI-compatible chat completions server (SSE).

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::error::E2eError;

/// One model turn produced by the fake LLM.
#[derive(Clone, Debug)]
pub enum ScriptStep {
    /// Stream assistant text then finish stop.
    Text(String),
    /// Stream one or more tool calls then finish tool_calls.
    ToolCalls(Vec<ToolCallStep>),
}

#[derive(Clone, Debug)]
pub struct ToolCallStep {
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Default)]
struct Shared {
    scripts: VecDeque<ScriptStep>,
    requests: Vec<Value>,
}

/// Running FakeLlm HTTP server.
pub struct FakeLlm {
    addr: SocketAddr,
    shared: Arc<Mutex<Shared>>,
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

/// Handle shared with axum handlers.
#[derive(Clone)]
pub struct FakeLlmHandle {
    shared: Arc<Mutex<Shared>>,
}

impl FakeLlm {
    /// Bind `127.0.0.1:0` and serve `/v1/chat/completions`.
    pub async fn start(scripts: Vec<ScriptStep>) -> Result<Self, E2eError> {
        let shared = Arc::new(Mutex::new(Shared {
            scripts: scripts.into(),
            requests: Vec::new(),
        }));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| E2eError::Other(format!("bind fake llm: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| E2eError::Other(format!("local_addr: {e}")))?;
        let (tx, rx) = oneshot::channel::<()>();
        let state = FakeLlmHandle {
            shared: Arc::clone(&shared),
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .with_state(state);
        let join = tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = rx.await;
            });
            let _ = server.await;
        });
        Ok(Self {
            addr,
            shared,
            shutdown: Some(tx),
            join: Some(join),
        })
    }

    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// Recorded request bodies (for assertions).
    pub fn requests(&self) -> Result<Vec<Value>, E2eError> {
        let guard = self
            .shared
            .lock()
            .map_err(|_| E2eError::Other("fake llm mutex poisoned".into()))?;
        Ok(guard.requests.clone())
    }

    pub fn remaining_scripts(&self) -> Result<usize, E2eError> {
        let guard = self
            .shared
            .lock()
            .map_err(|_| E2eError::Other("fake llm mutex poisoned".into()))?;
        Ok(guard.scripts.len())
    }
}

impl Drop for FakeLlm {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

async fn chat_completions(
    State(state): State<FakeLlmHandle>,
    _headers: HeaderMap,
    body: axum::Json<Value>,
) -> Response {
    let step = {
        let Ok(mut guard) = state.shared.lock() else {
            return (StatusCode::INTERNAL_SERVER_ERROR, "mutex poisoned").into_response();
        };
        guard.requests.push(body.0.clone());
        guard.scripts.pop_front()
    };
    let Some(step) = step else {
        // Terminate agent loop cleanly if scripts exhausted.
        return sse_response(vec![
            json!({"choices":[{"delta":{"content":""},"finish_reason":null}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            Value::String("[DONE]".into()),
        ]);
    };
    let frames = match step {
        ScriptStep::Text(text) => vec![
            json!({"choices":[{"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
            json!({"choices":[{"delta":{"content": text},"finish_reason":null}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            Value::String("[DONE]".into()),
        ],
        ScriptStep::ToolCalls(calls) => {
            let mut frames = Vec::new();
            for (index, call) in calls.iter().enumerate() {
                let args = call.arguments.to_string();
                let id = format!("call_{index}");
                frames.push(json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": index,
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": ""
                                }
                            }]
                        },
                        "finish_reason": null
                    }]
                }));
                frames.push(json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": index,
                                "function": { "arguments": args }
                            }]
                        },
                        "finish_reason": null
                    }]
                }));
            }
            frames.push(json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}));
            frames.push(Value::String("[DONE]".into()));
            frames
        }
    };
    sse_response(frames)
}

fn sse_response(frames: Vec<Value>) -> Response {
    let mut body = String::new();
    for frame in frames {
        match frame {
            Value::String(s) if s == "[DONE]" => {
                body.push_str("data: [DONE]\n\n");
            }
            other => {
                body.push_str("data: ");
                body.push_str(&other.to_string());
                body.push_str("\n\n");
            }
        }
    }
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(Body::from(body))
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "response build failed").into_response()
        })
}

/// Helper constructors.
pub fn text_step(s: impl Into<String>) -> ScriptStep {
    ScriptStep::Text(s.into())
}

pub fn tool_step(name: impl Into<String>, arguments: Value) -> ScriptStep {
    ScriptStep::ToolCalls(vec![ToolCallStep {
        name: name.into(),
        arguments,
    }])
}

pub fn tools_step(calls: Vec<(&str, Value)>) -> ScriptStep {
    ScriptStep::ToolCalls(
        calls
            .into_iter()
            .map(|(name, arguments)| ToolCallStep {
                name: name.to_string(),
                arguments,
            })
            .collect(),
    )
}
