//! The gRPC binding for `hya.v1`: every unary rpc dispatches through the
//! same axum `/v1` router the HTTP transport serves (protojson in/out), so
//! dual-transport parity holds by construction. Streaming rpcs share the
//! engine-bus producers with the SSE handlers.

use std::collections::BTreeMap;
use std::pin::Pin;

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode};
use futures::StreamExt;
use hya_api::v1 as pb;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_stream::Stream;
use tonic::{Request as GrpcRequest, Response as GrpcResponse, Status};

use crate::{AppState, Origin};

/// Shared state + router used by every generated service impl.
#[derive(Clone)]
pub struct V1Grpc {
    state: crate::ServerState,
    router: axum::Router,
}

impl V1Grpc {
    /// Build the binding over the application state's one server state
    /// and its `/v1` router: a router, binding, or [`crate::Server`] made
    /// from the same `AppState` (or a clone) shares it.
    #[must_use]
    pub fn new(app: AppState) -> Self {
        Self {
            state: app.server_state(),
            router: crate::router(app),
        }
    }

    /// Every `hya.v1` gRPC service of this binding (the `CreateTurn`
    /// message limit included), ready to serve — what [`crate::Server`]
    /// routes `application/grpc*` requests to.
    #[must_use]
    pub fn routes(self) -> tonic::service::Routes {
        use pb::{
            agent_models_server::AgentModelsServer, auth_server::AuthServer,
            bundle_api_server::BundleApiServer, catalog_server::CatalogServer,
            events_server::EventsServer, files_server::FilesServer,
            interactions_server::InteractionsServer, logs_server::LogsServer,
            mcp_server::McpServer, messages_server::MessagesServer, process_server::ProcessServer,
            project_server::ProjectServer, pty_server::PtyServer,
            relay_control_server::RelayControlServer, session_server::SessionServer,
            turn_server::TurnServer, workflow_server::WorkflowServer,
            worktrees_server::WorktreesServer,
        };
        tonic::service::Routes::new(ProcessServer::new(self.clone()))
            .add_service(CatalogServer::new(self.clone()))
            .add_service(AgentModelsServer::new(self.clone()))
            .add_service(AuthServer::new(self.clone()))
            .add_service(SessionServer::new(self.clone()))
            .add_service(
                TurnServer::new(self.clone())
                    .max_decoding_message_size(crate::MAX_TURN_GRPC_MESSAGE_BYTES),
            )
            .add_service(MessagesServer::new(self.clone()))
            .add_service(EventsServer::new(self.clone()))
            .add_service(InteractionsServer::new(self.clone()))
            .add_service(WorkflowServer::new(self.clone()))
            .add_service(FilesServer::new(self.clone()))
            .add_service(ProjectServer::new(self.clone()))
            .add_service(WorktreesServer::new(self.clone()))
            .add_service(McpServer::new(self.clone()))
            .add_service(PtyServer::new(self.clone()))
            .add_service(LogsServer::new(self.clone()))
            .add_service(BundleApiServer::new(self.clone()))
            .add_service(RelayControlServer::new(self))
            .prepare()
    }

    fn status_from_error_body(status: StatusCode, body: &[u8]) -> Status {
        let code = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.get("code"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let message = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
        let tonic_code = hya_api::error::Code::from_wire(&code)
            .map_or(tonic::Code::Internal, |code| code.grpc_code());
        let _ = status;
        Status::new(tonic_code, message)
    }

    /// Call the `/v1` router in process with `request` as protojson,
    /// carrying the gRPC call's context (`ctx`): its origin (a relay-origin
    /// call stays one, so the relay refusals hold), its peer address, and
    /// its admitted host and browser headers.
    #[allow(clippy::result_large_err)]
    async fn dispatch<Req, Resp>(
        &self,
        ctx: &Call,
        method: &str,
        path: &str,
        query: impl IntoIterator<Item = (String, String)>,
        request: &Req,
    ) -> Result<Resp, Status>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let mut uri = String::from(path);
        for (index, (key, value)) in query.into_iter().enumerate() {
            uri.push(if index == 0 { '?' } else { '&' });
            uri.push_str(&format!("{}={}", encode(&key), encode(&value)));
        }
        let body = serde_json::to_vec(request)
            .map_err(|error| Status::internal(format!("serialize request: {error}")))?;
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Ok(value) = HeaderValue::from_str(&body_len(&body)) {
            builder = builder.header("content-length", value);
        }
        let mut http_request = builder
            .body(Body::from(body))
            .map_err(|error| Status::internal(format!("build request: {error}")))?;
        ctx.apply(&mut http_request);
        let mut router = self.router.clone();
        use tower::Service;
        let response = router
            .call(http_request)
            .await
            .map_err(|error| Status::internal(format!("dispatch: {error}")))?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .map_err(|error| Status::internal(format!("read body: {error}")))?;
        if !status.is_success() {
            return Err(Self::status_from_error_body(status, &bytes));
        }
        serde_json::from_slice(&bytes)
            .map_err(|error| Status::internal(format!("decode response: {error}")))
    }

