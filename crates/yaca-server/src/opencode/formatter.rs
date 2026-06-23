use std::collections::BTreeMap;

use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};

use crate::{FormatterStatus, ServerState};

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/formatter", get(status))
}

async fn status(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<Vec<FormatterStatus>> {
    if st.formatter_status.is_empty() {
        let location = super::location::LocationRef::from_request(&query, &headers);
        let workdir = super::location::workdir_at(&st, &location);
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
