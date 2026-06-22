use crate::ServerState;
use axum::Router;
use axum::routing::get;

mod fs;
mod ignore;
mod legacy;
mod mime;
mod path;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/find", get(legacy::find_text))
        .route("/find/file", get(legacy::find_file))
        .route(
            "/find/symbol",
            get(legacy::empty_array::<legacy::EmptyQuery>),
        )
        .route("/file", get(legacy::list))
        .route("/file/content", get(legacy::content))
        .route(
            "/file/status",
            get(legacy::empty_array::<legacy::EmptyQuery>),
        )
        .route("/api/fs/read/*path", get(fs::read))
        .route("/api/fs/list", get(fs::list))
        .route("/api/fs/find", get(fs::find))
}
