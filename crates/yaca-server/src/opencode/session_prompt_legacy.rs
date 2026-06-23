use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

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
    parts: Vec<PromptPart>,
}

#[derive(Deserialize)]
struct PromptPart {
    #[serde(rename = "type")]
    part_type: String,
    text: Option<String>,
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
    let message = st
        .engine
        .admit_user_prompt(session, prompt_text(req)?)
        .await?;
    if !no_reply {
        let Some(run) = st.runs.start(session) else {
            return Ok(super::errors::session_busy(session));
        };
        let _finish = st.engine.run_turn(session, &st.agent, run.token()).await?;
    }
    Ok(Json(super::session_legacy::load_message(&st, session, message).await?).into_response())
}

fn prompt_text(req: PromptPayload) -> Result<String, ApiError> {
    if let Some(text) = req.text
        && !text.trim().is_empty()
    {
        return Ok(text);
    }
    let text = req
        .parts
        .into_iter()
        .filter(|part| part.part_type == "text")
        .filter_map(|part| part.text)
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return Err(ApiError::bad_request("prompt requires text"));
    }
    Ok(text)
}
