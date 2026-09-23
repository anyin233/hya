//! `/v1` bundle-registered endpoints: discovery plus the session-scoped and
//! global passthrough routes.
//!
//! A bundle with an explicit `extensions.process` may register its own HTTP
//! endpoints (manifest `apis:`). The engine resolves the bundle in the live
//! runtime generation, routes the concrete path against the bundle's
//! templates, and forwards the request to its process. HTTP answers with the
//! process's own status and JSON body verbatim; the gRPC binding (which shares
//! [`invoke`]) wraps them in `BundleApiResponse`.

use std::collections::BTreeMap;

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};
use axum::{Json, Router};
use hya_api::error::Code;
use hya_api::v1 as pb;
use hya_core::{
    ApiMethod, BundleApiCall, BundleApiError, BundleApiOutcome, MAX_BUNDLE_API_BODY_BYTES,
};
use hya_proto::SessionId;
use serde_json::Value;

use crate::ServerState;

use super::V1Error;
use super::session::parse_session;

/// The only media type bundle endpoints produce.
pub(crate) const BUNDLE_API_CONTENT_TYPE: &str = "application/json";

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/bundle-apis", get(list_bundle_apis))
        .route(
            "/v1/sessions/:id/bundles/:bundle/*path",
            every_method(invoke_session_http),
        )
        .route(
            "/v1/bundles/:bundle/api/*path",
            every_method(invoke_global_http),
        )
}

/// Bind one handler to the five methods a bundle endpoint may declare; any
/// other method gets axum's plain `405`.
fn every_method<H, T>(handler: H) -> MethodRouter<ServerState>
where
    H: axum::handler::Handler<T, ServerState> + Clone,
    T: 'static,
{
    get(handler.clone())
        .post(handler.clone())
        .put(handler.clone())
        .patch(handler.clone())
        .delete(handler)
}

impl From<BundleApiError> for V1Error {
    fn from(error: BundleApiError) -> Self {
        let code = match &error {
            BundleApiError::SessionNotFound(_) => Code::SessionNotFound,
            BundleApiError::NotFound { .. } => Code::BundleApiNotFound,
            BundleApiError::MethodNotAllowed { .. } => Code::BundleApiMethodNotAllowed,
            BundleApiError::BadRequest(_) => Code::BundleApiBadRequest,
            BundleApiError::Failed { .. } => Code::BundleApiFailed,
            BundleApiError::Core(_) => Code::Internal,
        };
        Self::new(code, error.to_string())
    }
}

