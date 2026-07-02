use axum::http::{HeaderName, HeaderValue, header};
use axum::response::{IntoResponse, Response};

pub(super) fn compat(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}
