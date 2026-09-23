//! `/v1` bundle session views: discovery and one-view reads.
//!
//! A bundle with an explicit `extensions.process` may declare read-only
//! session views; the engine resolves the bundle in the live runtime
//! generation and forwards the request to its process.

use std::collections::BTreeMap;

use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use hya_api::error::Code;
use hya_api::v1 as pb;
use hya_core::BundleViewError;
use serde_json::{Value, json};

use crate::ServerState;

use super::V1Error;
use super::session::parse_session;

/// The only media type bundle views produce.
const VIEW_CONTENT_TYPE: &str = "application/json";

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/sessions/:id/views", get(list_session_views))
        .route(
            "/v1/sessions/:id/views/:bundle/:view",
            get(get_session_view),
        )
}

impl From<BundleViewError> for V1Error {
    fn from(error: BundleViewError) -> Self {
        let code = match &error {
            BundleViewError::SessionNotFound(_) => Code::SessionNotFound,
            BundleViewError::BundleNotFound(_) | BundleViewError::ViewNotFound { .. } => {
                Code::ViewNotFound
            }
            BundleViewError::Failed { .. } => Code::ViewFailed,
            BundleViewError::Core(_) => Code::Internal,
        };
        Self::new(code, error.to_string())
    }
}

async fn list_session_views(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::ListSessionViewsResponse>, V1Error> {
    let session = parse_session(&id)?;
    if !st.engine.session_exists(session).await? {
        return Err(V1Error::session_not_found(&id));
    }
    let views = st
        .engine
        .bundle_views()
        .await
        .into_iter()
        .flat_map(|bundle| {
            bundle
                .views
                .into_iter()
                .map(move |view| pb::SessionViewInfo {
                    bundle: bundle.bundle.clone(),
                    view: view.id,
                    description: view.description,
                })
        })
        .collect();
    Ok(Json(pb::ListSessionViewsResponse { views }))
}

/// Answer one view. The body is rendered verbatim (not through protobuf
/// `Value`) so JSON integers stay exact for HTTP clients; the document is
/// still valid protojson for `SessionView`.
async fn get_session_view(
    State(st): State<ServerState>,
    AxumPath((id, bundle, view)): AxumPath<(String, String, String)>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<Value>, V1Error> {
    let session = parse_session(&id)?;
    if bundle.is_empty() || view.is_empty() {
        return Err(V1Error::invalid_argument("bundle and view are required"));
    }
    let body = st
        .engine
        .bundle_view(session, &bundle, &view, query)
        .await?;
    Ok(Json(json!({
        "bundle": bundle,
        "view": view,
        "contentType": VIEW_CONTENT_TYPE,
        "body": body,
    })))
}
