use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::Event as SseEvent;
use futures::StreamExt;
use futures::stream;

pub(super) fn stream(
    event: fn() -> SseEvent,
) -> impl futures::Stream<Item = Result<SseEvent, Infallible>> {
    stream::unfold(
        tokio::time::interval(Duration::from_secs(10)),
        move |mut interval| async move {
            interval.tick().await;
            Some((Ok(event()), interval))
        },
    )
    .skip(1)
}
