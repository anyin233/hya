use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hya_proto::{MessageId, PartId, ToolPartState};
use serde::Deserialize;
use serde_json::Value;

use crate::{ApiError, ServerState, parse_session};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(super) enum PartUpdatePayload {
    Text {
        id: String,
        #[serde(rename = "sessionID")]
        session_id: String,
        #[serde(rename = "messageID")]
        message_id: String,
        text: String,
    },
    Reasoning {
        id: String,
        #[serde(rename = "sessionID")]
        session_id: String,
        #[serde(rename = "messageID")]
        message_id: String,
        text: String,
    },
    Tool {
        id: String,
        #[serde(rename = "sessionID")]
        session_id: String,
        #[serde(rename = "messageID")]
        message_id: String,
        state: Value,
    },
}

impl PartUpdatePayload {
    fn ids(&self) -> (&str, &str, &str) {
        match self {
            Self::Text {
                id,
                session_id,
                message_id,
                ..
            }
            | Self::Reasoning {
                id,
                session_id,
                message_id,
                ..
            }
            | Self::Tool {
                id,
                session_id,
                message_id,
                ..
            } => (id, session_id, message_id),
        }
    }

    fn part_type(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Reasoning { .. } => "reasoning",
            Self::Tool { .. } => "tool",
        }
    }

    fn ensure_path_ids(&self, session: &str, message: &str, part: &str) -> Result<(), ApiError> {
        let (payload_part, payload_session, payload_message) = self.ids();
        if payload_part != part || payload_session != session || payload_message != message {
            return Err(ApiError::bad_request("part payload id mismatch"));
        }
        Ok(())
    }
}

pub(super) async fn update_part(
    State(st): State<ServerState>,
    Path((id, message, part)): Path<(String, String, String)>,
    Json(payload): Json<PartUpdatePayload>,
) -> Result<Response, ApiError> {
    payload.ensure_path_ids(&id, &message, &part)?;
    let session = parse_session(&id)?;
    let message_id = parse_message(&message)?;
    let part_id = parse_part(&part)?;
    let snapshot = match super::load_session(&st, session, None).await {
        Ok(snapshot) => snapshot,
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    };
    let Some(found) = snapshot
        .messages
        .iter()
        .find(|item| item.id() == message.as_str())
    else {
        return Err(ApiError::not_found("message not found"));
    };
    let Some(existing) = found.part(&part) else {
        return Err(ApiError::not_found("part not found"));
    };
    if existing["type"].as_str() != Some(payload.part_type()) {
        return Err(ApiError::bad_request("part type mismatch"));
    }
    match payload {
        PartUpdatePayload::Text { text, .. } => {
            st.engine
                .replace_text_part(session, message_id, part_id, text)
                .await?;
        }
        PartUpdatePayload::Reasoning { text, .. } => {
            st.engine
                .replace_reasoning_part(session, message_id, part_id, text)
                .await?;
        }
        PartUpdatePayload::Tool { state, .. } => {
            let state = parse_tool_state(state)?;
            st.engine
                .update_tool_part(session, message_id, part_id, state)
                .await?;
        }
    }
    let updated = super::session_legacy::load_message(&st, session, message_id).await?;
    let part = updated
        .part(&part_id.to_string())
        .ok_or_else(|| ApiError::not_found("part not found"))?;
    Ok(Json(part).into_response())
}

fn parse_message(id: &str) -> Result<MessageId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid message id"))
}

fn parse_part(id: &str) -> Result<PartId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid part id"))
}

fn parse_tool_state(state: Value) -> Result<ToolPartState, ApiError> {
    serde_json::from_value::<ToolPartState>(state.clone())
        .or_else(|_| serde_json::from_value::<OpenCodeToolState>(state).map(Into::into))
        .map_err(|_| ApiError::bad_request("invalid tool state"))
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum OpenCodeToolState {
    Pending {
        input: Value,
    },
    Running {
        input: Value,
    },
    Completed {
        input: Value,
        output: String,
        time: Option<OpenCodeToolTime>,
    },
    Error {
        input: Value,
        error: Value,
    },
}

#[derive(Deserialize)]
struct OpenCodeToolTime {
    start: Option<u64>,
    end: Option<u64>,
}

impl From<OpenCodeToolState> for ToolPartState {
    fn from(state: OpenCodeToolState) -> Self {
        match state {
            OpenCodeToolState::Pending { input } => Self::Pending { input },
            OpenCodeToolState::Running { input } => Self::Running { input },
            OpenCodeToolState::Completed {
                input,
                output,
                time,
            } => Self::Completed {
                input,
                output: Value::String(output),
                time_ms: elapsed_ms(time.as_ref()),
            },
            OpenCodeToolState::Error { input, error } => Self::Error {
                input,
                message: error_message(&error),
                value: Some(error),
            },
        }
    }
}

fn error_message(error: &Value) -> String {
    error
        .pointer("/error/message")
        .or_else(|| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| match error {
            Value::String(message) => message.clone(),
            other => other.to_string(),
        })
}

fn elapsed_ms(time: Option<&OpenCodeToolTime>) -> u64 {
    let Some(time) = time else {
        return 0;
    };
    match (time.start, time.end) {
        (Some(start), Some(end)) => end.saturating_sub(start),
        _ => 0,
    }
}
