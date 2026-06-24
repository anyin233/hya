//! SSE consumer for `GET /global/event`.
//!
//! VERIFIED frame shape: `data: {"payload":{"id":..,"type":"server.connected",...}}`.
//! Unparseable / unknown frames are tolerated (logged-and-skipped), never fatal.

use eventsource_stream::Eventsource;
use futures_util::StreamExt;

use crate::error::{Result, SdkError};
use crate::types::GlobalEvent;

/// Stream global events, invoking `on_event` for each decoded [`GlobalEvent`].
///
/// `on_event` returns `true` to keep streaming or `false` to stop. The stream also ends
/// when the server closes the connection.
///
/// # Errors
/// [`SdkError::EventStream`] if the request fails or the underlying transport errors.
pub async fn stream_global_events<F>(
    http: &reqwest::Client,
    base_url: &str,
    directory: &str,
    mut on_event: F,
) -> Result<()>
where
    F: FnMut(GlobalEvent) -> bool,
{
    let resp = http
        .get(format!("{base_url}/global/event"))
        .header(crate::DIRECTORY_HEADER, directory)
        .send()
        .await
        .map_err(|e| SdkError::EventStream(e.to_string()))?;

    let mut stream = resp.bytes_stream().eventsource();
    while let Some(event) = stream.next().await {
        let event = event.map_err(|e| SdkError::EventStream(e.to_string()))?;
        if event.data.is_empty() {
            continue;
        }
        // Tolerate frames we cannot decode (forward-compat with new event shapes).
        if let Ok(global) = serde_json::from_str::<GlobalEvent>(&event.data) {
            if !on_event(global) {
                break;
            }
        }
    }
    Ok(())
}
