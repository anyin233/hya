//! Proxy resource limits (ADR-0025 D8) and the per-stream token bucket.

use std::time::Duration;

use tokio::time::Instant;

/// Resource limits the proxy core enforces (ADR-0025 D8).
///
/// Every field has a conservative default (see [`ProxyLimits::default`]);
/// `hya proxy` exposes them as flags. Nothing is persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyLimits {
    /// Maximum registered rooms. A registration that replaces an existing
    /// room needs no new slot. Over the limit: `RESOURCE_EXHAUSTED`.
    pub max_rooms: usize,
    /// Maximum concurrent streams (waiting for `Accept` or spliced) per
    /// room. Over the limit: `RESOURCE_EXHAUSTED`.
    pub max_streams_per_room: usize,
    /// Maximum concurrent streams opened by one client identity
    /// ([`PeerInfo`](super::PeerInfo)). Over the limit: `RESOURCE_EXHAUSTED`.
    pub max_streams_per_peer: usize,
    /// Maximum rooms registered by one client identity. Replacing a room
    /// the same client already holds needs no new slot. Over the limit:
    /// `RESOURCE_EXHAUSTED`.
    pub max_rooms_per_peer: usize,
    /// Maximum host control streams from one client identity that have not
    /// finished registration yet (challenge sent, no valid `register`).
    /// Over the limit the new control stream gets `RESOURCE_EXHAUSTED`
    /// instead of a challenge.
    pub max_pending_registrations_per_peer: usize,
    /// A stream leg or host control stream that delivers no frame at all
    /// (heartbeats count) for this long is closed with `DEADLINE_EXCEEDED`.
    /// Also bounds how long the proxy waits for a peer to take a frame.
    pub idle_timeout: Duration,
    /// Byte-rate cap per stream and direction (`data` payload bytes per
    /// second); `0` disables it. Excess traffic is delayed, not dropped.
    pub stream_rate_bytes_per_sec: u64,
    /// Token-bucket burst for [`Self::stream_rate_bytes_per_sec`].
    pub stream_rate_burst_bytes: u64,
    /// Largest accepted `Chunk.data` payload. A larger chunk fails the
    /// stream with `RESOURCE_EXHAUSTED`.
    pub max_chunk_data: usize,
    /// `data` bytes buffered from an opener before the host accepted the
    /// stream; beyond it the proxy stops reading the opener (backpressure).
    pub early_data_limit: usize,
    /// How long an `Open` waits for the host's `Accept`; then the opener
    /// fails with `UNAVAILABLE`.
    pub accept_timeout: Duration,
    /// Deadline for the first frame of every stream: the host's `register`
    /// (the registration timeout), an opener's `open`, a host's `accept`.
    /// Then `DEADLINE_EXCEEDED`.
    pub handshake_timeout: Duration,
}

/// Default [`ProxyLimits::max_rooms`].
pub const DEFAULT_MAX_ROOMS: usize = 1024;
/// Default [`ProxyLimits::max_streams_per_room`].
pub const DEFAULT_MAX_STREAMS_PER_ROOM: usize = 64;
/// Default [`ProxyLimits::max_streams_per_peer`].
pub const DEFAULT_MAX_STREAMS_PER_PEER: usize = 256;
/// Default [`ProxyLimits::max_rooms_per_peer`].
pub const DEFAULT_MAX_ROOMS_PER_PEER: usize = 16;
/// Default [`ProxyLimits::max_pending_registrations_per_peer`].
pub const DEFAULT_MAX_PENDING_REGISTRATIONS_PER_PEER: usize = 8;
/// Default [`ProxyLimits::idle_timeout`] (8 missed 15 s heartbeats).
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Default [`ProxyLimits::stream_rate_bytes_per_sec`] (8 MiB/s).
pub const DEFAULT_STREAM_RATE_BYTES_PER_SEC: u64 = 8 * 1024 * 1024;
/// Default [`ProxyLimits::stream_rate_burst_bytes`] (1 MiB).
pub const DEFAULT_STREAM_RATE_BURST_BYTES: u64 = 1024 * 1024;
/// Default [`ProxyLimits::max_chunk_data`] (256 KiB).
pub const DEFAULT_MAX_CHUNK_DATA: usize = 256 * 1024;
/// Default [`ProxyLimits::early_data_limit`] (64 KiB).
pub const DEFAULT_EARLY_DATA_LIMIT: usize = 64 * 1024;
/// Default [`ProxyLimits::accept_timeout`].
pub const DEFAULT_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default [`ProxyLimits::handshake_timeout`].
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

impl Default for ProxyLimits {
    fn default() -> Self {
        Self {
            max_rooms: DEFAULT_MAX_ROOMS,
            max_streams_per_room: DEFAULT_MAX_STREAMS_PER_ROOM,
            max_streams_per_peer: DEFAULT_MAX_STREAMS_PER_PEER,
            max_rooms_per_peer: DEFAULT_MAX_ROOMS_PER_PEER,
            max_pending_registrations_per_peer: DEFAULT_MAX_PENDING_REGISTRATIONS_PER_PEER,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            stream_rate_bytes_per_sec: DEFAULT_STREAM_RATE_BYTES_PER_SEC,
            stream_rate_burst_bytes: DEFAULT_STREAM_RATE_BURST_BYTES,
            max_chunk_data: DEFAULT_MAX_CHUNK_DATA,
            early_data_limit: DEFAULT_EARLY_DATA_LIMIT,
            accept_timeout: DEFAULT_ACCEPT_TIMEOUT,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
        }
    }
}

/// A token bucket that delays (never drops) traffic over its rate.
///
/// Debt model: a chunk always passes, and the caller then waits until the
/// bucket is out of debt, so a chunk larger than the burst is still allowed.
#[derive(Debug)]
pub(crate) struct TokenBucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    /// A bucket for `rate` bytes/s with `burst` bytes of credit, or `None`
    /// when `rate` is 0 (unlimited).
    pub(crate) fn new(rate: u64, burst: u64) -> Option<Self> {
        (rate > 0).then(|| {
            let burst = burst as f64;
            Self {
                rate: rate as f64,
                burst,
                tokens: burst,
                last: Instant::now(),
            }
        })
    }

    /// Charge `bytes` and return how long the caller must wait before
    /// forwarding them.
    pub(crate) fn charge(&mut self, bytes: usize) -> Duration {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        self.tokens -= bytes as f64;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-self.tokens / self.rate)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn bucket_allows_the_burst_then_delays() {
        let Some(mut bucket) = TokenBucket::new(1_000, 500) else {
            panic!("rate > 0 gives a bucket");
        };
        assert_eq!(bucket.charge(500), Duration::ZERO);
        assert_eq!(bucket.charge(250), Duration::from_millis(250));
        tokio::time::advance(Duration::from_millis(250)).await;
        // Back to zero credit; 1 s later the burst is full again (capped).
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(bucket.charge(500), Duration::ZERO);
        assert_eq!(bucket.charge(1_000), Duration::from_secs(1));
    }

    #[test]
    fn zero_rate_is_unlimited() {
        assert!(TokenBucket::new(0, 10).is_none());
    }
}
