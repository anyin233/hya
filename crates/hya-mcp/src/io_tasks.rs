//! Completion tracking for connection I/O, including canceled initialization.
//!
//! Connection guards still own cancellation and their join handles. Composition
//! scopes this tracker so shutdown can also await cleanup of unpublished sources.
use std::future::Future;
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;

tokio::task_local! {
    static TRACKER: Option<TaskTracker>;
}

/// Capture the composition tracker for a connection's later restart or drop.
#[must_use]
pub fn current() -> Option<TaskTracker> {
    TRACKER.try_with(Clone::clone).ok().flatten()
}

/// Carry completion tracking across a task boundary.
pub fn scope<F: Future>(
    tracker: Option<TaskTracker>,
    future: F,
) -> impl Future<Output = F::Output> {
    TRACKER.scope(tracker, future)
}

/// Spawn connection I/O under the current composition completion barrier.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn_in(current(), future)
}

/// Spawn cleanup using the tracker captured by its connection owner.
pub fn spawn_in<F>(tracker: Option<TaskTracker>, future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let scoped = scope(tracker.clone(), future);
    match tracker {
        Some(tracker) => tracker.spawn(scoped),
        None => tokio::spawn(scoped),
    }
}
