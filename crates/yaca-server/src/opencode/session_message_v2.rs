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
    #[serde(default)]
    time: u64,
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
        return Ok(super::errors::invalid_cursor(
            "Cursor cannot be combined with order",
        ));
    }
    let decoded = match query.cursor.as_deref().map(decode_cursor).transpose() {
        Ok(decoded) => decoded,
        Err(()) => return Ok(super::errors::invalid_cursor("Invalid cursor")),
    };
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
    let mut items = super::session_context_messages::v2_messages(&envs, &projection);
    if matches!(order, MessageOrder::Desc) {
        items.reverse();
    }
    let limit = query
        .limit
        .filter(|limit| *limit > 0)
        .unwrap_or(items.len());
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
            .and_then(|message| encode_cursor_for(message, order, CursorDirection::Previous))
            .transpose()?,
        next: data
            .last()
            .and_then(|message| encode_cursor_for(message, order, CursorDirection::Next))
            .transpose()?,
    })
}

fn message_id(message: &Value) -> Option<&str> {
    message.get("id")?.as_str()
}

fn encode_cursor_for(
    message: &Value,
    order: MessageOrder,
    direction: CursorDirection,
) -> Option<Result<String, ApiError>> {
    Some(encode_cursor(
        message_id(message)?,
        message_time(message),
        order,
        direction,
    ))
}

fn message_time(message: &Value) -> u64 {
    message
        .get("time")
        .and_then(|time| time.get("created"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn encode_cursor(
    id: &str,
    time: u64,
    order: MessageOrder,
    direction: CursorDirection,
) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(&MessageCursor {
        id: id.to_string(),
        time,
        order,
        direction,
    })
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(cursor: &str) -> Result<MessageCursor, ()> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| ())?;
    serde_json::from_slice(&bytes).map_err(|_| ())
}
