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

use crate::AppState;

/// Shared state + router used by every generated service impl.
#[derive(Clone)]
pub struct V1Grpc {
    state: crate::ServerState,
    router: axum::Router,
}

impl V1Grpc {
    /// Build the binding over the same application state and `/v1` router.
    #[must_use]
    pub fn new(app: AppState) -> Self {
        let router = crate::router(app.clone());
        Self {
            state: crate::ServerState::new(app),
            router,
        }
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
        let tonic_code = match code.as_str() {
            "invalid_argument" | "bundle_api_bad_request" => tonic::Code::InvalidArgument,
            "not_found" | "session_not_found" | "bundle_api_not_found" => tonic::Code::NotFound,
            "bundle_api_method_not_allowed" => tonic::Code::Unimplemented,
            "permission_denied" => tonic::Code::PermissionDenied,
            "session_busy" | "conflict" => tonic::Code::FailedPrecondition,
            "unavailable" | "bundle_api_failed" => tonic::Code::Unavailable,
            _ => tonic::Code::Internal,
        };
        let _ = status;
        Status::new(tonic_code, message)
    }

    #[allow(clippy::result_large_err)]
    async fn dispatch<Req, Resp>(
        &self,
        method: &str,
        path: &str,
        query: BTreeMap<String, String>,
        request: &Req,
    ) -> Result<Resp, Status>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let mut uri = String::from(path);
        for (index, (key, value)) in query.iter().enumerate() {
            uri.push(if index == 0 { '?' } else { '&' });
            uri.push_str(&format!("{}={}", encode(key), encode(value)));
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
        let http_request = builder
            .body(Body::from(body))
            .map_err(|error| Status::internal(format!("build request: {error}")))?;
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
    async fn get<Req, Resp>(&self, path: &str, request: &Req) -> Result<Resp, Status>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let query = query_of(request);
        self.dispatch("GET", path, query, request).await
    }

    #[allow(clippy::result_large_err)]
    async fn post<Req, Resp>(&self, path: &str, request: &Req) -> Result<Resp, Status>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        self.dispatch::<Req, Resp>("POST", path, BTreeMap::new(), request)
            .await
    }
}

fn body_len(body: &[u8]) -> String {
    body.len().to_string()
}

