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

/// One tool call emitted in a scripted model turn.
#[derive(Clone, Debug)]
pub struct ToolCallStep {
    /// Tool name the model “calls”.
    pub name: String,
    /// JSON arguments object for the call.
    pub arguments: Value,
}

/// A per-agent script queue selected by a marker found in the request's
/// `system`-role content.
///
/// Multi-agent scenarios (residents + main) issue interleaved completion
/// requests, so a single shared queue is nondeterministic by construction: agent
/// A can pop agent B's step. A route pins one agent's steps to that agent and
/// records only that agent's request bodies, which is what makes a
/// recipient-side delivery oracle possible.
#[derive(Clone, Debug)]
struct Route {
    marker: String,
    steps: VecDeque<ScriptStep>,
    requests: Vec<Value>,
}

#[derive(Clone, Debug, Default)]
struct Shared {
    scripts: VecDeque<ScriptStep>,
    requests: Vec<Value>,
    routes: Vec<Route>,
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
            routes: Vec::new(),
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

    /// OpenAI-compatible base URL including the `/v1` suffix for config.
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

    /// Unconsumed steps left on the shared (unrouted) script queue.
    pub fn remaining_scripts(&self) -> Result<usize, E2eError> {
        let guard = self
            .shared
            .lock()
            .map_err(|_| E2eError::Other("fake llm mutex poisoned".into()))?;
        Ok(guard.scripts.len())
    }

    /// Pin `steps` to the agent whose **system prompt** contains `marker`.
    ///
    /// Routes are matched in registration order against the concatenated
    /// `system`-role content of the incoming body only — not the whole request.
    /// The system prompt is the one part of the transcript that belongs solely
    /// to the agent being asked; a marker anywhere else (tool-call arguments,
    /// mail bodies) is echoed back into the *caller's* own history too, so
    /// whole-body matching would let the caller steal its callee's queue.
    ///
    /// With no routes registered, dispatch is byte-identical to the shared
    /// queue behavior, so pre-existing scenarios are unaffected.
    pub fn route(&self, marker: impl Into<String>, steps: Vec<ScriptStep>) -> Result<(), E2eError> {
        let mut guard = self
            .shared
            .lock()
            .map_err(|_| E2eError::Other("fake llm mutex poisoned".into()))?;
        guard.routes.push(Route {
            marker: marker.into(),
            steps: steps.into(),
            requests: Vec::new(),
        });
        Ok(())
    }

    /// Unconsumed steps left on `marker`'s route (`None` if never registered).
    ///
    /// A route that never reaches 0 is the signal that the marker matched the
    /// wrong agent — or no agent at all.
    pub fn route_remaining(&self, marker: &str) -> Result<Option<usize>, E2eError> {
        let guard = self
            .shared
            .lock()
            .map_err(|_| E2eError::Other("fake llm mutex poisoned".into()))?;
        Ok(guard
            .routes
            .iter()
            .find(|route| route.marker == marker)
            .map(|route| route.steps.len()))
    }

