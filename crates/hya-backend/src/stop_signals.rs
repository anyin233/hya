//! Process stop-signal (`SIGINT`/`SIGTERM`) handling for store-owning commands.

use std::future::Future;
use std::io::Write as _;
use std::time::Duration;

use anyhow::Context as _;
use hya_app::BuiltSessionEngine;
use hya_proto::{FinishCause, SessionId};
use hya_store::SessionStore;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio_util::sync::CancellationToken;

/// A stop signal that ended a one-shot run early.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopSignal {
    /// `SIGINT` (Ctrl-C): the user stopped the run.
    Interrupt,
    /// `SIGTERM`: the process is being stopped.
    Terminate,
}

impl StopSignal {
    pub(crate) fn cause(self) -> FinishCause {
        match self {
            Self::Interrupt => FinishCause::UserCancel,
            Self::Terminate => FinishCause::Shutdown,
        }
    }

    /// Conventional `128 + signal` exit status.
    pub(crate) fn exit_code(self) -> i32 {
        match self {
            Self::Interrupt => 130,
            Self::Terminate => 143,
        }
    }
}

/// Live `SIGINT` and `SIGTERM` streams kept installed across startup, run, and exit drain.
pub(crate) struct StopSignals {
    interrupt: Signal,
    terminate: Signal,
}

impl StopSignals {
    /// Install `SIGINT` and `SIGTERM` handlers before starting store-backed work.
    pub(crate) fn install() -> anyhow::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt()).context("install SIGINT handler")?,
            terminate: signal(SignalKind::terminate()).context("install SIGTERM handler")?,
        })
    }

    /// Wait for the next `SIGINT` or `SIGTERM`.
    pub(crate) async fn recv(&mut self) -> StopSignal {
        tokio::select! {
            _ = self.interrupt.recv() => StopSignal::Interrupt,
            _ = self.terminate.recv() => StopSignal::Terminate,
        }
    }

    /// Await `fut` while letting a subsequent `SIGINT` emit an immediate durability report
    /// and exit with status 130.
    pub(crate) async fn allow_immediate_interrupt<T>(
        &mut self,
        store: &SessionStore,
        fut: impl Future<Output = T>,
    ) -> T {
        tokio::select! {
            output = fut => output,
            _ = self.interrupt.recv() => {
                eprintln!("hya: interrupted again; exiting without finishing the drain");
                let _ = std::io::stdout().flush();
                store.drain_and_report_exit(Duration::ZERO).await;
                std::process::exit(StopSignal::Interrupt.exit_code());
            }
        }
    }

    /// Await a shutdown/drain future while watching both `SIGINT` and `SIGTERM`.
    /// On the first signal in this window (or a repeat signal when `already_stopped`),
    /// reports the current queue durability state immediately and exits 130/143.
    pub(crate) async fn finish_with_store_report<T>(
        &mut self,
        store: &SessionStore,
        already_stopped: bool,
        fut: impl Future<Output = T>,
    ) -> T {
        tokio::select! {
            biased;
            sig = self.recv() => {
                if already_stopped && sig == StopSignal::Interrupt {
                    eprintln!("hya: interrupted again; exiting without finishing the drain");
                }
                let _ = std::io::stdout().flush();
                store.drain_and_report_exit(Duration::ZERO).await;
                exit_stopped(sig);
            }
            output = fut => output,
        }
    }

    /// Drain pending appends up to [`hya_store::EXIT_DRAIN_TIMEOUT`] and emit the exit report
    /// while watching `SIGINT` and `SIGTERM` for immediate report-and-exit.
    pub(crate) async fn drain_store_on_exit(
        &mut self,
        store: &SessionStore,
        already_stopped: bool,
    ) {
        self.finish_with_store_report(
            store,
            already_stopped,
            store.drain_and_report_exit(hya_store::EXIT_DRAIN_TIMEOUT),
        )
        .await;
    }

    /// Drive `work` until completion or stop signal, cancelling `cancel` and waiting up to
    /// `deadline` (if any) while honoring a second `SIGINT` for immediate exit.
    pub(crate) async fn run_cancellable<T>(
        &mut self,
        store: &SessionStore,
        cancel: &CancellationToken,
        deadline: Option<Duration>,
        work: impl Future<Output = T>,
    ) -> (Option<T>, Option<StopSignal>) {
        tokio::pin!(work);
        let stop = tokio::select! {
            output = &mut work => return (Some(output), None),
            stop = self.recv() => stop,
        };
        eprintln!("hya: stopping — draining in-flight turns (Ctrl-C again to exit now)");
        cancel.cancel();
        let finish = async {
            match deadline {
                Some(limit) => tokio::time::timeout(limit, &mut work).await.ok(),
                None => Some(work.await),
            }
        };
        let output = self.allow_immediate_interrupt(store, finish).await;
        (output, Some(stop))
    }
}

/// Start a [`crate::startup::HeadlessSession`] while guarding the startup window with
/// `signals`; on signal, drains and reports the store before exiting with 130/143.
pub(crate) async fn start_headless_with_signals(
    signals: &mut StopSignals,
    store: SessionStore,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
    session: Option<SessionId>,
) -> anyhow::Result<crate::startup::HeadlessSession> {
    let stop = tokio::select! {
        res = crate::startup::HeadlessSession::start(
            store.clone(),
            model_override,
            yolo,
            pure,
            session,
        ) => return res,
        stop = signals.recv() => stop,
    };
    eprintln!("hya: stopping — draining store before exit");
    signals.drain_store_on_exit(&store, true).await;
    exit_stopped(stop);
}

/// Drive a full-mode one-shot `work` (`exec`, `-p`, `loop`, `workflow`) with signal draining.
pub(crate) async fn run_until_stopped<T>(
    built: &BuiltSessionEngine,
    signals: &mut StopSignals,
    cancel: &CancellationToken,
    work: impl Future<Output = T>,
) -> (T, Option<StopSignal>) {
    tokio::pin!(work);
    let stop = tokio::select! {
        output = &mut work => return (output, None),
        stop = signals.recv() => stop,
    };
    eprintln!("hya: stopping — draining in-flight turns (Ctrl-C again to exit now)");
    built.engine().begin_drain(stop.cause());
    cancel.cancel();
    let engine = built.engine();
    let store = engine.store();
    let (output, _) = signals
        .allow_immediate_interrupt(store, async {
            tokio::join!(&mut work, built.drain(stop.cause()))
        })
        .await;
    (output, Some(stop))
}

/// Flush stdout and exit with the stop signal's conventional status (`130` or `143`).
pub(crate) fn exit_stopped(stop: StopSignal) -> ! {
    let _ = std::io::stdout().flush();
    std::process::exit(stop.exit_code());
}

/// Exit with the stop signal's conventional status once teardown is done.
pub(crate) fn exit_if_stopped(stop: Option<StopSignal>) {
    if let Some(stop) = stop {
        exit_stopped(stop);
    }
}
