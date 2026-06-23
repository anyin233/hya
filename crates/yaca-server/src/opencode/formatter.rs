use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::{FormatterStatus, ServerState};

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/formatter", get(status))
}

async fn status(State(st): State<ServerState>) -> Json<Vec<FormatterStatus>> {
    if st.formatter_status.is_empty() {
        let workdir =
            std::fs::canonicalize(&st.agent.workdir).unwrap_or_else(|_| st.agent.workdir.clone());
        return Json(
            st.engine
                .formatter()
                .status(&workdir)
                .await
                .unwrap_or_default(),
        );
    }
    Json(st.formatter_status)
}
