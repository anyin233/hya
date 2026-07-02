use hya_proto::Envelope;
use tokio::sync::broadcast;

/// Default live-broadcast capacity. Raised well above the original 1024 so bursts
/// from 100+ concurrently-streaming subagents do not immediately lag SSE/WebSocket
/// subscribers into a resync. Memory cost is `capacity * sizeof(Envelope)`, which is
/// trivial. The production runtime can override this from config/env.
pub const DEFAULT_BUS_CAPACITY: usize = 8192;

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Envelope>,
    capacity: usize,
}

impl EventBus {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        // A zero capacity would panic in `broadcast::channel`; clamp to at least 1.
        let capacity = capacity.max(1);
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx, capacity }
    }

    /// The configured live-broadcast capacity (buffered envelopes before a slow
    /// subscriber lags).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn publish(&self, envelope: Envelope) {
        let _ = self.tx.send(envelope);
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_BUS_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_configurable_and_default_is_raised() {
        assert_eq!(EventBus::new(4096).capacity(), 4096);
        assert_eq!(EventBus::default().capacity(), DEFAULT_BUS_CAPACITY);
        assert_eq!(DEFAULT_BUS_CAPACITY, 8192);
        // Zero is clamped rather than panicking.
        assert_eq!(EventBus::new(0).capacity(), 1);
    }
}
