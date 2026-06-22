use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use yaca_proto::Projection;

use crate::{ApiError, ServerState, parse_session};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/api/session/:id/message", get(messages))
}

#[derive(Deserialize)]
struct MessagesQuery {
    limit: Option<usize>,
    order: Option<MessageOrder>,
    cursor: Option<String>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum MessageOrder {
    Asc,
    Desc,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum CursorDirection {
    Previous,
    Next,
}

#[derive(Deserialize, Serialize)]
struct MessageCursor {
    id: String,
    order: MessageOrder,
    direction: CursorDirection,
}

#[derive(Serialize)]
struct MessagesResponse {
    data: Vec<Value>,
    cursor: ResponseCursor,
}

#[derive(Default, Serialize)]
struct ResponseCursor {
    #[serde(skip_serializing_if = "Option::is_none")]
    previous: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

async fn messages(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> Result<Response, ApiError> {
    if query.cursor.is_some() && query.order.is_some() {
        return Err(ApiError::bad_request(
            "cursor cannot be combined with order",
        ));
    }
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request("limit must be between 1 and 200"));
    }
    let decoded = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let order = decoded
        .as_ref()
        .map(|cursor| cursor.order)
        .or(query.order)
        .unwrap_or(MessageOrder::Desc);
    let session = parse_session(&id)?;
    let envs = st.engine.replay(session).await?;
    if envs.is_empty() {
        return Ok(super::errors::session_not_found(&id));
    }
    let projection = Projection::from_events(&envs);
    let mut items = super::session_context::v2_messages(&envs, &projection);
    if matches!(order, MessageOrder::Desc) {
        items.reverse();
    }
    let start = decoded
        .as_ref()
        .map(|cursor| cursor_start(&items, cursor, limit))
        .unwrap_or(0);
    let data: Vec<_> = items.into_iter().skip(start).take(limit).collect();
    Ok(Json(MessagesResponse {
        cursor: response_cursor(&data, order)?,
        data,
    })
    .into_response())
}

fn cursor_start(messages: &[Value], cursor: &MessageCursor, limit: usize) -> usize {
    let Some(position) = messages
        .iter()
        .position(|message| message_id(message) == Some(cursor.id.as_str()))
    else {
        return messages.len();
    };
    match cursor.direction {
        CursorDirection::Previous => position.saturating_sub(limit),
        CursorDirection::Next => position.saturating_add(1),
    }
}

fn response_cursor(data: &[Value], order: MessageOrder) -> Result<ResponseCursor, ApiError> {
    Ok(ResponseCursor {
        previous: data
            .first()
            .and_then(message_id)
            .map(|id| encode_cursor(id, order, CursorDirection::Previous))
            .transpose()?,
        next: data
            .last()
            .and_then(message_id)
            .map(|id| encode_cursor(id, order, CursorDirection::Next))
            .transpose()?,
    })
}

fn message_id(message: &Value) -> Option<&str> {
    message.get("id")?.as_str()
}

fn encode_cursor(
    id: &str,
    order: MessageOrder,
    direction: CursorDirection,
) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(&MessageCursor {
        id: id.to_string(),
        order,
        direction,
    })
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(cursor: &str) -> Result<MessageCursor, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| ApiError::bad_request("invalid cursor"))?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::bad_request("invalid cursor"))
}