/// Render a bundle API failure; a `405` also lists the allowed methods in
/// the `Allow` header.
fn error_response(error: BundleApiError) -> Response {
    let allow = match &error {
        BundleApiError::MethodNotAllowed { allow, .. } => Some(
            allow
                .iter()
                .map(|method| method.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => None,
    };
    let mut response = V1Error::from(error).into_response();
    if let Some(allow) = allow.and_then(|allow| HeaderValue::from_str(&allow).ok()) {
        response.headers_mut().insert(header::ALLOW, allow);
    }
    response
}

fn bad_request(message: impl Into<String>) -> Response {
    error_response(BundleApiError::BadRequest(message.into()))
}

/// Serve one call through the engine; shared by HTTP and gRPC.
pub(crate) async fn invoke(
    st: &ServerState,
    call: BundleApiCall,
) -> Result<BundleApiOutcome, BundleApiError> {
    st.engine.invoke_bundle_api(call).await
}

/// Parse a method name for the gRPC binding (HTTP routes by method already).
pub(crate) fn parse_method(method: &str) -> Result<ApiMethod, BundleApiError> {
    ApiMethod::parse(method).ok_or_else(|| {
        BundleApiError::BadRequest(format!(
            "method `{method}` must be GET, POST, PUT, PATCH, or DELETE"
        ))
    })
}

async fn list_bundle_apis(
    State(st): State<ServerState>,
) -> Result<Json<pb::ListBundleApisResponse>, V1Error> {
    let mut apis = Vec::new();
    for bundle in st.engine.bundle_apis().await {
        for api in bundle.apis {
            apis.push(pb::BundleApiInfo {
                bundle: bundle.bundle.clone(),
                api: api.id,
                method: api.method.as_str().to_string(),
                scope: api.scope.as_str().to_string(),
                path: api.path.as_str().to_string(),
                description: api.description,
                request_schema: api.request_schema.map(to_pb_value).transpose()?,
                response_schema: api.response_schema.map(to_pb_value).transpose()?,
            });
        }
    }
    apis.sort_by(|left, right| {
        left.bundle
            .cmp(&right.bundle)
            .then_with(|| left.api.cmp(&right.api))
    });
    Ok(Json(pb::ListBundleApisResponse { apis }))
}

/// Convert plain JSON into a protobuf `Value`.
pub(crate) fn to_pb_value(value: Value) -> Result<pbjson_types::Value, V1Error> {
    serde_json::from_value(value)
        .map_err(|error| V1Error::internal(format!("encode JSON value: {error}")))
}

async fn invoke_session_http(
    State(st): State<ServerState>,
    AxumPath((id, bundle, _)): AxumPath<(String, String, String)>,
    method: Method,
    uri: Uri,
    body: Body,
) -> Response {
    let session = match parse_session(&id) {
        Ok(session) => session,
        Err(error) => return error.into_response(),
    };
    // `/v1/sessions/{id}/bundles/{bundle}/{tail…}`: the raw (still
    // percent-encoded) tail follows the sixth `/`.
    let tail = uri.path().splitn(7, '/').nth(6).unwrap_or_default();
    serve_http(&st, bundle, Some(session), method, &uri, tail, body).await
}

async fn invoke_global_http(
    State(st): State<ServerState>,
    AxumPath((bundle, _)): AxumPath<(String, String)>,
    method: Method,
    uri: Uri,
    body: Body,
) -> Response {
    // `/v1/bundles/{bundle}/api/{tail…}`: the raw tail follows the fifth `/`.
    let tail = uri.path().splitn(6, '/').nth(5).unwrap_or_default();
    serve_http(&st, bundle, None, method, &uri, tail, body).await
}

async fn serve_http(
    st: &ServerState,
    bundle: String,
    session: Option<SessionId>,
    method: Method,
    uri: &Uri,
    tail: &str,
    body: Body,
) -> Response {
    let Some(method) = ApiMethod::parse(method.as_str()) else {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    };
    let query = match Query::<BTreeMap<String, String>>::try_from_uri(uri) {
        Ok(Query(query)) => query,
        Err(error) => return bad_request(format!("invalid query string: {error}")),
    };
    let bytes = match axum::body::to_bytes(body, MAX_BUNDLE_API_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return bad_request(format!(
                "request body must be at most {MAX_BUNDLE_API_BODY_BYTES} bytes"
            ));
        }
    };
    let body = if bytes.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(body) => body,
            Err(error) => return bad_request(format!("request body must be JSON: {error}")),
        }
    };
    let call = BundleApiCall {
        bundle,
        method,
        session,
        path: format!("/{tail}"),
        query,
        body,
    };
    match invoke(st, call).await {
        Ok(outcome) => http_response(outcome),
        Err(error) => error_response(error),
    }
}

/// The process's status and body, verbatim: a JSON body is rendered as
/// `application/json` (integers stay exact); a `null` body sends none.
fn http_response(outcome: BundleApiOutcome) -> Response {
    let status = StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::BAD_GATEWAY);
    if outcome.body.is_null() {
        return status.into_response();
    }
    match serde_json::to_vec(&outcome.body) {
        Ok(bytes) => (
            status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(BUNDLE_API_CONTENT_TYPE),
            )],
            bytes,
        )
            .into_response(),
        Err(error) => V1Error::internal(format!("encode bundle API body: {error}")).into_response(),
    }
}
