use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use super::projection::OpenCodeSessionInfo;
use crate::ApiError;

#[derive(Serialize)]
pub(super) struct SessionsResponse {
    pub(super) data: Vec<OpenCodeSessionInfo>,
    pub(super) cursor: SessionCursors,
}

#[derive(Serialize)]
pub(super) struct SessionCursors {
    previous: Option<String>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct SessionCursor {
    id: String,
    direction: CursorDirection,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum CursorDirection {
    Previous,
    Next,
}

pub(super) fn cursor_start(
    sessions: &[OpenCodeSessionInfo],
    cursor: Option<&str>,
    limit: usize,
) -> Result<usize, ()> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let cursor = decode_cursor(cursor)?;
    let Some(position) = sessions
        .iter()
        .position(|session| session.id() == cursor.id.as_str())
    else {
        return Ok(sessions.len());
    };
    Ok(match cursor.direction {
        CursorDirection::Previous => position.saturating_sub(limit),
        CursorDirection::Next => position.saturating_add(1),
    })
}

pub(super) fn response_cursor(data: &[OpenCodeSessionInfo]) -> Result<SessionCursors, ApiError> {
    Ok(SessionCursors {
        previous: data
            .first()
            .map(|session| encode_cursor(session.id(), CursorDirection::Previous))
            .transpose()?,
        next: data
            .last()
            .map(|session| encode_cursor(session.id(), CursorDirection::Next))
            .transpose()?,
    })
}

fn encode_cursor(id: &str, direction: CursorDirection) -> Result<String, ApiError> {
    let raw = serde_json::json!({ "id": id, "direction": direction });
    let bytes = serde_json::to_vec(&raw).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(cursor: &str) -> Result<SessionCursor, ()> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| ())?;
    serde_json::from_slice(&bytes).map_err(|_| ())
}
