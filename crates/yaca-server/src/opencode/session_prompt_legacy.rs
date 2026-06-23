use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;

use crate::{ApiError, ServerState, parse_session};

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/session/:id/message", post(prompt))
}

#[derive(Deserialize)]
struct PromptPayload {
    #[serde(default, rename = "noReply")]
    no_reply: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    parts: Vec<Value>,
}

async fn prompt(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<PromptPayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match super::load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }

    let no_reply = req.no_reply;
    let text = prompt_text(&req)?;
    let files = prompt_parts(&req, "file");
    let agents = prompt_parts(&req, "agent");
    let message = st.engine.admit_user_prompt(session, text).await?;
    st.engine
        .record_user_prompt_context(session, message, files, agents)
        .await?;
    if !no_reply {
        let Some(run) = st.runs.start(session) else {
            return Ok(super::errors::session_busy(session));
        };
        let agent = super::reference::agent_with_guidance(&st).await;
        let external_dirs = super::reference::external_directories(&st).await;
        let _finish = st
            .engine
            .run_turn_with_external_dirs(session, &agent, run.token(), &external_dirs)
            .await?;
    }
    Ok(Json(super::session_legacy::load_message(&st, session, message).await?).into_response())
}

fn prompt_text(req: &PromptPayload) -> Result<String, ApiError> {
    if let Some(text) = &req.text
        && !text.trim().is_empty()
    {
        return Ok(text.clone());
    }
    let text = req
        .parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return Err(ApiError::bad_request("prompt requires text"));
    }
    Ok(text)
}

fn prompt_parts(req: &PromptPayload, part_type: &str) -> Vec<Value> {
    req.parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some(part_type))
        .cloned()
        .collect()
}
