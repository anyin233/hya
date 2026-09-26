//! `hya-server` v1 HTTP binding: the `/v1` routes generated from the
//! `hya.v1` contract (`crates/hya-api`).
//!
//! Every handler speaks the generated protojson types directly, renders
//! failures through the stable error model (`hya_api::error`), and shares
//! the same engine/app state as the legacy surface. The gRPC binding
//! (phase P3) wraps the same handler logic.

mod agent_models;
mod auth;
mod bundle_api;
mod catalog;
mod convert;
mod events;
mod fs;
mod grpc;
mod interaction;
mod logs;
mod mcp;
mod message;
mod process;
mod project;
mod providers;
mod pty;
mod relay;
mod session;
mod turn;
mod workflow;
mod worktree;

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::ServerState;
use hya_api::error::{ApiError, Code};

/// Directory scope header (D6): overrides the `directory` request field.
pub(crate) const DIRECTORY_HEADER: &str = "x-hya-directory";

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .merge(process::router())
        .merge(catalog::router())
        .merge(agent_models::router())
        .merge(auth::router())
        .merge(logs::router())
        .merge(session::router())
        .merge(bundle_api::router())
        .merge(turn::router())
        .merge(message::router())
        .merge(events::router())
        .merge(interaction::router())
        .merge(workflow::router())
        .merge(fs::router())
        .merge(project::router())
        .merge(worktree::router())
        .merge(mcp::router())
        .merge(pty::router())
        .merge(relay::router())
}

/// The v1 JSON body extractor and response: `axum::Json`, except that a body
/// that fails to decode (malformed JSON, a wrong field type, an out-of-range
/// number, a missing JSON content type) is a v1 `invalid_argument` error in
/// the `{"error": {...}}` shape instead of axum's plain-text 400/415/422.
/// An oversized body keeps the transport's 413.
pub(crate) struct Json<T>(pub(crate) T);

#[axum::async_trait]
impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_rejection(rejection)),
        }
    }
}

fn json_rejection(rejection: JsonRejection) -> Response {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return rejection.into_response();
    }
    V1Error::invalid_argument(format!("invalid request body: {}", rejection.body_text()))
        .into_response()
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// One failed v1 call rendered as `{"error": {"code", "message"}}` with the
/// canonical HTTP status from the stable error table.
pub(crate) struct V1Error(ApiError);

impl V1Error {
    /// Build an error from a stable code and message.
    pub(crate) fn new(code: Code, message: impl Into<String>) -> Self {
        Self(ApiError::new(code, message))
    }

    /// The request body or parameters are invalid.
    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(Code::InvalidArgument, message)
    }

    /// The session id does not exist.
    pub(crate) fn session_not_found(session: &str) -> Self {
        Self::new(
            Code::SessionNotFound,
            format!("session not found: {session}"),
        )
    }

    /// Another run owns the session's admission slot.
    pub(crate) fn session_busy() -> Self {
        Self::new(Code::SessionBusy, "session busy")
    }

    /// A required runtime capability is unavailable.
    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(Code::Unavailable, message)
    }

    /// The requested resource does not exist.
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(Code::NotFound, message)
    }

    /// Unhandled internal failure.
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(Code::Internal, message)
    }

    /// The same failure as a gRPC status (stable code table mapping).
    pub(crate) fn grpc_status(&self) -> tonic::Status {
        self.0.grpc_status()
    }
}

impl From<crate::ApiError> for V1Error {
    fn from(error: crate::ApiError) -> Self {
        let code = match error.code() {
            Some("invalid_argument") | Some("bad_request") => Code::InvalidArgument,
            Some("session_not_found") | Some("not_found") => Code::SessionNotFound,
            Some("session_busy") | Some("conflict") => Code::SessionBusy,
            Some("forbidden") | Some("permission_denied") => Code::PermissionDenied,
            Some("unavailable") | Some("service_unavailable") => Code::Unavailable,
            _ => Code::Internal,
        };
        Self::new(code, error.text().to_owned())
    }
}

impl From<hya_core::CoreError> for V1Error {
    fn from(error: hya_core::CoreError) -> Self {
        let code = match &error {
            hya_core::CoreError::TurnAlreadyActive { .. } => Code::SessionBusy,
            _ => Code::Internal,
        };
        Self::new(code, error.to_string())
    }
}

impl From<hya_store::StoreError> for V1Error {
    fn from(error: hya_store::StoreError) -> Self {
        use hya_store::StoreError as E;
        let code = match &error {
            E::ProjectNameEmpty
            | E::ProjectRootsEmpty
            | E::ProjectRootNotAbsolute { .. }
            | E::ProjectRootInvalid { .. } => Code::InvalidArgument,
            E::ProjectNotFound { .. } => Code::NotFound,
            E::ProjectInUse { .. } => Code::FailedPrecondition,
            _ => Code::Internal,
        };
        Self::new(code, error.to_string())
    }
}

impl IntoResponse for V1Error {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0.code().http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            Json(json!({
                "error": {
                    "code": self.0.code().as_str(),
                    "message": self.0.message(),
                }
            })),
        )
            .into_response()
    }
}

