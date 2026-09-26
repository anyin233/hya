//! Reconnect pacing for the host control stream.
//!
//! [`Backoff`] implements [`ReconnectPolicy`]: jittered exponential backoff
//! (1 s doubling to a 60 s cap by default), a separate, slower schedule when
//! the proxy reports `ALREADY_EXISTS` (another process holds the same room
//! identity, so hammering it would only flip the room back and forth), and a
//! reset once a connection has stayed up for a stable period.
//!
//! The loop that uses it (the host connector) looks like:
//!
//! ```ignore
//! let mut backoff = Backoff::new(ReconnectPolicy::default());
//! loop {
//!     let kind = match client.host().await {
//!         Ok(mut control) => match register_host(&mut control, &key, &token_hash, deadline).await {
//!             Ok(registration) => {
//!                 backoff.connected();
//!                 // Serve `incoming` until the control stream ends; an
//!                 // `Err(e)` item gives `RetryKind::of_transport_error(&e)`.
//!                 serve(control, registration).await
//!             }
//!             Err(error) => RetryKind::of_client_error(&error),
//!         },
//!         Err(error) => RetryKind::of_client_error(&error),
//!     };
//!     tokio::time::sleep(backoff.next_delay(kind)).await;
//! }
//! ```

use std::time::Duration;

use rand_core::{OsRng, TryRngCore};
use tokio::time::Instant;

use crate::proto::RelayErrorCode;
use crate::transport::TransportError;

use super::ClientError;

/// Timing of [`Backoff`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// First delay after an ordinary failure (default 1 s).
    pub initial: Duration,
    /// Largest ordinary delay (default 60 s).
    pub max: Duration,
    /// First delay after `ALREADY_EXISTS` (default 30 s).
    pub conflict_initial: Duration,
    /// Largest delay after repeated `ALREADY_EXISTS` (default 10 min).
    pub conflict_max: Duration,
    /// A connection that stayed up this long resets the schedule (default
    /// 60 s).
    pub stable_after: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            conflict_initial: Duration::from_secs(30),
            conflict_max: Duration::from_secs(600),
            stable_after: Duration::from_secs(60),
        }
    }
}

/// Why the previous attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    /// Any failure or disconnect.
    Normal,
    /// The proxy said `ALREADY_EXISTS`: another control stream holds the room.
    Conflict,
}

impl RetryKind {
    /// Classify a client error.
    #[must_use]
    pub fn of_client_error(error: &ClientError) -> Self {
        Self::of_code(error.code())
    }

    /// Classify a transport error (for example the control stream's end).
    #[must_use]
    pub fn of_transport_error(error: &TransportError) -> Self {
        match error {
            TransportError::Status { code, .. } => Self::of_code(Some(*code)),
            _ => RetryKind::Normal,
        }
    }

    fn of_code(code: Option<RelayErrorCode>) -> Self {
        if code == Some(RelayErrorCode::AlreadyExists) {
            RetryKind::Conflict
        } else {
            RetryKind::Normal
        }
    }
}

/// The reconnect schedule of one control stream.
#[derive(Debug, Clone)]
pub struct Backoff {
    policy: ReconnectPolicy,
    attempts: u32,
    conflicts: u32,
    connected_at: Option<Instant>,
    rng: u64,
}

impl Backoff {
    /// A fresh schedule with a random jitter seed.
    #[must_use]
    pub fn new(policy: ReconnectPolicy) -> Self {
        let seed = OsRng.try_next_u64().unwrap_or(0x9e37_79b9_7f4a_7c15);
        Self::with_seed(policy, seed)
    }

    /// A fresh schedule with a fixed jitter seed (deterministic tests).
    #[must_use]
    pub fn with_seed(policy: ReconnectPolicy, seed: u64) -> Self {
        Self {
            policy,
            attempts: 0,
            conflicts: 0,
            connected_at: None,
            rng: seed,
        }
    }

    /// The policy.
    #[must_use]
    pub fn policy(&self) -> &ReconnectPolicy {
        &self.policy
    }

    /// Record that a connection is up (the room is registered). If it stays
    /// up for [`ReconnectPolicy::stable_after`], the next delay starts over.
    pub fn connected(&mut self) {
        self.connected_at = Some(Instant::now());
    }

    /// Start over, as after a stable connection.
    pub fn reset(&mut self) {
        self.attempts = 0;
        self.conflicts = 0;
        self.connected_at = None;
    }

    /// The delay before the next attempt, after an attempt that ended with
    /// `kind`. Each call moves further along the schedule.
    pub fn next_delay(&mut self, kind: RetryKind) -> Duration {
        if let Some(at) = self.connected_at.take()
            && at.elapsed() >= self.policy.stable_after
        {
            self.attempts = 0;
            self.conflicts = 0;
        }
        let (initial, cap, count) = match kind {
            RetryKind::Normal => (self.policy.initial, self.policy.max, &mut self.attempts),
            RetryKind::Conflict => (
                self.policy.conflict_initial,
                self.policy.conflict_max,
                &mut self.conflicts,
            ),
        };
        let exponent = (*count).min(30);
        *count = count.saturating_add(1);
        let ceiling = initial.saturating_mul(1u32 << exponent).min(cap);
        // "Equal jitter": half fixed, half random, so delays never collapse
        // to zero and concurrent hosts spread out.
        let half = ceiling / 2;
        let spread = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
        let random = if spread == 0 {
            0
        } else {
            self.next_random() % spread.saturating_add(1)
        };
        half + Duration::from_nanos(random)
    }

    /// splitmix64: plenty for jitter, and reproducible from a seed.
    fn next_random(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}
