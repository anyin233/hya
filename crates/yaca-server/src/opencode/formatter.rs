use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::{FormatterStatus, ServerState};

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/formatter", get(status))
}

async fn status(State(st): State<ServerState>) -> Json<Vec<FormatterStatus>> {
    Json(st.formatter_status)
}