    #[allow(clippy::result_large_err)]
    async fn get<Req, Resp>(&self, ctx: &Call, path: &str, request: &Req) -> Result<Resp, Status>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let query = query_of(request);
        self.dispatch(ctx, "GET", path, query, request).await
    }

    #[allow(clippy::result_large_err)]
    async fn post<Req, Resp>(&self, ctx: &Call, path: &str, request: &Req) -> Result<Resp, Status>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        self.dispatch::<Req, Resp>(ctx, "POST", path, BTreeMap::new(), request)
            .await
    }
}

/// What the in-process dispatch keeps of a gRPC call: tonic hands a
/// handler the message, metadata, and extensions, but the synthesized
/// router request starts empty, so the parts the router's guard and
/// handlers read are carried over explicitly.
#[derive(Clone, Debug, Default)]
pub(crate) struct Call {
    /// The call arrived through the relay ([`Origin::Relay`]).
    relay: bool,
    /// The TCP peer (`ConnectInfo` or tonic's `TcpConnectInfo`).
    peer: Option<std::net::SocketAddr>,
    /// `host` (the admitted `:authority`), `origin`, and `sec-fetch-*`.
    headers: axum::http::HeaderMap,
}

/// The headers a gRPC call's dispatch forwards (besides `host`): the
/// browser markers the relay guard and the relay-control rpcs check, and the
/// removed `x-hya-directory` scope header, so the router's guard refuses it
/// on a binding used without [`crate::GrpcHostLayer`] too.
const FORWARDED_HEADERS: [&str; 5] = [
    "origin",
    "sec-fetch-site",
    "sec-fetch-mode",
    "sec-fetch-dest",
    super::REMOVED_DIRECTORY_HEADER,
];

impl Call {
    fn of<T>(request: &GrpcRequest<T>) -> Self {
        let extensions = request.extensions();
        let peer = extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|axum::extract::ConnectInfo(peer)| *peer)
            .or_else(|| request.remote_addr());
        let metadata = request.metadata().clone().into_headers();
        let mut headers = axum::http::HeaderMap::new();
        let host = extensions
            .get::<crate::host::GrpcAuthority>()
            .map(|authority| authority.0.clone())
            .or_else(|| metadata.get(axum::http::header::HOST).cloned());
        if let Some(host) = host {
            headers.insert(axum::http::header::HOST, host);
        }
        for name in FORWARDED_HEADERS {
            if let Some(value) = metadata.get(name) {
                headers.insert(name, value.clone());
            }
        }
        Self {
            relay: Origin::of(extensions) == Origin::Relay,
            peer,
            headers,
        }
    }

    /// Put this context on the synthesized router request.
    fn apply(&self, request: &mut axum::http::Request<Body>) {
        for (name, value) in &self.headers {
            request.headers_mut().insert(name, value.clone());
        }
        if self.relay {
            request.extensions_mut().insert(Origin::Relay);
        }
        if let Some(peer) = self.peer {
            request
                .extensions_mut()
                .insert(super::relay::GrpcPeer(peer));
        }
    }
}

/// Split a gRPC request into its dispatch context and message.
fn split<T>(request: GrpcRequest<T>) -> (Call, T) {
    (Call::of(&request), request.into_inner())
}

fn body_len(body: &[u8]) -> String {
    body.len().to_string()
}

