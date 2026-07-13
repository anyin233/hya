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
    let runs = st.runs.clone();
    let engine = st.engine.clone();
    let agent = super::reference::session_agent_with_guidance(&st, session).await;
    let external_dirs = super::reference::external_directories_at(&st, &agent.workdir).await;
    std::mem::drop(tokio::spawn(async move {
        let Some(run) = runs.start(session) else {
            publish_background_error(&engine, session, "session busy".to_string()).await;
            return;
        };
        let cancel = run.token();
        let guard = run;
        publish_session_status(&engine, session, "busy").await;
        let result = async {
            engine.admit_user_prompt(session, text).await?;
            let _ = engine.auto_title_session(session, &agent.model).await;
            engine
                .run_turn_with_external_dirs(session, &agent, cancel, &external_dirs)
                .await?;
            Ok::<(), hya_core::CoreError>(())
        }
        .await;
        if let Err(error) = result {
            publish_background_error(&engine, session, error.to_string()).await;
        }
        drop(guard);
        publish_session_status(&engine, session, "idle").await;
    }));
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn publish_session_status(
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

async fn publish_background_error(
    engine: &hya_core::SessionEngine,
    session: SessionId,
    message: String,
) {
    let event = Event::Error {
        session: Some(session),
        code: "prompt_async".to_string(),
        message,
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
