//! `/v1` worktree domain over the engine's git worktree helpers.

use std::collections::BTreeMap;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::ServerState;
use hya_api::v1 as pb;

use super::{V1Error, scope_directory};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/worktrees", get(list_worktrees).post(create_worktree))
        .route("/v1/worktrees/:id", axum::routing::delete(delete_worktree))
        .route("/v1/worktrees/:id/reset", post(reset_worktree))
}

fn worktree_info(value: serde_json::Value) -> pb::Worktree {
    pb::Worktree {
        id: field(&value, "directory"),
        path: field(&value, "directory"),
        branch: field(&value, "branch"),
        head: field(&value, "head"),
    }
}

async fn list_worktrees(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListWorktreesResponse>, V1Error> {
    let request: pb::ListWorktreesRequest = super::query_request(&[], &query)?;
    let source = scope_directory(&headers, &request.directory);
    let infos = crate::support::worktree_git::infos(&source)
        .await
        .map_err(V1Error::internal)?;
    let worktrees = infos
        .iter()
        .filter_map(|info| serde_json::to_value(info).ok())
        .map(worktree_info)
        .collect();
    Ok(Json(pb::ListWorktreesResponse { worktrees }))
}

async fn create_worktree(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    Json(request): Json<pb::CreateWorktreeRequest>,
) -> Result<Json<pb::Worktree>, V1Error> {
    let scope: pb::ListWorktreesRequest = super::query_request(&[], &query)?;
    let source = scope_directory(&headers, &scope.directory);
    let requested = if request.name.is_empty() {
        None
    } else {
        Some(request.name.as_str())
    };
    let info = crate::support::worktree_git::create(&source, requested)
        .await
        .map_err(V1Error::internal)?;
    Ok(Json(worktree_info(
        serde_json::to_value(&info).unwrap_or(serde_json::Value::Null),
    )))
}

async fn delete_worktree(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::DeleteWorktreeResponse>, V1Error> {
    let scope: pb::ListWorktreesRequest = super::query_request(&[], &query)?;
    let source = scope_directory(&headers, &scope.directory);
    let removed = crate::support::worktree_git::remove(&source, &id)
        .await
        .map_err(V1Error::internal)?;
    if !removed {
        return Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("worktree not found: {id}"),
        ));
    }
    Ok(Json(pb::DeleteWorktreeResponse {}))
}

async fn reset_worktree(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::Worktree>, V1Error> {
    let scope: pb::ListWorktreesRequest = super::query_request(&[], &query)?;
    let source = scope_directory(&headers, &scope.directory);
    let reset = crate::support::worktree_git::reset(&source, &id)
        .await
        .map_err(V1Error::internal)?;
    if !reset {
        return Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("worktree not found: {id}"),
        ));
    }
    Ok(Json(pb::Worktree {
        id: id.clone(),
        path: id,
        branch: String::new(),
        head: String::new(),
    }))
}

fn field(value: &serde_json::Value, name: &str) -> String {
    value
        .get(name)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .unwrap_or_default()
}
