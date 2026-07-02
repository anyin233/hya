use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hya_proto::MessageId;
use serde::Deserialize;

use crate::{ApiError, ServerState, parse_session};

use super::projection;

#[derive(Deserialize)]
pub(super) struct MessagesQuery {
    before: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct MessageCursor {
    id: String,
    index: usize,
}

pub(super) async fn messages(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let messages = match super::session_legacy::legacy_load_session(&st, session, None).await? {
        Ok(snapshot) => snapshot.messages,
        Err(response) => return Ok(response),
    };
    if query.limit.is_none() && query.before.is_some() {
        return Err(ApiError::bad_request("before requires limit"));
    }
    let Some(limit) = query.limit.filter(|limit| *limit > 0) else {
        return Ok(Json(messages).into_response());
    };
    let before = match query.before {
        Some(before) => Some(decode_cursor(&before)?),
        None => None,
    };
    let page = page_messages(session.to_string(), messages, limit, before.as_ref())?;
    Ok(page.into_response())
}

pub(super) async fn message(
    State(st): State<ServerState>,
    Path((id, message)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let message = message
        .parse::<MessageId>()
        .map_err(|_| ApiError::bad_request("invalid message id"))?
        .to_string();
    let snapshot = match super::session_legacy::legacy_load_session(&st, session, None).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    let message = snapshot
        .messages
        .into_iter()
        .find(|item| item.id() == message)
        .ok_or_else(|| ApiError::not_found("message not found"))?;
    Ok(Json(message).into_response())
}

fn page_messages(
    session: String,
    messages: Vec<projection::CompatMessage>,
    limit: usize,
    before: Option<&MessageCursor>,
) -> Result<(HeaderMap, Json<Vec<projection::CompatMessage>>), ApiError> {
    let end = before.map_or(messages.len(), |cursor| cursor.index);
    if before.is_some_and(|cursor| {
        messages
            .get(cursor.index)
            .map(projection::CompatMessage::id)
            != Some(cursor.id.as_str())
    }) {
        return Err(ApiError::bad_request("invalid cursor"));
    }
    let start = end.saturating_sub(limit);
    let page: Vec<_> = messages
        .into_iter()
        .enumerate()
        .filter_map(|(idx, message)| (idx >= start && idx < end).then_some(message))
        .collect();
    let mut headers = HeaderMap::new();
    if start > 0 {
        let tail = page
            .first()
            .ok_or_else(|| ApiError::bad_request("invalid cursor"))?;
        let cursor = encode_cursor(tail.id(), start)?;
        headers.insert(
            "x-next-cursor",
            HeaderValue::from_str(&cursor).map_err(|e| ApiError::internal(e.to_string()))?,
        );
        headers.insert(
            "access-control-expose-headers",
            HeaderValue::from_static("Link, X-Next-Cursor"),
        );
        let link =
            format!("</session/{session}/message?limit={limit}&before={cursor}>; rel=\"next\"");
        headers.insert(
            "link",
            HeaderValue::from_str(&link).map_err(|e| ApiError::internal(e.to_string()))?,
        );
    }
    Ok((headers, Json(page)))
}

fn encode_cursor(id: &str, index: usize) -> Result<String, ApiError> {
    let raw = serde_json::json!({ "id": id, "index": index });
    let bytes = serde_json::to_vec(&raw).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(cursor: &str) -> Result<MessageCursor, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| ApiError::bad_request("invalid cursor"))?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::bad_request("invalid cursor"))
}
