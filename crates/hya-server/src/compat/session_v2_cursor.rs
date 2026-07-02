use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::projection::CompatSessionInfo;
use crate::ApiError;

#[derive(Serialize)]
pub(super) struct SessionsResponse {
    pub(super) data: Vec<CompatSessionInfo>,
    pub(super) cursor: SessionCursors,
}

#[derive(Serialize)]
pub(super) struct SessionCursors {
    previous: Option<String>,
    next: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) struct CursorParams<'a> {
    pub(super) order: Option<&'a str>,
    pub(super) search: Option<&'a str>,
    pub(super) directory: Option<&'a str>,
    pub(super) workspace: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub(super) struct SessionCursor {
    pub(super) order: Option<String>,
    pub(super) search: Option<String>,
    pub(super) directory: Option<String>,
    pub(super) workspace: Option<String>,
    anchor: CursorAnchor,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CursorAnchor {
    id: String,
    time: u64,
    direction: CursorDirection,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum CursorDirection {
    Previous,
    Next,
}

#[derive(Deserialize)]
struct CursorPayload {
    order: Option<String>,
    search: Option<String>,
    directory: Option<String>,
    workspace: Option<String>,
    anchor: CursorAnchor,
}

#[derive(Deserialize)]
struct LegacyCursor {
    id: String,
    direction: CursorDirection,
}

#[derive(Serialize)]
struct CursorPayloadRef<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    order: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    directory: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<&'a str>,
    anchor: CursorAnchorRef<'a>,
}

#[derive(Serialize)]
struct CursorAnchorRef<'a> {
    id: &'a str,
    time: u64,
    direction: CursorDirection,
}

pub(super) fn decode_cursor(cursor: &str) -> Result<SessionCursor, ()> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| ())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if let Ok(payload) = serde_json::from_value::<CursorPayload>(value.clone()) {
        return Ok(SessionCursor {
            order: payload.order,
            search: payload.search,
            directory: payload.directory,
            workspace: payload.workspace,
            anchor: payload.anchor,
        });
    }
    let legacy = serde_json::from_value::<LegacyCursor>(value).map_err(|_| ())?;
    Ok(SessionCursor {
        order: None,
        search: None,
        directory: None,
        workspace: None,
        anchor: CursorAnchor {
            id: legacy.id,
            time: 0,
            direction: legacy.direction,
        },
    })
}

pub(super) fn cursor_start(
    sessions: &[CompatSessionInfo],
    cursor: &SessionCursor,
    limit: usize,
) -> usize {
    let Some(position) = sessions
        .iter()
        .position(|session| session.id() == cursor.anchor.id.as_str())
    else {
        return sessions.len();
    };
    match cursor.anchor.direction {
        CursorDirection::Previous => position.saturating_sub(limit),
        CursorDirection::Next => position.saturating_add(1),
    }
}

pub(super) fn response_cursor(
    data: &[CompatSessionInfo],
    params: CursorParams<'_>,
) -> Result<SessionCursors, ApiError> {
    Ok(SessionCursors {
        previous: data
            .first()
            .map(|session| encode_cursor(session, CursorDirection::Previous, params))
            .transpose()?,
        next: data
            .last()
            .map(|session| encode_cursor(session, CursorDirection::Next, params))
            .transpose()?,
    })
}

fn encode_cursor(
    session: &CompatSessionInfo,
    direction: CursorDirection,
    params: CursorParams<'_>,
) -> Result<String, ApiError> {
    let raw = CursorPayloadRef {
        order: params.order,
        search: params.search,
        directory: params.directory,
        workspace: params.workspace,
        anchor: CursorAnchorRef {
            id: session.id(),
            time: session.created_millis(),
            direction,
        },
    };
    let bytes = serde_json::to_vec(&raw).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
