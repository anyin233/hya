//! Ending live event streams at shutdown (ADR-0023).
//!
//! `StreamSessionEvents` / `StreamGlobalEvents` streams (HTTP SSE and gRPC)
//! never finish on their own, so a graceful shutdown would wait for every
//! connected client to hang up. The backend is a daemon that outlives its
//! clients (`hya serve stop` must work while TUIs are attached), so
//! [`StreamShutdown::close`] ends every open stream, and every stream opened
//! later, and makes `GET /v1/health` answer `unavailable`. Clients see the
//! stream end, probe health, and reconnect to (or start) the next server.

use std::sync::Arc;

use tokio::sync::watch;

/// The shutdown signal shared by every live stream of one server.
#[derive(Clone, Debug)]
pub struct StreamShutdown {
    closing: Arc<watch::Sender<bool>>,
}

impl Default for StreamShutdown {
    fn default() -> Self {
        let (closing, _) = watch::channel(false);
        Self {
            closing: Arc::new(closing),
        }
    }
}

impl StreamShutdown {
    /// End every open stream (and every stream opened from now on) and make
    /// `GET /v1/health` answer `unavailable`. Idempotent.
    pub fn close(&self) {
        self.closing.send_replace(true);
    }

    /// Whether [`close`](Self::close) was called.
    #[must_use]
    pub fn is_closing(&self) -> bool {
        *self.closing.borrow()
    }

    /// Resolves once [`close`](Self::close) is called (at once if it was).
    pub(crate) fn closed(&self) -> impl std::future::Future<Output = ()> + Send + use<> {
        let mut receiver = self.closing.subscribe();
        async move {
            // `wait_for` fails only when the sender is gone, which cannot
            // happen while the state holding it lives; never resolve then.
            if receiver.wait_for(|closing| *closing).await.is_err() {
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
        shutdown.close();
        assert!(waiting.await.is_ok());
        assert!(shutdown.is_closing());
        shutdown.closed().await;
    }
}