/// Serialize a request into non-empty query parameters. A repeated string
/// field becomes one `key=value` pair per element (`?paths=a&paths=b`).
/// Nested messages use dotted keys, matching the HTTP query contract.
fn query_of<Req: Serialize>(request: &Req) -> Vec<(String, String)> {
    let mut query = Vec::new();
    let Ok(Value::Object(map)) = serde_json::to_value(request) else {
        return query;
    };
    for (key, value) in map {
        let text = match value {
            Value::String(text) => text,
            Value::Number(number) => number.to_string(),
            Value::Bool(flag) => flag.to_string(),
            Value::Array(items) => {
                for item in items {
                    if let Value::String(text) = item
                        && !text.is_empty()
                    {
                        query.push((key.clone(), text));
                    }
                }
                continue;
            }
            Value::Object(fields) => {
                for (nested_key, nested_value) in query_of(&fields) {
                    query.push((format!("{key}.{nested_key}"), nested_value));
                }
                continue;
            }
            _ => continue,
        };
        if !text.is_empty() && text != "0" && text != "false" {
            query.push((key, text));
        }
    }
    query
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn field(request: &impl Serialize, name: &str) -> String {
    serde_json::to_value(request)
        .ok()
        .and_then(|value| value.get(name).cloned())
        .and_then(|value| match value {
            Value::String(text) => Some(text),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

#[allow(clippy::result_large_err)]
fn into_response<T>(message: T) -> Result<GrpcResponse<T>, Status> {
    Ok(GrpcResponse::new(message))
}

impl V1Grpc {
    /// Serve one bundle API call through the same engine path the HTTP
    /// routes use. The HTTP handler answers the process's status and body
    /// verbatim (no envelope), so this binding calls the shared
    /// [`super::bundle_api::invoke`] directly instead of decoding an HTTP
    /// response: a process status (2xx or not) is an OK gRPC reply carrying
    /// `status`, and only host-side failures become a gRPC error.
    #[allow(clippy::result_large_err)]
    async fn invoke_bundle_api(
        &self,
        call: Result<hya_core::BundleApiCall, super::V1Error>,
    ) -> Result<GrpcResponse<pb::BundleApiResponse>, Status> {
        let call = call.map_err(|error| error.grpc_status())?;
        let bundle = call.bundle.clone();
        let outcome = super::bundle_api::invoke(&self.state, call)
            .await
            .map_err(|error| super::V1Error::from(error).grpc_status())?;
        let (content_type, body) = if outcome.body.is_null() {
            (String::new(), None)
        } else {
            (
                super::bundle_api::BUNDLE_API_CONTENT_TYPE.to_string(),
                Some(
                    super::bundle_api::to_pb_value(outcome.body)
                        .map_err(|error| error.grpc_status())?,
                ),
            )
        };
        into_response(pb::BundleApiResponse {
            bundle,
            api: outcome.api,
            status: u32::from(outcome.status),
            content_type,
            body,
        })
    }
}

/// Build an engine call from gRPC fields; `session` is `None` for a global
/// endpoint.
fn bundle_api_call(
    bundle: String,
    session: Option<&str>,
    method: &str,
    path: String,
    query: std::collections::HashMap<String, String>,
    body: Option<pbjson_types::Value>,
) -> Result<hya_core::BundleApiCall, super::V1Error> {
    let session = session.map(super::session::parse_session).transpose()?;
    let method = super::bundle_api::parse_method(method)?;
    let body = match body {
        Some(body) => serde_json::to_value(body)
            .map_err(|error| super::V1Error::internal(format!("decode body: {error}")))?,
        None => Value::Null,
    };
    Ok(hya_core::BundleApiCall {
        bundle,
        method,
        session,
        path,
        query: query.into_iter().collect(),
        body,
    })
}

impl V1Grpc {
    async fn special_invoke_session_bundle_api(
        &self,
        request: GrpcRequest<pb::InvokeSessionBundleApiRequest>,
    ) -> Result<GrpcResponse<pb::BundleApiResponse>, Status> {
        let inner = request.into_inner();
        self.invoke_bundle_api(bundle_api_call(
            inner.bundle,
            Some(&inner.session),
            &inner.method,
            inner.path,
            inner.query,
            inner.body,
        ))
        .await
    }

    async fn special_invoke_global_bundle_api(
        &self,
        request: GrpcRequest<pb::InvokeGlobalBundleApiRequest>,
    ) -> Result<GrpcResponse<pb::BundleApiResponse>, Status> {
        let inner = request.into_inner();
        self.invoke_bundle_api(bundle_api_call(
            inner.bundle,
            None,
            &inner.method,
            inner.path,
            inner.query,
            inner.body,
        ))
        .await
    }
}

impl V1Grpc {
    async fn special_wait_turn(
        &self,
        request: GrpcRequest<pb::WaitTurnRequest>,
    ) -> Result<GrpcResponse<pb::TurnInfo>, Status> {
        let (ctx, inner) = split(request);
        let session = field(&inner, "session");
        let turn = field(&inner, "turn");
        let timeout = field(&inner, "timeoutMs");
        into_response(
            self.post(
                &ctx,
                &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs={timeout}"),
                &pb::WaitTurnRequest::default(),
            )
            .await?,
        )
    }
}

impl V1Grpc {
    async fn special_stream_session_events(
        &self,
        request: GrpcRequest<pb::StreamSessionEventsRequest>,
    ) -> Result<
        GrpcResponse<Pin<Box<dyn Stream<Item = Result<pb::StreamFrame, Status>> + Send>>>,
        Status,
    > {
        let inner = request.into_inner();
        let session = inner
            .session
            .parse::<hya_proto::SessionId>()
            .map_err(|_| Status::invalid_argument("invalid session id"))?;
        match self.state.engine.session_exists(session).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(Status::not_found(format!("session not found: {session}")));
            }
            Err(error) => return Err(Status::internal(error.to_string())),
        }
        Ok(GrpcResponse::new(Box::pin(
            super::events::frame_stream(
                self.state.clone(),
                super::events::StreamScope::session(session, inner.include_descendants),
                inner.since_seq,
                false,
            )
            .map(|frame| Ok(frame.frame.clone())),
        )))
    }

    async fn special_stream_global_events(
        &self,
        request: GrpcRequest<pb::StreamGlobalEventsRequest>,
    ) -> Result<
        GrpcResponse<Pin<Box<dyn Stream<Item = Result<pb::StreamFrame, Status>> + Send>>>,
        Status,
    > {
        let inner = request.into_inner();
        Ok(GrpcResponse::new(Box::pin(
            super::events::frame_stream(
                self.state.clone(),
                super::events::StreamScope::Global,
                inner.since_seq,
                inner.interactions_only,
            )
            .map(|frame| Ok(frame.frame.clone())),
        )))
    }
}

impl V1Grpc {
    async fn special_stream_pty(
        &self,
        request: GrpcRequest<tonic::Streaming<pb::PtyClientFrame>>,
    ) -> Result<
        GrpcResponse<Pin<Box<dyn Stream<Item = Result<pb::PtyServerFrame, Status>> + Send>>>,
        Status,
    > {
        use crate::support::pty_state::PtyEvent;
        use hya_api::v1::pty_client_frame::Frame as F;
        let mut client = request.into_inner();
        // The first client frame must attach to a PTY session.
        let attach = match client.next().await {
            Some(Ok(frame)) => match frame.frame {
                Some(F::Attach(attach)) => attach,
                _ => return Err(Status::invalid_argument("first frame must attach")),
            },
            _ => return Err(Status::cancelled("stream closed before attach")),
        };
        let id = attach.id;
        let Some(mut attachment) = self.state.pty.attach(&id, None).await else {
            return Err(Status::not_found(format!("pty session not found: {id}")));
        };
        let state = self.state.clone();
        let (output_tx, output_rx) =
            tokio::sync::mpsc::channel::<Result<pb::PtyServerFrame, Status>>(64);
        tokio::spawn(async move {
            if !attachment.replay.is_empty()
                && output_tx
                    .send(Ok(pb::PtyServerFrame {
                        frame: Some(pb::pty_server_frame::Frame::Output(
                            attachment.replay.clone().into_bytes(),
                        )),
                    }))
                    .await
                    .is_err()
            {
                return;
            }
            loop {
                tokio::select! {
                    frame = client.next() => {
                        let Some(Ok(frame)) = frame else { break };
                        match frame.frame {
                            Some(F::Input(bytes)) => {
                                let _ = state.pty.write(&id, &String::from_utf8_lossy(&bytes)).await;
                            }
                            Some(F::Attach(_)) | Some(F::Resize(_)) | Some(F::Ping(_)) | None => {}
                        }
                    }
                    event = attachment.events.recv() => match event {
                        Ok(PtyEvent::Data(chunk)) => {
                            if output_tx
                                .send(Ok(pb::PtyServerFrame {
                                    frame: Some(pb::pty_server_frame::Frame::Output(
                                        chunk.into_bytes(),
                                    )),
                                }))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(PtyEvent::End) => {
                            let _ = output_tx
                                .send(Ok(pb::PtyServerFrame {
                                    frame: Some(pb::pty_server_frame::Frame::Exit(0)),
                                }))
                                .await;
                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    },
                }
            }
        });
        Ok(GrpcResponse::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(output_rx),
        )))
    }
}

// ---------------------------------------------------------------------------
// RelayControl (loopback-only: a relay-origin, non-loopback, or unknown
// gRPC peer is refused here; the in-process dispatch carries the origin and
// the peer, so the router's check sees them too)
// ---------------------------------------------------------------------------

/// Refuse a relay-control call that did not come from the local owner:
/// through the relay, from a non-loopback peer, or from an unknown one
/// (fails closed).
#[allow(clippy::result_large_err)]
fn require_loopback_peer(ctx: &Call) -> Result<(), Status> {
    if ctx.relay {
        return Err(Status::permission_denied(
            "relay control is loopback-only: refused for a request that arrived through the relay",
        ));
    }
    match ctx.peer {
        Some(peer) if peer.ip().is_loopback() => Ok(()),
        Some(_) => Err(Status::permission_denied(
            "relay control is loopback-only: refused for a non-loopback client",
        )),
        None => Err(Status::permission_denied(
            "relay control is loopback-only: refused for a request whose client address is unknown",
        )),
    }
}

impl V1Grpc {
    #[allow(clippy::result_large_err)]
    async fn relay_post<Req, Resp>(
        &self,
        path: &str,
        request: GrpcRequest<Req>,
    ) -> Result<GrpcResponse<Resp>, Status>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let (ctx, inner) = split(request);
        require_loopback_peer(&ctx)?;
        into_response(self.post(&ctx, path, &inner).await?)
    }

    #[allow(clippy::result_large_err)]
    async fn relay_get<Req, Resp>(
        &self,
        path: &str,
        request: GrpcRequest<Req>,
    ) -> Result<GrpcResponse<Resp>, Status>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let (ctx, inner) = split(request);
        require_loopback_peer(&ctx)?;
        into_response(self.get(&ctx, path, &inner).await?)
    }
}

