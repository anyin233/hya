use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ApiError, ServerState};

pub(in crate::compat) mod git;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/vcs", get(info))
        .route("/vcs/status", get(status))
        .route("/vcs/diff", get(diff))
        .route("/vcs/diff/raw", get(diff_raw))
        .route("/vcs/apply", post(apply))
}

#[derive(Serialize)]
struct VcsInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_branch: Option<String>,
}

#[derive(Deserialize)]
struct DiffQuery {
    mode: String,
    context: Option<usize>,
    #[serde(flatten)]
    routing: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ApplyInput {
    patch: String,
}

#[derive(Serialize)]
struct ApplyResult {
    applied: bool,
}

async fn info(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<VcsInfo> {
    let workdir = routed_workdir(&st, &query, &headers);
    Json(VcsInfo {
        branch: git::branch(&workdir),
        default_branch: git::default_branch(&workdir),
    })
}

async fn status(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Vec<git::FileStatus>>, ApiError> {
    let workdir = routed_workdir(&st, &query, &headers);
    if !git::is_repo(&workdir) {
        return Ok(Json(Vec::new()));
    }
    Ok(Json(git::status(&workdir)?))
}

async fn diff(
    State(st): State<ServerState>,
    Query(query): Query<DiffQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<git::FileDiff>>, ApiError> {
    let workdir = routed_workdir(&st, &query.routing, &headers);
    if !git::is_repo(&workdir) {
        return Ok(Json(Vec::new()));
    }
    Ok(Json(git::diff(&workdir, &query.mode, query.context)?))
}

async fn diff_raw(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<(HeaderMap, String), ApiError> {
    let workdir = routed_workdir(&st, &query, &headers);
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/x-diff; charset=utf-8"),
    );
    if !git::is_repo(&workdir) {
        return Ok((response_headers, String::new()));
    }
    Ok((response_headers, git::raw_diff(&workdir)?))
}

async fn apply(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    Json(input): Json<ApplyInput>,
) -> Result<Json<ApplyResult>, (StatusCode, Json<serde_json::Value>)> {
    let workdir = routed_workdir(&st, &query, &headers);
    if !git::is_repo(&workdir) {
        return Err(apply_error(
            "Patch can't be applied because the project is not git-based",
            "non-git",
        ));
    }
    git::apply_patch(&workdir, &input.patch)
        .map_err(|_| apply_error("Patch can't be applied", "not-clean"))?;
    Ok(Json(ApplyResult { applied: true }))
}

fn apply_error(message: &str, reason: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "name": "VcsApplyError", "data": { "message": message, "reason": reason } })),
    )
}

fn routed_workdir(
    st: &ServerState,
    query: &BTreeMap<String, String>,
    headers: &HeaderMap,
) -> PathBuf {
    let location = super::super::location::LocationRef::from_request(query, headers);
    super::super::location::workdir_at(st, &location)
}
