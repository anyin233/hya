use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::Event as SseEvent;
use futures::StreamExt;
use futures::stream;

pub(super) fn stream<F>(event: F) -> impl futures::Stream<Item = Result<SseEvent, Infallible>>
where
    F: Fn() -> SseEvent,
{
    stream::unfold(
        (tokio::time::interval(Duration::from_secs(10)), event),
        |(mut interval, event)| async move {
            interval.tick().await;
            Some((Ok(event()), (interval, event)))
        },
    )
    .skip(1)
}
