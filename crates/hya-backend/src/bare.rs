//! Process and output handling for `hya exec --bare`.

use std::io::{self, Write};
use std::time::Duration;

use hya_app::{BareEventSink, run_bare};
use hya_proto::Envelope;
use hya_store::SessionStore;
use hya_tool::deadline;
use tokio_util::sync::CancellationToken;

use crate::exec_stream::write_envelope;

const BARE_CANCEL_DRAIN: Duration = Duration::from_secs(5);

/// `hya exec --bare` on `db`: always in process; a server holding the
/// database is an error (exit 75), never a route.
pub(crate) async fn exec_entry(
    prompt: String,
    model_override: Option<String>,
    db: &str,
    time_budget: Option<u64>,
    json: bool,
) -> anyhow::Result<()> {
    match crate::db_writer::claim_or_memory(db, "exec").await? {
        crate::db_writer::Writer::Direct(store) => {
            let mut signals = crate::stop_signals::StopSignals::install()?;
            if let Some(seconds) = time_budget {
                deadline::install(Duration::from_secs(seconds));
            }
            let cancel = CancellationToken::new();
            let (result, stop) = signals
                .run_cancellable(
                    &store,
                    &cancel,
                    Some(BARE_CANCEL_DRAIN),
                    exec(prompt, model_override, &store, json, &cancel),
                )
                .await;
            deadline::shutdown_background_jobs().await;
            signals.drain_store_on_exit(&store, stop.is_some()).await;
            crate::stop_signals::exit_if_stopped(stop);
            result.unwrap_or(Ok(()))
        }
        crate::db_writer::Writer::Server(server) => crate::db_writer::exit_unroutable(
            "exec",
            db,
            &server,
            "--bare runs the kernel in process and cannot use a running server",
        ),
    }
}

async fn exec(
    prompt: String,
    model_override: Option<String>,
    store: &SessionStore,
    json: bool,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    crate::first_run_config_bootstrap(false)?;
    let mut out = io::stdout();
    let mut write = |envelope: &Envelope| {
        write_envelope(&mut out, envelope)
            .and_then(|()| out.flush())
            .map_err(|error| format!("write session event to stdout: {error}"))
    };
    let sink = json.then_some(&mut write as &mut BareEventSink<'_>);
    if let Some(text) = run_bare(prompt, model_override, store, sink, cancel).await? {
        println!("{text}");
        let _ = io::stdout().flush();
    }
    Ok(())
}
