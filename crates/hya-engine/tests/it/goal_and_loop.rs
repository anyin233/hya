//! Integration tests for `hya-engine`'s shared [`IterationDriver`] primitive.
//!
//! Goal-mode driver/evaluator tests live in `bundles/builtin/goal/native/src/lib.rs`,
//! and loop-mode driver/verifier/planner tests live in `bundles/builtin/loop/native/src/tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use hya_engine::completion::{IterationDriver, IterationExecutor};
use hya_engine::{CoreError, GateOutcome, IterationGate, RunOutcome, SafetyCaps};
use tokio_util::sync::CancellationToken;

fn caps_max(max_iterations: u32) -> SafetyCaps {
    SafetyCaps {
        max_iterations,
        ..SafetyCaps::default()
    }
}

struct WrapFailure {
    calls: AtomicUsize,
    cancel: bool,
}

#[async_trait]
impl IterationExecutor for WrapFailure {
    async fn run_iteration(&self, _: &str, token: &CancellationToken) -> Result<String, CoreError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok("latest worker evidence".into());
        }
        if self.cancel {
            token.cancel();
            return Err(CoreError::Cancelled);
        }
        Err(CoreError::CapacityExceeded {
            requested: 1,
            limit: 1,
        })
    }
}

struct ContinueGate;

#[async_trait]
impl IterationGate for ContinueGate {
    async fn judge(&self, _: &str, _: &CancellationToken) -> Result<GateOutcome, CoreError> {
        Ok(GateOutcome::Continue {
            directive: "continue".into(),
        })
    }
}

#[tokio::test]
async fn goal_wrap_up_failure_propagates_capacity_and_preserves_cancellation() {
    let cap_res = IterationDriver::new(caps_max(1))
        .run(
            &WrapFailure {
                calls: AtomicUsize::new(0),
                cancel: false,
            },
            &ContinueGate,
            "work".into(),
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(cap_res, Err(CoreError::CapacityExceeded { .. })),
        "{cap_res:?}"
    );

    let cancel_res = IterationDriver::new(caps_max(1))
        .run(
            &WrapFailure {
                calls: AtomicUsize::new(0),
                cancel: true,
            },
            &ContinueGate,
            "work".into(),
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(cancel_res, Ok(RunOutcome::Cancelled)),
        "{cancel_res:?}"
    );
}
