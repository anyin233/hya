use axum::Json;
use axum::extract::{Path, State};

use crate::{ApiError, ServerState, parse_session};

pub(super) async fn share(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<super::projection::OpenCodeSessionInfo>, ApiError> {
    let session = parse_session(&id)?;
    super::load_session(&st, session, None).await?;
    st.engine
        .set_share(session, format!("yaca://session/{session}"))
        .await?;
    Ok(Json(super::load_session(&st, session, None).await?.info))
}

pub(super) async fn unshare(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<super::projection::OpenCodeSessionInfo>, ApiError> {
    let session = parse_session(&id)?;
    super::load_session(&st, session, None).await?;
    st.engine.clear_share(session).await?;
    Ok(Json(super::load_session(&st, session, None).await?.info))
}
