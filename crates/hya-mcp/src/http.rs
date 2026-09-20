//! HTTP transports for MCP: Streamable HTTP (2025-06-18) and classic HTTP+SSE
//! (2024-11-05).
//!
//! Streamable HTTP POSTs each JSON-RPC message to the server URL and reads the
//! response either as one `application/json` body or as `text/event-stream`
//! frames; the `Mcp-Session-Id` response header of `initialize` is replayed on
//! every later request, and a 404 after a session was established means the
//! server lost the session. Classic HTTP+SSE opens a GET event stream whose
//! first `endpoint` event names the POST target; responses travel back on the
//! stream and are demultiplexed by request id.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};

use crate::client::{McpError, Pending};
use crate::protocol::{JsonRpcRequest, JsonRpcResponse};

/// `Accept` header value required by the Streamable HTTP spec.
const STREAMABLE_ACCEPT: &str = "application/json, text/event-stream";
/// Cap on error-body text carried inside [`McpError::Http`].
const MAX_ERROR_BODY_BYTES: usize = 512;

/// Map a decoded JSON-RPC response envelope onto the client result type.
pub(crate) fn response_result(response: JsonRpcResponse) -> Result<Value, McpError> {
    match (response.result, response.error) {
        (Some(value), None) => Ok(value),
        (_, Some(error)) => Err(McpError::Rpc {
            code: error.code,
            message: error.message,
        }),
        (None, None) => Err(McpError::Closed),
    }
}

fn http_error(status: u16, body: String) -> McpError {
    let mut detail = body;
    if detail.len() > MAX_ERROR_BODY_BYTES {
        detail.truncate(MAX_ERROR_BODY_BYTES);
        detail.push('…');
    }
    McpError::Http {
        status,
        detail: detail.trim().to_string(),
    }
}

/// Streamable HTTP transport: one POST per JSON-RPC message, session header
/// captured from the `initialize` response.
pub(crate) struct StreamableHttpTransport {
    http: reqwest::Client,
    url: String,
    next_id: AtomicU64,
    state: StdMutex<StreamableState>,
}

#[derive(Default)]
struct StreamableState {
    session: Option<String>,
    protocol_version: Option<String>,
}

impl StreamableHttpTransport {
    pub(crate) fn new(url: String) -> Result<Self, McpError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| McpError::Io(e.to_string()))?;
        Ok(Self {
            http,
            url,
            next_id: AtomicU64::new(1),
            state: StdMutex::new(StreamableState::default()),
        })
    }

    fn session_header(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session
            .clone()
    }

    fn remember_handshake(&self, response: &reqwest::Response) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
        {
            state.session = Some(session.to_string());
        }
        if let Some(version) = response
            .headers()
            .get("mcp-protocol-version")
            .and_then(|value| value.to_str().ok())
        {
            state.protocol_version = Some(version.to_string());
        }
    }

    async fn send(&self, body: Value) -> Result<reqwest::Response, McpError> {
        let mut request = self
            .http
            .post(&self.url)
            .header("Accept", STREAMABLE_ACCEPT)
            .json(&body);
        if let Some(session) = self.session_header() {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(version) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .protocol_version
            .clone()
        {
            request = request.header("MCP-Protocol-Version", version);
        }
        request
            .send()
            .await
            .map_err(|e| McpError::Io(e.to_string()))
    }

    /// POST one JSON-RPC request and decode the response in either wire mode.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };
        let body = serde_json::to_value(&request).map_err(|e| McpError::Json(e.to_string()))?;
        let is_initialize = method == "initialize";

        let fut = async {
            let response = self.send(body).await?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(http_error(status.as_u16(), body));
            }
            if is_initialize {
                self.remember_handshake(&response);
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if content_type.contains("text/event-stream") {
                read_matching_sse_response(response, id).await
            } else {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|e| McpError::Io(e.to_string()))?;
                let response: JsonRpcResponse =
                    serde_json::from_slice(&bytes).map_err(|e| McpError::Json(e.to_string()))?;
                response_result(response)
            }
        };
        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| McpError::Timeout {
                method: method.to_string(),
            })?
    }

    /// POST one JSON-RPC notification; the server answers 202 without a body.
    pub(crate) async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let response = self.send(body).await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(http_error(status.as_u16(), text));
        }
        Ok(())
    }
}

