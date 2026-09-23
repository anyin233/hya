use eventsource_stream::Eventsource as _;
use futures::StreamExt as _;
use hya_proto::{Event, MessageId, SessionId};
use serde_json::Value;
use tokio::{sync::mpsc, time::timeout};

use crate::{
    Decoder, Protocol, ProviderError, ReasoningEffort, stream_error::classify_error_frame,
};

use super::RouteCore;

/// Everything the zero-event replay window needs to re-issue a streamed
/// completion from inside the pump task: the clonable route state, a shared
/// protocol (fresh decoder per attempt), and the request payload.
pub(super) struct ReissuePlan {
    pub(super) core: RouteCore,
    pub(super) protocol: std::sync::Arc<dyn Protocol>,
    pub(super) url: String,
    pub(super) body: Value,
    pub(super) extra_headers: std::collections::BTreeMap<String, String>,
    pub(super) model_override: Option<String>,
    pub(super) idle_timeout: std::time::Duration,
    /// Attempts already consumed by the pre-stream loop in `stream()`.
    pub(super) attempts_used: usize,
}

/// A stream's terminal failure plus whether it is a *link-level* failure.
///
/// Link-level failures (SSE byte-stream decode errors, connection resets,
/// idle stalls before any frame) hit before the provider's semantics are
/// involved, so replaying the whole request while nothing was consumed is
/// safe. Provider-decided failures are replayed at zero events only when the
/// upstream classed them transient: in-stream error frames classified as
/// rate limit / overload / 5xx ([`ProviderError::is_retryable_before_stream`]).
/// Deterministic ones — invalid request, auth, unclassified error frames,
/// malformed payloads, missing terminal frames — surface immediately:
/// replaying them would only burn the budget.
pub(super) struct TerminalError {
    pub(super) error: ProviderError,
    pub(super) link_level: bool,
}

/// How one pump run over an established response ended.
pub(super) struct PumpOutcome {
    /// Whether any event reached the consumer from this response.
    pub(super) delivered_any: bool,
    /// The consumer dropped the stream; stop silently and abort the body.
    pub(super) consumer_gone: bool,
    /// The terminal error, if the stream ended in failure. The pump itself
    /// never sends errors: the caller decides whether the zero-event replay
    /// window applies or the error is forwarded to the consumer.
    pub(super) terminal: Option<TerminalError>,
}

/// Drive one established SSE response into `tx`, returning how it ended.
pub(super) async fn pump(
    resp: reqwest::Response,
    mut decoder: Box<dyn Decoder>,
    tx: mpsc::Sender<Result<Event, ProviderError>>,
    idle_timeout: std::time::Duration,
) -> PumpOutcome {
    let mut outcome = PumpOutcome {
        delivered_any: false,
        consumer_gone: false,
        terminal: None,
    };
    let mut sse = resp.bytes_stream().eventsource();
    loop {
        // The window opens at headers (first event) and resets on every frame
        // (inter-event silence). A miss is a terminal error under the
        // no-replay boundary: the caller surfaces it exactly once and never
        // replays once events have been delivered.
        // Cancel/drop of the EventStream must abort this HTTP body immediately;
        // otherwise keepalive comments keep `sse.next()` pending until idle.
        let next = tokio::select! {
            biased;
            () = tx.closed() => {
                outcome.consumer_gone = true;
                return outcome;
            }
            next = timeout(idle_timeout, sse.next()) => next,
        };
        let next = match next {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_elapsed) => {
                outcome.terminal = Some(TerminalError {
                    error: ProviderError::Http(format!(
                        "stalled stream: no SSE frame within {idle_timeout:#?}"
                    )),
                    link_level: true,
                });
                return outcome;
            }
        };
        let frame = match next {
            Ok(f) => f,
            Err(e) => {
                outcome.terminal = Some(TerminalError {
                    error: ProviderError::Http(e.to_string()),
                    link_level: true,
                });
                return outcome;
            }
        };
        if frame.data.contains("\"error\"")
            && let Ok(value) = serde_json::from_str::<Value>(&frame.data)
            && let Some(err) = value
                .get("error")
                .filter(|err| err.is_object() || err.is_string())
        {
            outcome.terminal = Some(TerminalError {
                error: classify_error_frame(err, &value, "provider returned an error"),
                link_level: false,
            });
            return outcome;
        }
        match decoder.push(&frame.data) {
            Ok(events) => {
                for event in events {
                    if tx.send(Ok(event)).await.is_err() {
                        outcome.consumer_gone = true;
                        return outcome;
                    }
                    outcome.delivered_any = true;
                }
            }
            Err(e) => {
                outcome.terminal = Some(TerminalError {
                    error: e,
                    link_level: false,
                });
                return outcome;
            }
        }
    }
    match decoder.finish() {
        Ok(events) => {
            for event in events {
                if tx.send(Ok(event)).await.is_err() {
                    outcome.consumer_gone = true;
                    return outcome;
                }
                outcome.delivered_any = true;
            }
        }
        Err(e) => {
            outcome.terminal = Some(TerminalError {
                error: e,
                link_level: false,
            });
        }
    }
    outcome
}

/// Pump the first response, then — while the consumer has seen no events at
/// all — transparently re-issue the whole request inside the remaining
/// [`super::RetryConfig`] budget.
///
/// The moment a single event is delivered the strict no-replay boundary holds:
/// later errors are forwarded to the consumer exactly once and never replayed
/// or failed over. `stream()` has already returned by then, so this task owns
/// the lifecycle.
pub(super) async fn pump_with_reissue(
    mut plan: ReissuePlan,
    mut resp: reqwest::Response,
    session: SessionId,
    message: MessageId,
    reasoning: Option<ReasoningEffort>,
    tx: mpsc::Sender<Result<Event, ProviderError>>,
) {
    loop {
        let decoder = plan.protocol.decoder(session, message, reasoning);
        let outcome = pump(resp, decoder, tx.clone(), plan.idle_timeout).await;
        if outcome.consumer_gone {
            return;
        }
        let Some(terminal) = outcome.terminal else {
            return;
        };
        // STRICT NO-REPLAY: a single delivered event closes the window.
        let replayable = terminal.link_level || terminal.error.is_retryable_before_stream();
        if outcome.delivered_any
            || plan.attempts_used >= plan.core.retry.max_attempts
            || !replayable
        {
            let _ = tx.send(Err(terminal.error)).await;
            return;
        }
        let remaining = plan.core.retry.max_attempts - plan.attempts_used;
        tokio::time::sleep(super::retry_delay(
            &terminal.error,
            plan.attempts_used,
            &plan.core.retry,
        ))
        .await;
        match plan
            .core
            .send_stream_request(
                &plan.url,
                &plan.body,
                &plan.extra_headers,
                plan.model_override.as_deref(),
                remaining,
            )
            .await
        {
            Ok((next_resp, used)) => {
                plan.attempts_used += used;
                resp = next_resp;
            }
            Err(pre_stream) => {
                // The replay itself died before headers (statuses already
                // retried inside their own budget). Surface it as the stream's
                // terminal item: the consumer context is identical to a
                // mid-stream failure.
                let _ = tx.send(Err(pre_stream)).await;
                return;
            }
        }
    }
}
