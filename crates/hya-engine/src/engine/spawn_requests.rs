use std::sync::Arc;

use hya_tool::{SpawnRequest, SpawnRequestSendError, SpawnRequestSink, SpawnerPlane};

use crate::TurnBinding;

/// One spawn request bound to the immutable runtime snapshot of its parent turn.
///
/// This value is process-local orchestration state. It is never persisted or
/// exposed on the wire; dropping it naturally releases the retained snapshot.
pub struct BoundSpawnRequest {
    binding: TurnBinding,
    request: SpawnRequest,
}

impl BoundSpawnRequest {
    /// Consume into the retained turn binding and the raw tool-plane spawn request.
    #[must_use]
    pub fn into_parts(self) -> (TurnBinding, SpawnRequest) {
        (self.binding, self.request)
    }
}

/// Core-owned sender for parent-turn-bound spawn requests.
#[derive(Clone)]
pub struct BoundSpawnSender {
    tx: tokio::sync::mpsc::Sender<BoundSpawnRequest>,
}

impl BoundSpawnSender {
    /// Create a bounded channel pair for the app's spawn worker loop.
    #[must_use]
    pub fn with_capacity(
        capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<BoundSpawnRequest>) {
        let capacity = capacity.clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    pub(super) fn disconnected() -> Self {
        let (sender, receiver) = Self::with_capacity(1);
        drop(receiver);
        sender
    }

    /// Derive a raw tool-plane facade closed over one already-admitted turn.
    #[must_use]
    pub fn for_binding(&self, binding: &TurnBinding) -> SpawnerPlane {
        SpawnerPlane::from_sink(Arc::new(BoundSpawnRequestSink {
            tx: self.tx.clone(),
            binding: binding.clone(),
        }))
    }
}

struct BoundSpawnRequestSink {
    tx: tokio::sync::mpsc::Sender<BoundSpawnRequest>,
    binding: TurnBinding,
}

impl SpawnRequestSink for BoundSpawnRequestSink {
    fn try_send(&self, request: SpawnRequest) -> Result<(), SpawnRequestSendError> {
        self.tx
            .try_send(BoundSpawnRequest {
                binding: self.binding.clone(),
                request,
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => SpawnRequestSendError::Full,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => SpawnRequestSendError::Closed,
            })
    }
}
