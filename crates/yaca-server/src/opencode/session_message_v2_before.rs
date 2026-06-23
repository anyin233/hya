use axum::Json;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use yaca_proto::Projection;

use crate::{ApiError, ServerState, parse_session};

#[derive(Deserialize, Serialize)]
struct BeforeCursor {
    id: String,
    time: u64,
}

pub(super) async fn messages(
    st: ServerState,
    id: String,
    limit: Option<usize>,
    before: Option<String>,
) -> Result<Response, ApiError> {
    if before.is_some() && limit.is_none() {
        return Err(ApiError::bad_request("before requires limit"));
    }
    let before = match before.as_deref().map(decode_cursor).transpose() {
        Ok(before) => before,
        Err(()) => return Err(ApiError::bad_request("invalid cursor")),
    };
    let session = parse_session(&id)?;
    let envs = st.engine.replay(session).await?;
    if envs.is_empty() {
        return Ok(super::errors::session_not_found(&id));
    }
    let projection = Projection::from_events(&envs);
    let items = super::session_context_messages::v2_messages(&envs, &projection);
    let Some(limit) = limit.filter(|limit| *limit > 0) else {
        return Ok(Json(items).into_response());
    };
    Ok(before_page(id, items, limit, before.as_ref())?.into_response())
}

fn before_page(
    session: String,
    messages: Vec<Value>,
    limit: usize,
    before: Option<&BeforeCursor>,
) -> Result<(HeaderMap, Json<Vec<Value>>), ApiError> {
    let mut desc = messages
        .into_iter()
        .rev()
        .filter(|message| before.is_none_or(|cursor| older_than(message, cursor)))
        .collect::<Vec<_>>();
    let more = desc.len() > limit;
    if more {
        desc.truncate(limit);
    }
    let tail = desc.last().cloned();
    desc.reverse();
    Ok((headers(session, limit, tail, more)?, Json(desc)))
}

fn headers(
    session: String,
    limit: usize,
    tail: Option<Value>,
    more: bool,
) -> Result<HeaderMap, ApiError> {
    let mut headers = HeaderMap::new();
    let Some(cursor) = tail
        .filter(|_| more)
        .as_ref()
        .and_then(encode_cursor_for)
        .transpose()?
    else {
        return Ok(headers);
    };
    headers.insert(
        "x-next-cursor",
        HeaderValue::from_str(&cursor).map_err(|e| ApiError::internal(e.to_string()))?,
    );
    headers.insert(
        "access-control-expose-headers",
        HeaderValue::from_static("Link, X-Next-Cursor"),
    );
    let link =
        format!("</api/session/{session}/message?limit={limit}&before={cursor}>; rel=\"next\"");
    headers.insert(
        "link",
        HeaderValue::from_str(&link).map_err(|e| ApiError::internal(e.to_string()))?,
    );
    Ok(headers)
}

fn older_than(message: &Value, cursor: &BeforeCursor) -> bool {
    let time = message_time(message);
    time < cursor.time
        || (time == cursor.time && message_id(message).is_some_and(|id| id < cursor.id.as_str()))
}

fn encode_cursor_for(message: &Value) -> Option<Result<String, ApiError>> {
    let bytes = serde_json::to_vec(&BeforeCursor {
        id: message_id(message)?.to_string(),
        time: message_time(message),
    })
    .map_err(|e| ApiError::internal(e.to_string()));
    Some(bytes.map(|bytes| URL_SAFE_NO_PAD.encode(bytes)))
}

fn decode_cursor(cursor: &str) -> Result<BeforeCursor, ()> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| ())?;
    serde_json::from_slice(&bytes).map_err(|_| ())
}

fn message_id(message: &Value) -> Option<&str> {
    message.get("id")?.as_str()
}

fn message_time(message: &Value) -> u64 {
    message
        .get("time")
        .and_then(|time| time.get("created"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}