impl V1Grpc {
    async fn special_connect_relay(
        &self,
        request: GrpcRequest<pb::ConnectRelayRequest>,
    ) -> Result<GrpcResponse<pb::ConnectRelayResponse>, Status> {
        self.relay_post("/v1/relay/connect", request).await
    }

    async fn special_disconnect_relay(
        &self,
        request: GrpcRequest<pb::DisconnectRelayRequest>,
    ) -> Result<GrpcResponse<pb::RelayStatus>, Status> {
        self.relay_post("/v1/relay/disconnect", request).await
    }

    async fn special_get_relay_status(
        &self,
        request: GrpcRequest<pb::GetRelayStatusRequest>,
    ) -> Result<GrpcResponse<pb::RelayStatus>, Status> {
        self.relay_get("/v1/relay/status", request).await
    }

    async fn special_get_relay_link(
        &self,
        request: GrpcRequest<pb::GetRelayLinkRequest>,
    ) -> Result<GrpcResponse<pb::RelayLinkResponse>, Status> {
        self.relay_get("/v1/relay/link", request).await
    }

    async fn special_rotate_relay_key(
        &self,
        request: GrpcRequest<pb::RotateRelayKeyRequest>,
    ) -> Result<GrpcResponse<pb::RelayLinkResponse>, Status> {
        self.relay_post("/v1/relay/rotate", request).await
    }
}

include!("grpc_generated.rs");
