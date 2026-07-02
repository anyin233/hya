use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use hya_core::CreateSession;
use hya_proto::{MessageId, Projection};
use serde::Deserialize;

use crate::{ApiError, ServerState, parse_session};

#[derive(Deserialize)]
struct ForkPayload {
    #[serde(rename = "messageID")]
    message_id: Option<String>,
}

pub(super) async fn fork(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let before = parse_body(&body)?;
    let source = parse_session(&id)?;
    let envs = st.engine.replay(source).await?;
    if envs.is_empty() {
        return Ok(super::errors::legacy_session_not_found(source));
    }
    let projection = Projection::from_events(&envs);
    let source_info = super::load_session(&st, source, None).await?.info;
    let target = st
        .engine
        .create(CreateSession {
            parent: None,
            agent: projection
                .session
                .agent
                .clone()
                .unwrap_or_else(|| st.agent.name.clone()),
            model: projection
                .session
                .model
                .clone()
                .unwrap_or_else(|| st.agent.model.clone()),
            workdir: st.agent.workdir.to_string_lossy().into_owned(),
        })
        .await?;
    st.engine
        .set_title(target, forked_title(source_info.title()))
        .await?;
    if let Some(metadata) = projection.session.metadata.clone() {
        st.engine.set_metadata(target, metadata).await?;
    }
    st.engine
        .copy_messages_to_session(target, &projection, before)
        .await?;
    Ok(Json(super::load_session(&st, target, None).await?.info).into_response())
}

fn parse_body(body: &[u8]) -> Result<Option<MessageId>, ApiError> {
    let text = std::str::from_utf8(body).map_err(|_| ApiError::bad_request("invalid json"))?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let payload: ForkPayload =
        serde_json::from_str(text).map_err(|_| ApiError::bad_request("invalid json"))?;
    payload.message_id.as_deref().map(parse_message).transpose()
}

fn parse_message(id: &str) -> Result<MessageId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid message id"))
}

fn forked_title(title: &str) -> String {
    let Some(stripped) = title.strip_suffix(')') else {
        return format!("{title} (fork #1)");
    };
    let Some((base, number)) = stripped.rsplit_once(" (fork #") else {
        return format!("{title} (fork #1)");
    };
    match number.parse::<u64>() {
        Ok(value) => format!("{base} (fork #{})", value.saturating_add(1)),
        Err(_) => format!("{title} (fork #1)"),
    }
}