/// The directory scope a request names, if any.
///
/// Precedence: the `x-hya-directory` header, then the request's `directory`
/// field. `hya serve` has no working directory of its own (ADR-0024), so
/// there is no fallback: `Ok(None)` means the request named no scope. A
/// relative scope would resolve against the server process's cwd, so it is
/// `invalid_argument`.
pub(crate) fn request_scope(
    headers: &axum::http::HeaderMap,
    requested: &str,
) -> Result<Option<std::path::PathBuf>, V1Error> {
    let named = headers
        .get(DIRECTORY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|header| !header.is_empty())
        .or_else(|| Some(requested.trim()).filter(|field| !field.is_empty()));
    let Some(named) = named else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(named);
    if !path.is_absolute() {
        return Err(V1Error::invalid_argument(format!(
            "the directory scope must be an absolute path, got `{named}`"
        )));
    }
    Ok(Some(path))
}

/// The catalog place of a catalog read: the request's directory scope
/// ([`request_scope`]) resolved through the Project that contains it.
///
/// Every catalog rpc (agents, commands, skills, bootstrap, agent models,
/// permission modes, bundle APIs) answers from this one helper so they
/// agree: a directory inside a registered Project lists that Project's
/// catalog (every root, first root wins); a directory in no Project lists
/// only its own inert tiers (no Project bundle); no directory is the
/// global view.
pub(crate) async fn catalog_scope(
    st: &crate::ServerState,
    headers: &axum::http::HeaderMap,
    requested: &str,
) -> Result<crate::support::catalog_place::CatalogPlace, V1Error> {
    let directory = request_scope(headers, requested)?;
    Ok(crate::support::catalog_place::CatalogPlace::for_directory(st, directory).await)
}

/// The session a catalog read is scoped to, if its `session` field names
/// one: `invalid_argument` for a malformed id, `not_found` for an unknown
/// session.
pub(crate) async fn scope_session(
    st: &crate::ServerState,
    session: &str,
) -> Result<Option<hya_proto::SessionId>, V1Error> {
    let session = session.trim();
    if session.is_empty() {
        return Ok(None);
    }
    let id = session
        .parse::<hya_proto::SessionId>()
        .map_err(|_| V1Error::invalid_argument(format!("invalid session id: {session}")))?;
    if !st.engine.session_exists(id).await? {
        return Err(V1Error::session_not_found(session));
    }
    Ok(Some(id))
}

/// The directory scope of an rpc that cannot work without one.
///
/// Fails with `invalid_argument` when the request names none; the server
/// never substitutes its own working directory.
pub(crate) fn scope_directory(
    headers: &axum::http::HeaderMap,
    requested: &str,
) -> Result<std::path::PathBuf, V1Error> {
    request_scope(headers, requested)?.ok_or_else(|| {
        V1Error::invalid_argument(format!(
            "this rpc needs a directory scope: send the `{DIRECTORY_HEADER}` header or \
             the request's `directory` field (hya serve has no working directory)"
        ))
    })
}

/// Build a generated request message from path variables and query
/// parameters.
///
/// GET/DELETE requests carry their fields as query parameters in protojson
/// camelCase form; `page.cursor` and `page.limit` populate the nested page
/// message. Path variables override query values. String-encoded numbers
/// follow protojson rules.
pub(crate) fn query_request<T: DeserializeOwned>(
    path_vars: &[(&str, &str)],
    query: &BTreeMap<String, String>,
) -> Result<T, V1Error> {
    query_request_pairs(path_vars, query.iter(), &[])
}

/// [`query_request`] over raw query pairs, where each key in `repeated`
/// (a `repeated` proto field) collects every occurrence into a JSON array
/// (`?paths=a&paths=b`). Other keys keep the last occurrence.
pub(crate) fn query_request_pairs<'a, T: DeserializeOwned>(
    path_vars: &[(&str, &str)],
    query: impl IntoIterator<Item = (&'a String, &'a String)>,
    repeated: &[&str],
) -> Result<T, V1Error> {
    let mut map = serde_json::Map::new();
    let mut page = serde_json::Map::new();
    for (key, value) in query {
        if value.is_empty() {
            continue;
        }
        if repeated.contains(&key.as_str()) {
            let entry = map
                .entry(key.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(items) = entry {
                items.push(Value::String(value.clone()));
            }
            continue;
        }
        // Query strings carry every value as text; protojson bools need
        // real JSON booleans, so coerce the canonical literals.
        let json = match value.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(value.clone()),
        };
        if let Some(field @ ("cursor" | "limit")) = key.strip_prefix("page.") {
            page.insert(field.to_owned(), json);
        } else {
            map.insert(key.clone(), json);
        }
    }
    if !page.is_empty() {
        map.insert("page".to_owned(), Value::Object(page));
    }
    for (key, value) in path_vars {
        map.insert((*key).to_owned(), Value::String((*value).to_owned()));
    }
    serde_json::from_value(Value::Object(map))
        .map_err(|error| V1Error::invalid_argument(format!("invalid request fields: {error}")))
}

pub use grpc::V1Grpc;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hya_api::v1 as pb;

    use super::query_request;

    #[test]
    fn query_request_decodes_page_fields_from_dotted_query_keys() {
        let query = BTreeMap::from([
            ("page.cursor".to_owned(), "next".to_owned()),
            ("page.limit".to_owned(), "2".to_owned()),
        ]);
        let request: pb::ListSessionsRequest = query_request(&[], &query).unwrap_or_default();
        let page = request.page;
        assert_eq!(page.as_ref().map(|page| page.cursor.as_str()), Some("next"));
        assert_eq!(page.as_ref().map(|page| page.limit), Some(2));
    }
}