/// Read an SSE response body until the frame whose JSON-RPC `id` matches.
async fn read_matching_sse_response(
    response: reqwest::Response,
    id: u64,
) -> Result<Value, McpError> {
    let mut events = response.bytes_stream().eventsource();
    while let Some(event) = events.next().await {
        let event = event.map_err(|e| McpError::Io(e.to_string()))?;
        if !(event.event.is_empty() || event.event == "message") {
            continue;
        }
        let data = event.data.trim();
        if data.is_empty() {
            continue;
        }
        let parsed: JsonRpcResponse =
            serde_json::from_str(data).map_err(|e| McpError::Json(e.to_string()))?;
        if parsed.id == id {
            return response_result(parsed);
        }
    }
    Err(McpError::Closed)
}

/// Classic HTTP+SSE transport: GET event stream first, POST requests to the
/// endpoint event, responses demultiplexed off the stream by id.
pub(crate) struct ClassicSseTransport {
    http: reqwest::Client,
    post_url: String,
    next_id: AtomicU64,
    pending: Pending,
}

impl ClassicSseTransport {
    /// Open the SSE channel and wait for its `endpoint` event.
    pub(crate) async fn connect(url: &str) -> Result<Self, McpError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| McpError::Io(e.to_string()))?;
        let response = http
            .get(url)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(http_error(status.as_u16(), body));
        }
        let base: reqwest::Url = url
            .parse()
            .map_err(|e| McpError::Io(format!("invalid sse url: {e}")))?;

        let pending: Pending = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let (endpoint_tx, endpoint_rx) = oneshot::channel();
        tokio::spawn(run_sse_stream(response, base, pending.clone(), endpoint_tx));
        let post_url = tokio::time::timeout(Duration::from_secs(10), endpoint_rx)
            .await
            .map_err(|_| McpError::Timeout {
                method: "sse endpoint".to_string(),
            })?
            .map_err(|_| McpError::Closed)?;
        Ok(Self {
            http,
            post_url,
            next_id: AtomicU64::new(1),
            pending,
        })
    }

    /// POST a request and await its response frame on the SSE stream.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let response = self
            .http
            .post(&self.post_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| McpError::Io(e.to_string()));
        match response {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                self.pending.lock().await.remove(&id);
                return Err(http_error(status.as_u16(), body));
            }
            Err(error) => {
                self.pending.lock().await.remove(&id);
                return Err(error);
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(McpError::Closed),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(McpError::Timeout {
                    method: method.to_string(),
                })
            }
        }
    }

    /// POST a notification; no response frame is awaited.
    pub(crate) async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let response = self
            .http
            .post(&self.post_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| McpError::Io(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(http_error(status.as_u16(), text));
        }
        Ok(())
    }
}

async fn run_sse_stream(
    response: reqwest::Response,
    base: reqwest::Url,
    pending: Pending,
    endpoint_tx: oneshot::Sender<String>,
) {
    let mut endpoint_tx = Some(endpoint_tx);
    let mut events = response.bytes_stream().eventsource();
    while let Some(event) = events.next().await {
        let Ok(event) = event else {
            break;
        };
        let data = event.data.trim().to_string();
        if data.is_empty() {
            continue;
        }
        if event.event == "endpoint" {
            let post_url = base
                .join(&data)
                .map(|joined| joined.to_string())
                .unwrap_or(data);
            if let Some(tx) = endpoint_tx.take() {
                let _ = tx.send(post_url);
            }
            continue;
        }
        if !(event.event.is_empty() || event.event == "message") {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<JsonRpcResponse>(&data) else {
            continue;
        };
        if let Some(tx) = pending.lock().await.remove(&parsed.id) {
            let _ = tx.send(response_result(parsed));
        }
    }
    crate::client::close_pending(&pending, McpError::Closed).await;
}