    /// Request bodies attributed to `marker`'s route, in arrival order.
    ///
    /// This is the recipient-side observation channel: mail delivered to a
    /// resident is injected as a user prompt into its next turn, so a delivered
    /// message appears here and nowhere else.
    pub fn route_requests(&self, marker: &str) -> Result<Option<Vec<Value>>, E2eError> {
        let guard = self
            .shared
            .lock()
            .map_err(|_| E2eError::Other("fake llm mutex poisoned".into()))?;
        Ok(guard
            .routes
            .iter()
            .find(|route| route.marker == marker)
            .map(|route| route.requests.clone()))
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
        // Attribution is by marker alone. An exhausted route does NOT fall back
        // to the shared queue: a resident that ran out of script must stop, not
        // start eating the main agent's steps.
        let system = system_text(&body.0);
        match guard
            .routes
            .iter_mut()
            .find(|route| system.contains(&route.marker))
        {
            Some(route) => {
                route.requests.push(body.0.clone());
                route.steps.pop_front()
            }
            None => guard.scripts.pop_front(),
        }
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

/// Concatenate the content of every `system`-role message in a chat body.
///
/// Used for route attribution: the system prompt identifies *which agent* is
/// being asked, unlike the rest of the transcript which is shared between an
/// agent and whoever quoted it.
fn system_text(body: &Value) -> String {
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return String::new();
    };
    messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
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

/// Build a text-only script step for FakeLlm.
pub fn text_step(s: impl Into<String>) -> ScriptStep {
    ScriptStep::Text(s.into())
}

/// Build a single-tool-call script step (name + JSON arguments).
pub fn tool_step(name: impl Into<String>, arguments: Value) -> ScriptStep {
    ScriptStep::ToolCalls(vec![ToolCallStep {
        name: name.into(),
        arguments,
    }])
}

/// Build a multi-tool-call script step from `(name, arguments)` pairs.
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// POST one chat completion whose system prompt is `system` and whose last
    /// user message is `user`; return the raw SSE body.
    async fn ask(base: &str, system: &str, user: &str) -> String {
        let body = json!({
            "model": "fake/model",
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "stream": true,
        });
        reqwest::Client::new()
            .post(format!("{base}/chat/completions"))
            .json(&body)
            .send()
            .await
            .expect("post")
            .text()
            .await
            .expect("body")
    }

    /// Two routed agents each consume their own queue, and neither touches the
    /// other's or the shared default queue.
    #[tokio::test]
    async fn routes_isolate_two_agents_from_each_other_and_the_default_queue() {
        let fake = FakeLlm::start(vec![text_step("DEFAULT_ONLY")])
            .await
            .expect("start");
        fake.route(
            "SYS_ALPHA",
            vec![text_step("ALPHA_1"), text_step("ALPHA_2")],
        )
        .expect("route alpha");
        fake.route("SYS_BETA", vec![text_step("BETA_1")])
            .expect("route beta");
        let base = fake.base_url();

        // The caller quotes the other agents' markers in its *user* content, the
        // way a real main agent quotes an inline_agent prompt in its tool-call
        // arguments. Attribution must ignore that.
        let default_body = ask(&base, "SYS_MAIN", "spawn SYS_ALPHA and SYS_BETA").await;
        assert!(
            default_body.contains("DEFAULT_ONLY"),
            "unrouted request must pop the shared queue; body={default_body}"
        );

        let alpha_1 = ask(&base, "SYS_ALPHA prompt", "go").await;
        let beta_1 = ask(&base, "SYS_BETA prompt", "go").await;
        let alpha_2 = ask(&base, "SYS_ALPHA prompt", "again").await;
        assert!(alpha_1.contains("ALPHA_1"), "body={alpha_1}");
        assert!(beta_1.contains("BETA_1"), "body={beta_1}");
        assert!(alpha_2.contains("ALPHA_2"), "body={alpha_2}");

        assert_eq!(fake.route_remaining("SYS_ALPHA").unwrap(), Some(0));
        assert_eq!(fake.route_remaining("SYS_BETA").unwrap(), Some(0));
        assert_eq!(
            fake.remaining_scripts().unwrap(),
            0,
            "routed agents must not drain the shared queue"
        );

        // Per-route request capture is the recipient-side observation channel.
        let alpha_requests = fake.route_requests("SYS_ALPHA").unwrap().expect("route");
        assert_eq!(alpha_requests.len(), 2);
        let alpha_dump = alpha_requests
            .iter()
            .map(Value::to_string)
            .collect::<String>();
        assert!(
            alpha_dump.contains("again") && !alpha_dump.contains("spawn SYS_ALPHA"),
            "alpha's capture must hold only alpha's requests; dump={alpha_dump}"
        );
        assert_eq!(fake.route_requests("SYS_MISSING").unwrap(), None);
    }

    /// An exhausted route stops its agent cleanly instead of falling through to
    /// the shared queue and stealing the main agent's next step.
    #[tokio::test]
    async fn exhausted_route_does_not_fall_back_to_the_shared_queue() {
        let fake = FakeLlm::start(vec![text_step("MAIN_STEP")])
            .await
            .expect("start");
        fake.route("SYS_ALPHA", vec![]).expect("route alpha");
        let base = fake.base_url();

        let alpha = ask(&base, "SYS_ALPHA prompt", "go").await;
        assert!(
            !alpha.contains("MAIN_STEP"),
            "empty route must not consume the shared queue; body={alpha}"
        );
        assert_eq!(fake.remaining_scripts().unwrap(), 1);

        let main = ask(&base, "SYS_MAIN", "go").await;
        assert!(main.contains("MAIN_STEP"), "body={main}");
    }
}
