//! Ending live event streams at shutdown (ADR-0023).
//!
//! `StreamSessionEvents` / `StreamGlobalEvents` streams (HTTP SSE and gRPC)
//! never finish on their own, so a graceful shutdown would wait for every
//! connected client to hang up. The backend is a daemon that outlives its
//! clients (`hya serve stop` must work while TUIs are attached), so
//! [`StreamShutdown::close`] ends every open stream, and every stream opened
//! later, and makes `GET /v1/health` answer `unavailable`. The last frame of
//! each stream is a live `serverStopping {reason}`: after `stop` (or a plain
//! signal) clients stay disconnected, after `restart` they wait for the next
//! server, and a stream that ends without it lost its server unexpectedly,
//! so clients find or start the next one.

use std::sync::Arc;

use tokio::sync::watch;

/// Why a server shuts down; sent to every live stream as its last frame
/// (`serverStopping {reason}`) so clients know whether to start the next
/// server themselves (docs/protocol/README.md "Server shutdown").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownReason {
    /// Stopped on purpose (`hya serve stop`): clients must not start another.
    Stop,
    /// `hya serve restart`: a new server of the same database follows.
    Restart,
    /// A termination signal from anything else (a foreground `hya serve`
    /// interrupted, a supervisor). Clients treat it like [`Self::Stop`].
    Signal,
}

impl ShutdownReason {
    /// The wire value (`ServerStopping.reason`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Signal => "signal",
        }
    }

    /// Parse a wire value; `None` for anything unknown.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            "signal" => Some(Self::Signal),
            _ => None,
        }
    }
}

/// The shutdown signal shared by every live stream of one server.
#[derive(Clone, Debug)]
pub struct StreamShutdown {
    /// `Some(reason)` once closing.
    closing: Arc<watch::Sender<Option<ShutdownReason>>>,
}

impl Default for StreamShutdown {
    fn default() -> Self {
        let (closing, _) = watch::channel(None);
        Self {
            closing: Arc::new(closing),
        }
    }
}

impl StreamShutdown {
    /// End every open stream (and every stream opened from now on) with a
    /// final `serverStopping {reason}` frame, and make `GET /v1/health`
    /// answer `unavailable`. Idempotent: the first reason wins.
    pub fn close(&self, reason: ShutdownReason) {
        self.closing.send_if_modified(|closing| {
            if closing.is_some() {
                return false;
            }
            *closing = Some(reason);
            true
        });
    }

    /// Whether [`close`](Self::close) was called.
    #[must_use]
    pub fn is_closing(&self) -> bool {
        self.closing.borrow().is_some()
    }

    /// The reason given to [`close`](Self::close), once called.
    #[must_use]
    pub fn reason(&self) -> Option<ShutdownReason> {
        *self.closing.borrow()
    }

    /// Resolves once [`close`](Self::close) is called (at once if it was).
    pub(crate) fn closed(&self) -> impl std::future::Future<Output = ()> + Send + use<> {
        let mut receiver = self.closing.subscribe();
        async move {
            // `wait_for` fails only when the sender is gone, which cannot
            // happen while the state holding it lives; never resolve then.
            if receiver.wait_for(Option::is_some).await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closed_resolves_after_close_and_at_once_later() {
        let shutdown = StreamShutdown::default();
        let waiting = tokio::spawn(shutdown.closed());
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        assert!(!shutdown.is_closing());
        shutdown.close(ShutdownReason::Stop);
        assert!(waiting.await.is_ok());
        assert!(shutdown.is_closing());
        shutdown.closed().await;
        assert_eq!(shutdown.reason(), Some(ShutdownReason::Stop));
    }

    #[test]
    fn reasons_round_trip_their_wire_values() {
        for reason in [
            ShutdownReason::Stop,
            ShutdownReason::Restart,
            ShutdownReason::Signal,
        ] {
            assert_eq!(ShutdownReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(ShutdownReason::parse("crash"), None);
    }
}