/// Serialize a request into non-empty query parameters.
fn query_of<Req: Serialize>(request: &Req) -> BTreeMap<String, String> {
    let mut query = BTreeMap::new();
    let Ok(Value::Object(map)) = serde_json::to_value(request) else {
        return query;
    };
    for (key, value) in map {
        let text = match value {
            Value::String(text) => text,
            Value::Number(number) => number.to_string(),
            Value::Bool(flag) => flag.to_string(),
            _ => continue,
        };
        if !text.is_empty() && text != "0" && text != "false" {
            query.insert(key, text);
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

macro_rules! unary {
    ($self:expr, $method:expr, $path:expr, $request:expr) => {
        into_response(
            $self
                .dispatch::<_, _>($method, $path, BTreeMap::new(), &$request.into_inner())
                .await?,
        )
    };
}

macro_rules! get_rpc {
    ($self:expr, $path:expr, $request:expr) => {
        into_response($self.get($path, &$request.into_inner()).await?)
    };
}

// ---------------------------------------------------------------------------
// Process
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::process_server::Process for V1Grpc {
    async fn get_health(
        &self,
        request: GrpcRequest<pb::GetHealthRequest>,
    ) -> Result<GrpcResponse<pb::GetHealthResponse>, Status> {
        get_rpc!(self, "/v1/health", request)
    }

    async fn get_location(
        &self,
        request: GrpcRequest<pb::GetLocationRequest>,
    ) -> Result<GrpcResponse<pb::LocationInfo>, Status> {
        get_rpc!(self, "/v1/location", request)
    }

    async fn get_config(
        &self,
        request: GrpcRequest<pb::GetConfigRequest>,
    ) -> Result<GrpcResponse<pb::GetConfigResponse>, Status> {
        get_rpc!(self, "/v1/config", request)
    }

    async fn update_config(
        &self,
        request: GrpcRequest<pb::UpdateConfigRequest>,
    ) -> Result<GrpcResponse<pb::GetConfigResponse>, Status> {
        unary!(self, "PATCH", "/v1/config", request)
    }

    async fn dispose_process(
        &self,
        request: GrpcRequest<pb::DisposeProcessRequest>,
    ) -> Result<GrpcResponse<pb::DisposeProcessResponse>, Status> {
        unary!(self, "POST", "/v1/process/dispose", request)
    }

    async fn upgrade_process(
        &self,
        request: GrpcRequest<pb::UpgradeProcessRequest>,
    ) -> Result<GrpcResponse<pb::UpgradeProcessResponse>, Status> {
        unary!(self, "POST", "/v1/process/upgrade", request)
    }

    async fn get_bootstrap(
        &self,
        request: GrpcRequest<pb::GetBootstrapRequest>,
    ) -> Result<GrpcResponse<pb::Bootstrap>, Status> {
        get_rpc!(self, "/v1/bootstrap", request)
    }
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::catalog_server::Catalog for V1Grpc {
    async fn list_agents(
        &self,
        request: GrpcRequest<pb::ListAgentsRequest>,
    ) -> Result<GrpcResponse<pb::ListAgentsResponse>, Status> {
        get_rpc!(self, "/v1/agents", request)
    }

    async fn list_models(
        &self,
        request: GrpcRequest<pb::ListModelsRequest>,
    ) -> Result<GrpcResponse<pb::ListModelsResponse>, Status> {
        get_rpc!(self, "/v1/models", request)
    }

    async fn list_providers(
        &self,
        request: GrpcRequest<pb::ListProvidersRequest>,
    ) -> Result<GrpcResponse<pb::ListProvidersResponse>, Status> {
        get_rpc!(self, "/v1/providers", request)
    }

    async fn get_provider(
        &self,
        request: GrpcRequest<pb::GetProviderRequest>,
    ) -> Result<GrpcResponse<pb::ProviderInfo>, Status> {
        let provider_id = encode(&field(&request.into_inner(), "providerId"));
        into_response(
            self.get(
                &format!("/v1/providers/{provider_id}"),
                &pb::GetProviderRequest::default(),
            )
            .await?,
        )
    }

    async fn upsert_provider(
        &self,
        request: GrpcRequest<pb::UpsertProviderRequest>,
    ) -> Result<GrpcResponse<pb::ProviderUpdate>, Status> {
        let inner = request.into_inner();
        let provider_id = encode(&field(&inner, "providerId"));
        into_response(
            self.dispatch::<_, _>(
                "PUT",
                &format!("/v1/providers/{provider_id}"),
                BTreeMap::new(),
                &inner,
            )
            .await?,
        )
    }

    async fn refresh_provider(
        &self,
        request: GrpcRequest<pb::RefreshProviderRequest>,
    ) -> Result<GrpcResponse<pb::ProviderUpdate>, Status> {
        let inner = request.into_inner();
        let provider_id = encode(&field(&inner, "providerId"));
        into_response(
            self.post(&format!("/v1/providers/{provider_id}/refresh"), &inner)
                .await?,
        )
    }

    async fn set_provider_model(
        &self,
        request: GrpcRequest<pb::SetProviderModelRequest>,
    ) -> Result<GrpcResponse<pb::ProviderUpdate>, Status> {
        let inner = request.into_inner();
        let provider_id = encode(&field(&inner, "providerId"));
        into_response(
            self.dispatch::<_, _>(
                "PUT",
                &format!("/v1/providers/{provider_id}/models"),
                BTreeMap::new(),
                &inner,
            )
            .await?,
        )
    }

    async fn remove_provider_model(
        &self,
        request: GrpcRequest<pb::RemoveProviderModelRequest>,
    ) -> Result<GrpcResponse<pb::ProviderUpdate>, Status> {
        let inner = request.into_inner();
        let provider_id = encode(&field(&inner, "providerId"));
        let query = BTreeMap::from([("modelId".to_owned(), field(&inner, "modelId"))]);
        into_response(
            self.dispatch::<_, _>(
                "DELETE",
                &format!("/v1/providers/{provider_id}/models"),
                query,
                &pb::RemoveProviderModelRequest::default(),
            )
            .await?,
        )
    }

    async fn test_provider_model(
        &self,
        request: GrpcRequest<pb::TestProviderModelRequest>,
    ) -> Result<GrpcResponse<pb::TestProviderModelResponse>, Status> {
        let inner = request.into_inner();
        let provider_id = encode(&field(&inner, "providerId"));
        into_response(
            self.post(&format!("/v1/providers/{provider_id}/test"), &inner)
                .await?,
        )
    }

    async fn list_commands(
        &self,
        request: GrpcRequest<pb::ListCommandsRequest>,
    ) -> Result<GrpcResponse<pb::ListCommandsResponse>, Status> {
        get_rpc!(self, "/v1/commands", request)
    }

    async fn list_skills(
        &self,
        request: GrpcRequest<pb::ListSkillsRequest>,
    ) -> Result<GrpcResponse<pb::ListSkillsResponse>, Status> {
        get_rpc!(self, "/v1/skills", request)
    }

    async fn list_tools(
        &self,
        request: GrpcRequest<pb::ListToolsRequest>,
    ) -> Result<GrpcResponse<pb::ListToolsResponse>, Status> {
        get_rpc!(self, "/v1/tools", request)
    }

    async fn list_permission_modes(
        &self,
        request: GrpcRequest<pb::ListPermissionModesRequest>,
    ) -> Result<GrpcResponse<pb::ListPermissionModesResponse>, Status> {
        get_rpc!(self, "/v1/permission-modes", request)
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::auth_server::Auth for V1Grpc {
    async fn list_provider_auth(
        &self,
        request: GrpcRequest<pb::ListProviderAuthRequest>,
    ) -> Result<GrpcResponse<pb::ListProviderAuthResponse>, Status> {
        get_rpc!(self, "/v1/auth", request)
    }

    async fn set_provider_auth(
        &self,
        request: GrpcRequest<pb::SetProviderAuthRequest>,
    ) -> Result<GrpcResponse<pb::SetProviderAuthResponse>, Status> {
        let inner = request.into_inner();
        let provider_id = encode(&field(&inner, "providerId"));
        into_response(
            self.dispatch::<_, _>(
                "PUT",
                &format!("/v1/auth/{provider_id}"),
                BTreeMap::new(),
                &inner,
            )
            .await?,
        )
    }

    async fn remove_provider_auth(
        &self,
        request: GrpcRequest<pb::RemoveProviderAuthRequest>,
    ) -> Result<GrpcResponse<pb::RemoveProviderAuthResponse>, Status> {
        let provider_id = encode(&field(&request.into_inner(), "providerId"));
        let empty = pb::RemoveProviderAuthRequest::default();
        into_response(
            self.dispatch::<_, _>(
                "DELETE",
                &format!("/v1/auth/{provider_id}"),
                BTreeMap::new(),
                &empty,
            )
            .await?,
        )
    }

    async fn start_oauth(
        &self,
        request: GrpcRequest<pb::StartOauthRequest>,
    ) -> Result<GrpcResponse<pb::StartOauthResponse>, Status> {
        let provider_id = field(&request.into_inner(), "providerId");
        into_response(
            self.post(
                &format!("/v1/auth/{provider_id}/oauth/start"),
                &pb::StartOauthRequest::default(),
            )
            .await?,
        )
    }

    async fn complete_oauth(
        &self,
        request: GrpcRequest<pb::CompleteOauthRequest>,
    ) -> Result<GrpcResponse<pb::CompleteOauthResponse>, Status> {
        let inner = request.into_inner();
        let provider_id = field(&inner, "providerId");
        into_response(
            self.post(&format!("/v1/auth/{provider_id}/oauth/callback"), &inner)
                .await?,
        )
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::session_server::Session for V1Grpc {
    async fn create_session(
        &self,
        request: GrpcRequest<pb::CreateSessionRequest>,
    ) -> Result<GrpcResponse<pb::CreateSessionResponse>, Status> {
        unary!(self, "POST", "/v1/sessions", request)
    }

    async fn get_session(
        &self,
        request: GrpcRequest<pb::GetSessionRequest>,
    ) -> Result<GrpcResponse<pb::SessionInfo>, Status> {
        let session = field(&request.into_inner(), "session");
        into_response(
            self.get(
                &format!("/v1/sessions/{session}"),
                &pb::GetSessionRequest::default(),
            )
            .await?,
        )
    }

    async fn list_sessions(
        &self,
        request: GrpcRequest<pb::ListSessionsRequest>,
    ) -> Result<GrpcResponse<pb::ListSessionsResponse>, Status> {
        get_rpc!(self, "/v1/sessions", request)
    }

    async fn update_session(
        &self,
        request: GrpcRequest<pb::UpdateSessionRequest>,
    ) -> Result<GrpcResponse<pb::SessionInfo>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        into_response(
            self.dispatch::<_, _>(
                "PATCH",
                &format!("/v1/sessions/{session}"),
                BTreeMap::new(),
                &inner,
            )
            .await?,
        )
    }

    async fn delete_session(
        &self,
        request: GrpcRequest<pb::DeleteSessionRequest>,
    ) -> Result<GrpcResponse<pb::DeleteSessionResponse>, Status> {
        let session = field(&request.into_inner(), "session");
        into_response(
            self.dispatch::<_, _>(
                "DELETE",
                &format!("/v1/sessions/{session}"),
                BTreeMap::new(),
                &pb::DeleteSessionRequest::default(),
            )
            .await?,
        )
    }

    async fn fork_session(
        &self,
        request: GrpcRequest<pb::ForkSessionRequest>,
    ) -> Result<GrpcResponse<pb::ForkSessionResponse>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        into_response(
            self.post(&format!("/v1/sessions/{session}/fork"), &inner)
                .await?,
        )
    }

    async fn compact_session(
        &self,
        request: GrpcRequest<pb::CompactSessionRequest>,
    ) -> Result<GrpcResponse<pb::CompactSessionResponse>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        into_response(
            self.post(&format!("/v1/sessions/{session}/compact"), &inner)
                .await?,
        )
    }

    async fn summarize_session(
        &self,
        request: GrpcRequest<pb::SummarizeSessionRequest>,
    ) -> Result<GrpcResponse<pb::SummarizeSessionResponse>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        into_response(
            self.post(&format!("/v1/sessions/{session}/summarize"), &inner)
                .await?,
        )
    }

    async fn revert_session(
        &self,
        request: GrpcRequest<pb::RevertSessionRequest>,
    ) -> Result<GrpcResponse<pb::RevertSessionResponse>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        into_response(
            self.post(&format!("/v1/sessions/{session}/revert"), &inner)
                .await?,
        )
    }
}

// ---------------------------------------------------------------------------
// BundleApi
// ---------------------------------------------------------------------------

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

#[tonic::async_trait]
impl pb::bundle_api_server::BundleApi for V1Grpc {
    async fn list_bundle_apis(
        &self,
        request: GrpcRequest<pb::ListBundleApisRequest>,
    ) -> Result<GrpcResponse<pb::ListBundleApisResponse>, Status> {
        unary!(self, "GET", "/v1/bundle-apis", request)
    }

    async fn invoke_session_bundle_api(
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

    async fn invoke_global_bundle_api(
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

// ---------------------------------------------------------------------------
// Turn
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::turn_server::Turn for V1Grpc {
    async fn create_turn(
        &self,
        request: GrpcRequest<pb::CreateTurnRequest>,
    ) -> Result<GrpcResponse<pb::CreateTurnResponse>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        into_response(
            self.post(&format!("/v1/sessions/{session}/turns"), &inner)
                .await?,
        )
    }

    async fn get_turn(
        &self,
        request: GrpcRequest<pb::GetTurnRequest>,
    ) -> Result<GrpcResponse<pb::TurnInfo>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        let turn = field(&inner, "turn");
        into_response(
            self.get(
                &format!("/v1/sessions/{session}/turns/{turn}"),
                &pb::GetTurnRequest::default(),
            )
            .await?,
        )
    }

    async fn wait_turn(
        &self,
        request: GrpcRequest<pb::WaitTurnRequest>,
    ) -> Result<GrpcResponse<pb::TurnInfo>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        let turn = field(&inner, "turn");
        let timeout = field(&inner, "timeoutMs");
        into_response(
            self.post(
                &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs={timeout}"),
                &pb::WaitTurnRequest::default(),
            )
            .await?,
        )
    }

    async fn cancel_turn(
        &self,
        request: GrpcRequest<pb::CancelTurnRequest>,
    ) -> Result<GrpcResponse<pb::TurnInfo>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        let turn = field(&inner, "turn");
        into_response(
            self.post(
                &format!("/v1/sessions/{session}/turns/{turn}/cancel"),
                &pb::CancelTurnRequest::default(),
            )
            .await?,
        )
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::messages_server::Messages for V1Grpc {
    async fn list_messages(
        &self,
        request: GrpcRequest<pb::ListMessagesRequest>,
    ) -> Result<GrpcResponse<pb::ListMessagesResponse>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        into_response(
            self.get(&format!("/v1/sessions/{session}/messages"), &inner)
                .await?,
        )
    }

    async fn get_message(
        &self,
        request: GrpcRequest<pb::GetMessageRequest>,
    ) -> Result<GrpcResponse<pb::MessageInfo>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        let message = field(&inner, "message");
        into_response(
            self.get(
                &format!("/v1/sessions/{session}/messages/{message}"),
                &pb::GetMessageRequest::default(),
            )
            .await?,
        )
    }

    async fn delete_message_part(
        &self,
        request: GrpcRequest<pb::DeleteMessagePartRequest>,
    ) -> Result<GrpcResponse<pb::DeleteMessagePartResponse>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        let message = field(&inner, "message");
        let part = field(&inner, "part");
        into_response(
            self.dispatch::<_, _>(
                "DELETE",
                &format!("/v1/sessions/{session}/messages/{message}/parts/{part}"),
                BTreeMap::new(),
                &pb::DeleteMessagePartRequest::default(),
            )
            .await?,
        )
    }

    async fn get_session_todo(
        &self,
        request: GrpcRequest<pb::GetSessionTodoRequest>,
    ) -> Result<GrpcResponse<pb::TodoList>, Status> {
        let session = field(&request.into_inner(), "session");
        into_response(
            self.get(
                &format!("/v1/sessions/{session}/todo"),
                &pb::GetSessionTodoRequest::default(),
            )
            .await?,
        )
    }
}

// ---------------------------------------------------------------------------
// Events (unary replay + direct streaming)
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::events_server::Events for V1Grpc {
    async fn list_events(
        &self,
        request: GrpcRequest<pb::ListEventsRequest>,
    ) -> Result<GrpcResponse<pb::ListEventsResponse>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        into_response(
            self.get(&format!("/v1/sessions/{session}/events"), &inner)
                .await?,
        )
    }

    type StreamSessionEventsStream =
        Pin<Box<dyn Stream<Item = Result<pb::StreamFrame, Status>> + Send>>;

    async fn stream_session_events(
        &self,
        request: GrpcRequest<pb::StreamSessionEventsRequest>,
    ) -> Result<GrpcResponse<Self::StreamSessionEventsStream>, Status> {
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
        Ok(GrpcResponse::new(Box::pin(super::events::frame_stream(
            self.state.clone(),
            super::events::StreamScope::session(session, inner.include_descendants),
            inner.since_seq,
        ))))
    }

    type StreamGlobalEventsStream =
        Pin<Box<dyn Stream<Item = Result<pb::StreamFrame, Status>> + Send>>;

    async fn stream_global_events(
        &self,
        request: GrpcRequest<pb::StreamGlobalEventsRequest>,
    ) -> Result<GrpcResponse<Self::StreamGlobalEventsStream>, Status> {
        let inner = request.into_inner();
        Ok(GrpcResponse::new(Box::pin(super::events::frame_stream(
            self.state.clone(),
            super::events::StreamScope::Global,
            inner.since_seq,
        ))))
    }
}

