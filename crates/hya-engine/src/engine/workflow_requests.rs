use std::sync::Arc;

use crate::TurnBinding;

/// A workflow-tool request bound to the triggering turn's runtime snapshot.
pub struct BoundWorkflowRequest {
    binding: TurnBinding,
    request: hya_tool::WorkflowRequest,
}

impl BoundWorkflowRequest {
    /// Consume into the retained turn binding and the raw tool-plane request.
    #[must_use]
    pub fn into_parts(self) -> (TurnBinding, hya_tool::WorkflowRequest) {
        (self.binding, self.request)
    }
}

/// Core-owned sender for turn-bound workflow requests.
#[derive(Clone)]
pub struct BoundWorkflowSender {
    tx: tokio::sync::mpsc::Sender<BoundWorkflowRequest>,
}

impl BoundWorkflowSender {
    /// Create a bounded channel pair for the host's workflow worker loop.
    #[must_use]
    pub fn with_capacity(
        capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<BoundWorkflowRequest>) {
        let capacity = capacity.clamp(1, 65_535);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    pub(super) fn disconnected() -> Self {
        let (sender, receiver) = Self::with_capacity(1);
        drop(receiver);
        sender
    }

    /// Derive a raw tool-plane facade closed over one already-resolved turn.
    #[must_use]
    pub fn for_binding(&self, binding: &TurnBinding) -> hya_tool::WorkflowPlane {
        hya_tool::WorkflowPlane::from_sink(Arc::new(BoundWorkflowSink {
            tx: self.tx.clone(),
            binding: binding.clone(),
        }))
    }
}

struct BoundWorkflowSink {
    tx: tokio::sync::mpsc::Sender<BoundWorkflowRequest>,
    binding: TurnBinding,
}

impl hya_tool::WorkflowRequestSink for BoundWorkflowSink {
    fn try_send(
        &self,
        request: hya_tool::WorkflowRequest,
    ) -> Result<(), hya_tool::WorkflowSendError> {
        self.tx
            .try_send(BoundWorkflowRequest {
                binding: self.binding.clone(),
                request,
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    hya_tool::WorkflowSendError::Full
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    hya_tool::WorkflowSendError::Closed
                }
            })
    }
}
