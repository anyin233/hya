//! Cancellation and joining for runtime services and their admitted children.

use std::future::Future;

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Owns runtime services until explicit shutdown. Drop signals cancellation;
/// retain the owner and await [`Self::shutdown`] to join the services.
#[must_use]
#[derive(Default)]
pub struct RuntimeTaskOwner {
    /// Shutdown signal shared by the owned services.
    pub stop: CancellationToken,
    /// Services and admitted children that shutdown must join.
    pub tasks: TaskTracker,
    /// Plugin and MCP connection I/O. BuiltSessionEngine::drain and the standalone
    /// spawn_team_supervisor owner join it after stopping residents and sources;
    /// AppState owns no connection I/O.
    pub io_tasks: TaskTracker,
}

impl RuntimeTaskOwner {
    /// Create an accepting runtime task owner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel intake and wait for every tracked service and handler to exit.
    pub fn shutdown(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        let stop = self.stop.clone();
        let tasks = self.tasks.clone();
        async move {
            stop.cancel();
            tasks.close();
            tasks.wait().await;
        }
    }
}

impl Drop for RuntimeTaskOwner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Register before checking cancellation, so shutdown joins racing admissions.
/// Services keep these handles, never the owner that cancels on drop.
pub(crate) fn spawn_cancellable(
    tasks: &TaskTracker,
    stop: &CancellationToken,
    future: impl Future<Output = ()> + Send + 'static,
) {
    let tracked = tasks.token();
    if stop.is_cancelled() {
        return;
    }
    let stop = stop.clone();
    tokio::spawn(async move {
        let _tracked = tracked;
        tokio::select! {
            biased;
            _ = stop.cancelled() => {},
            () = future => {},
        }
    });
}