// ---------------------------------------------------------------------------
// Interactions
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::interactions_server::Interactions for V1Grpc {
    async fn list_interactions(
        &self,
        request: GrpcRequest<pb::ListInteractionsRequest>,
    ) -> Result<GrpcResponse<pb::ListInteractionsResponse>, Status> {
        get_rpc!(self, "/v1/interactions", request)
    }

    async fn respond_interaction(
        &self,
        request: GrpcRequest<pb::RespondInteractionRequest>,
    ) -> Result<GrpcResponse<pb::RespondInteractionResponse>, Status> {
        let inner = request.into_inner();
        let id = field(&inner, "request");
        into_response(
            self.post(&format!("/v1/interactions/{id}/respond"), &inner)
                .await?,
        )
    }

    async fn list_saved_rules(
        &self,
        request: GrpcRequest<pb::ListSavedRulesRequest>,
    ) -> Result<GrpcResponse<pb::ListSavedRulesResponse>, Status> {
        get_rpc!(self, "/v1/permissions/rules", request)
    }

    async fn delete_saved_rule(
        &self,
        request: GrpcRequest<pb::DeleteSavedRuleRequest>,
    ) -> Result<GrpcResponse<pb::DeleteSavedRuleResponse>, Status> {
        let rule = field(&request.into_inner(), "rule");
        into_response(
            self.dispatch::<_, _>(
                "DELETE",
                &format!("/v1/permissions/rules/{rule}"),
                BTreeMap::new(),
                &pb::DeleteSavedRuleRequest::default(),
            )
            .await?,
        )
    }
}

