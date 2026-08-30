use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hya_proto::{Envelope, Event, SessionId, now_millis};
use serde_json::json;

use crate::{ApiError, ServerState, parse_session};

pub(super) async fn prompt_async(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<super::session_prompt_legacy::PromptPayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let text = super::session_prompt_legacy::prompt_text(&req)?;
    match super::load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }
    let run_state = st.clone();
    let engine = st.engine.clone();
    let turn = super::reference::session_agent_with_guidance(&st, session).await;
    let external_dirs = super::reference::external_directories_at(&st, &turn.agent.workdir).await;
    std::mem::drop(tokio::spawn(async move {
        let Some(run) = run_state.start_run(session) else {
            publish_prompt_error(&engine, session, "prompt_async", "session busy".to_string())
                .await;
            return;
        };
        let cancel = run.token();
        let guard = run;
        publish_session_status(&engine, session, "busy").await;
        let result = async {
            engine.admit_user_prompt(session, text).await?;
            let _ = engine.auto_title_session(session, &turn.agent.model).await;
            engine
                .run_turn_with_external_dirs_and_guidance(
                    session,
                    &turn.agent,
                    cancel,
                    &external_dirs,
                    turn.guidance,
                )
                .await?;
            Ok::<(), hya_core::CoreError>(())
        }
        .await;
        if let Err(error) = result {
            publish_prompt_error(&engine, session, "prompt_async", error.to_string()).await;
        }
        drop(guard);
        publish_session_status(&engine, session, "idle").await;
    }));
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Publish a durable background Session status and notify live subscribers.
pub(super) async fn publish_session_status(
    engine: &hya_core::SessionEngine,
    session: SessionId,
    status_type: &'static str,
) {
    publish_background_event(
        engine,
        session,
        Event::SessionStatus {
            session,
            status: json!({ "type": status_type }),
        },
    )
    .await;
}

/// Publish a durable prompt error with its route code and notify live subscribers.
pub(super) async fn publish_prompt_error(
    engine: &hya_core::SessionEngine,
    session: SessionId,
    code: &'static str,
    message: String,
) {
    let event = Event::Error {
        session: Some(session),
        code: code.to_string(),
        message: message.chars().take(2_048).collect(),
    };
    publish_background_event(engine, session, event).await;
}

async fn publish_background_event(
    engine: &hya_core::SessionEngine,
    session: SessionId,
    event: Event,
) {
    let Ok(seq) = engine.store().append_event(session, &event).await else {
        return;
    };
    engine.bus().publish(Envelope {
        seq,
        ts_millis: now_millis(),
        event,
    });
}