// ---------------------------------------------------------------------------
// Workflow
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::workflow_server::Workflow for V1Grpc {
    async fn list_workflows(
        &self,
        request: GrpcRequest<pb::ListWorkflowsRequest>,
    ) -> Result<GrpcResponse<pb::ListWorkflowsResponse>, Status> {
        get_rpc!(self, "/v1/workflows", request)
    }

    async fn get_workflow_state(
        &self,
        request: GrpcRequest<pb::GetWorkflowStateRequest>,
    ) -> Result<GrpcResponse<pb::WorkflowState>, Status> {
        let session = field(&request.into_inner(), "session");
        into_response(
            self.get(
                &format!("/v1/sessions/{session}/workflow"),
                &pb::GetWorkflowStateRequest::default(),
            )
            .await?,
        )
    }

    async fn submit_workflow_command(
        &self,
        request: GrpcRequest<pb::SubmitWorkflowCommandRequest>,
    ) -> Result<GrpcResponse<pb::SubmitWorkflowCommandResponse>, Status> {
        let inner = request.into_inner();
        let session = field(&inner, "session");
        into_response(
            self.post(&format!("/v1/sessions/{session}/workflow"), &inner)
                .await?,
        )
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::files_server::Files for V1Grpc {
    async fn read_file(
        &self,
        request: GrpcRequest<pb::ReadFileRequest>,
    ) -> Result<GrpcResponse<pb::ReadFileResponse>, Status> {
        get_rpc!(self, "/v1/fs/read", request)
    }

    async fn list_directory(
        &self,
        request: GrpcRequest<pb::ListDirectoryRequest>,
    ) -> Result<GrpcResponse<pb::ListDirectoryResponse>, Status> {
        get_rpc!(self, "/v1/fs/list", request)
    }

    async fn find_files(
        &self,
        request: GrpcRequest<pb::FindFilesRequest>,
    ) -> Result<GrpcResponse<pb::FindFilesResponse>, Status> {
        get_rpc!(self, "/v1/fs/find", request)
    }

    async fn search_text(
        &self,
        request: GrpcRequest<pb::SearchTextRequest>,
    ) -> Result<GrpcResponse<pb::SearchTextResponse>, Status> {
        get_rpc!(self, "/v1/fs/search", request)
    }

    async fn search_symbols(
        &self,
        request: GrpcRequest<pb::SearchSymbolsRequest>,
    ) -> Result<GrpcResponse<pb::SearchSymbolsResponse>, Status> {
        get_rpc!(self, "/v1/fs/symbols", request)
    }
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::project_server::Project for V1Grpc {
    async fn list_projects(
        &self,
        request: GrpcRequest<pb::ListProjectsRequest>,
    ) -> Result<GrpcResponse<pb::ListProjectsResponse>, Status> {
        get_rpc!(self, "/v1/projects", request)
    }

    async fn get_current_project(
        &self,
        request: GrpcRequest<pb::GetCurrentProjectRequest>,
    ) -> Result<GrpcResponse<pb::ProjectInfo>, Status> {
        get_rpc!(self, "/v1/projects/current", request)
    }

    async fn update_project(
        &self,
        request: GrpcRequest<pb::UpdateProjectRequest>,
    ) -> Result<GrpcResponse<pb::ProjectInfo>, Status> {
        let inner = request.into_inner();
        let project = field(&inner, "project");
        into_response(
            self.dispatch::<_, _>(
                "PATCH",
                &format!("/v1/projects/{project}"),
                BTreeMap::new(),
                &inner,
            )
            .await?,
        )
    }

    async fn list_project_directories(
        &self,
        request: GrpcRequest<pb::ListProjectDirectoriesRequest>,
    ) -> Result<GrpcResponse<pb::ListProjectDirectoriesResponse>, Status> {
        let project = field(&request.into_inner(), "project");
        into_response(
            self.get(
                &format!("/v1/projects/{project}/directories"),
                &pb::ListProjectDirectoriesRequest::default(),
            )
            .await?,
        )
    }

    async fn init_project_git(
        &self,
        request: GrpcRequest<pb::InitProjectGitRequest>,
    ) -> Result<GrpcResponse<pb::InitProjectGitResponse>, Status> {
        let project = field(&request.into_inner(), "project");
        into_response(
            self.post(
                &format!("/v1/projects/{project}/init-git"),
                &pb::InitProjectGitRequest::default(),
            )
            .await?,
        )
    }

    async fn get_vcs_status(
        &self,
        request: GrpcRequest<pb::GetVcsStatusRequest>,
    ) -> Result<GrpcResponse<pb::VcsStatus>, Status> {
        get_rpc!(self, "/v1/vcs", request)
    }

    async fn get_vcs_diff(
        &self,
        request: GrpcRequest<pb::GetVcsDiffRequest>,
    ) -> Result<GrpcResponse<pb::GetVcsDiffResponse>, Status> {
        get_rpc!(self, "/v1/vcs/diff", request)
    }

    async fn apply_patch(
        &self,
        request: GrpcRequest<pb::ApplyPatchRequest>,
    ) -> Result<GrpcResponse<pb::ApplyPatchResponse>, Status> {
        unary!(self, "POST", "/v1/vcs/apply", request)
    }
}

// ---------------------------------------------------------------------------
// Worktrees
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::worktrees_server::Worktrees for V1Grpc {
    async fn list_worktrees(
        &self,
        request: GrpcRequest<pb::ListWorktreesRequest>,
    ) -> Result<GrpcResponse<pb::ListWorktreesResponse>, Status> {
        get_rpc!(self, "/v1/worktrees", request)
    }

    async fn create_worktree(
        &self,
        request: GrpcRequest<pb::CreateWorktreeRequest>,
    ) -> Result<GrpcResponse<pb::Worktree>, Status> {
        unary!(self, "POST", "/v1/worktrees", request)
    }

    async fn delete_worktree(
        &self,
        request: GrpcRequest<pb::DeleteWorktreeRequest>,
    ) -> Result<GrpcResponse<pb::DeleteWorktreeResponse>, Status> {
        let worktree = field(&request.into_inner(), "worktree");
        into_response(
            self.dispatch::<_, _>(
                "DELETE",
                &format!("/v1/worktrees/{worktree}"),
                BTreeMap::new(),
                &pb::DeleteWorktreeRequest::default(),
            )
            .await?,
        )
    }

    async fn reset_worktree(
        &self,
        request: GrpcRequest<pb::ResetWorktreeRequest>,
    ) -> Result<GrpcResponse<pb::Worktree>, Status> {
        let worktree = field(&request.into_inner(), "worktree");
        into_response(
            self.post(
                &format!("/v1/worktrees/{worktree}/reset"),
                &pb::ResetWorktreeRequest::default(),
            )
            .await?,
        )
    }
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::mcp_server::Mcp for V1Grpc {
    async fn get_mcp_status(
        &self,
        request: GrpcRequest<pb::GetMcpStatusRequest>,
    ) -> Result<GrpcResponse<pb::GetMcpStatusResponse>, Status> {
        get_rpc!(self, "/v1/mcp", request)
    }

    async fn add_mcp_server(
        &self,
        request: GrpcRequest<pb::AddMcpServerRequest>,
    ) -> Result<GrpcResponse<pb::McpServerStatus>, Status> {
        unary!(self, "POST", "/v1/mcp", request)
    }

    async fn connect_mcp(
        &self,
        request: GrpcRequest<pb::ConnectMcpRequest>,
    ) -> Result<GrpcResponse<pb::McpServerStatus>, Status> {
        let name = field(&request.into_inner(), "name");
        into_response(
            self.post(
                &format!("/v1/mcp/{name}/connect"),
                &pb::ConnectMcpRequest::default(),
            )
            .await?,
        )
    }

    async fn disconnect_mcp(
        &self,
        request: GrpcRequest<pb::DisconnectMcpRequest>,
    ) -> Result<GrpcResponse<pb::McpServerStatus>, Status> {
        let name = field(&request.into_inner(), "name");
        into_response(
            self.post(
                &format!("/v1/mcp/{name}/disconnect"),
                &pb::DisconnectMcpRequest::default(),
            )
            .await?,
        )
    }

    async fn start_mcp_auth(
        &self,
        request: GrpcRequest<pb::StartMcpAuthRequest>,
    ) -> Result<GrpcResponse<pb::StartMcpAuthResponse>, Status> {
        let name = field(&request.into_inner(), "name");
        into_response(
            self.post(
                &format!("/v1/mcp/{name}/auth"),
                &pb::StartMcpAuthRequest::default(),
            )
            .await?,
        )
    }

    async fn complete_mcp_auth(
        &self,
        request: GrpcRequest<pb::CompleteMcpAuthRequest>,
    ) -> Result<GrpcResponse<pb::McpServerStatus>, Status> {
        let inner = request.into_inner();
        let name = field(&inner, "name");
        into_response(
            self.post(&format!("/v1/mcp/{name}/auth/complete"), &inner)
                .await?,
        )
    }

    async fn remove_mcp_auth(
        &self,
        request: GrpcRequest<pb::RemoveMcpAuthRequest>,
    ) -> Result<GrpcResponse<pb::RemoveMcpAuthResponse>, Status> {
        let name = field(&request.into_inner(), "name");
        into_response(
            self.dispatch::<_, _>(
                "DELETE",
                &format!("/v1/mcp/{name}/auth"),
                BTreeMap::new(),
                &pb::RemoveMcpAuthRequest::default(),
            )
            .await?,
        )
    }
}

// ---------------------------------------------------------------------------
// Pty (unary subset over the dispatcher; StreamPty rides the PTY runtime)
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::pty_server::Pty for V1Grpc {
    async fn list_shells(
        &self,
        request: GrpcRequest<pb::ListShellsRequest>,
    ) -> Result<GrpcResponse<pb::ListShellsResponse>, Status> {
        get_rpc!(self, "/v1/pty/shells", request)
    }

    async fn create_pty(
        &self,
        request: GrpcRequest<pb::CreatePtyRequest>,
    ) -> Result<GrpcResponse<pb::PtySession>, Status> {
        unary!(self, "POST", "/v1/pty", request)
    }

    async fn get_pty(
        &self,
        request: GrpcRequest<pb::GetPtyRequest>,
    ) -> Result<GrpcResponse<pb::PtySession>, Status> {
        let id = field(&request.into_inner(), "id");
        into_response(
            self.get(&format!("/v1/pty/{id}"), &pb::GetPtyRequest::default())
                .await?,
        )
    }

    async fn update_pty(
        &self,
        request: GrpcRequest<pb::UpdatePtyRequest>,
    ) -> Result<GrpcResponse<pb::PtySession>, Status> {
        let inner = request.into_inner();
        let id = field(&inner, "id");
        into_response(
            self.dispatch::<_, _>("PUT", &format!("/v1/pty/{id}"), BTreeMap::new(), &inner)
                .await?,
        )
    }

    async fn delete_pty(
        &self,
        request: GrpcRequest<pb::DeletePtyRequest>,
    ) -> Result<GrpcResponse<pb::DeletePtyResponse>, Status> {
        let id = field(&request.into_inner(), "id");
        into_response(
            self.dispatch::<_, _>(
                "DELETE",
                &format!("/v1/pty/{id}"),
                BTreeMap::new(),
                &pb::DeletePtyRequest::default(),
            )
            .await?,
        )
    }

    async fn create_connect_token(
        &self,
        request: GrpcRequest<pb::CreateConnectTokenRequest>,
    ) -> Result<GrpcResponse<pb::CreateConnectTokenResponse>, Status> {
        let id = field(&request.into_inner(), "id");
        into_response(
            self.post(
                &format!("/v1/pty/{id}/connect-token"),
                &pb::CreateConnectTokenRequest::default(),
            )
            .await?,
        )
    }

    type StreamPtyStream = Pin<Box<dyn Stream<Item = Result<pb::PtyServerFrame, Status>> + Send>>;

    async fn stream_pty(
        &self,
        request: GrpcRequest<tonic::Streaming<pb::PtyClientFrame>>,
    ) -> Result<GrpcResponse<Self::StreamPtyStream>, Status> {
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
// Logs
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::logs_server::Logs for V1Grpc {
    async fn ingest_log(
        &self,
        request: GrpcRequest<pb::IngestLogRequest>,
    ) -> Result<GrpcResponse<pb::IngestLogResponse>, Status> {
        unary!(self, "POST", "/v1/logs", request)
    }
}

// ---------------------------------------------------------------------------
// AgentModels
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::agent_models_server::AgentModels for V1Grpc {
    async fn list_agent_models(
        &self,
        request: GrpcRequest<pb::ListAgentModelsRequest>,
    ) -> Result<GrpcResponse<pb::ListAgentModelsResponse>, Status> {
        get_rpc!(self, "/v1/agent-models", request)
    }

    async fn set_agent_model(
        &self,
        request: GrpcRequest<pb::SetAgentModelRequest>,
    ) -> Result<GrpcResponse<pb::AgentModelState>, Status> {
        let inner = request.into_inner();
        let agent_id = field(&inner, "agentId");
        into_response(
            self.dispatch::<_, _>(
                "PUT",
                &format!("/v1/agent-models/{agent_id}"),
                BTreeMap::new(),
                &inner,
            )
            .await?,
        )
    }
}
