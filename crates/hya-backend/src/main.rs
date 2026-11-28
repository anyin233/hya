//! `hya` — the unified command-line entry point and the workspace's only
//! shipped executable (built from the `hya-backend` package). Bare `hya` on a
//! terminal starts the Bun TUI and WebUI against the database's daemon
//! (`frontend.rs`; a guidance banner without a terminal); subcommands select
//! the controlled area: headless `exec`, `-p` goal mode, HTTP/SSE `serve`,
//! `tail-session`, auth, bundles, Workflows, models, sessions, and `update`
//! for the release installer.
//!
//! Models come from Hya's provider declarations. Explicit model lists or bounded
//! startup discovery build authenticated or anonymous routes. With no resolved
//! live model rows, the offline echo provider keeps the whole stack runnable.

mod acp;
mod agent_cmd;
mod auth_cmd;
mod bridge;
mod bundle_cli;
mod bundle_cmd;
mod cli_args;
mod daemon;
mod db_lock;
mod db_writer;
mod exec_stream;
mod frontend;
mod models_cmd;
mod prompt;
mod provider_cmd;
mod proxy_cmd;
mod relay_doctor;
mod routed;
mod serve;
mod serve_relay;
mod sessions_cmd;
mod startup;
mod stop_signals;
mod update_cmd;
mod workflow_cmd;

pub use hya_app::{auth, config, permission, plugins};

use std::io::{Read as _, Write as _};

use anyhow::Context as _;
use hya_app::{prepare_driver, run_headless_lead};
use hya_engine::render_transcript;
use hya_proto::{ProjectId, SessionId};
use hya_store::SessionStore;
use tokio_util::sync::CancellationToken;

use cli_args::{Cli, Command};
use stop_signals::{StopSignals, exit_if_stopped, run_until_stopped, start_headless_with_signals};

pub use hya_app::{
    InvocationPolicy, RuntimeConfig, agent_base_with_model, agent_with_model,
    agent_with_model_pure, discover_context_files, host_info, offline_router, open_store,
    resolve_headless_agent_name, resolve_runtime, spawn_team_supervisor, today,
};

pub(crate) fn first_run_config_bootstrap(interactive: bool) -> anyhow::Result<()> {
    config::first_run_config_bootstrap(interactive)
}

/// Resolve the SQLite path for session-backed subcommands (`tail-session`, `sessions`).
///
/// Empty `--db` (CLI default) maps to `$XDG_STATE_HOME/hya/sessions.db` so
/// those subcommands see the same durable store across restarts. Explicit
/// `--db ""` is not distinguishable from the clap default here; use a real
/// path or `HYA_DB=` empty override for intentional in-memory runs.
fn resolve_interactive_db(cli_db: &str) -> String {
    if !cli_db.is_empty() {
        return cli_db.to_string();
    }
    state_dir()
        .join("sessions.db")
        .to_string_lossy()
        .into_owned()
}

/// `db` as an absolute path (relative to the working directory), so a
/// daemon, its clients, and `hya serve stop` name the same file wherever they
/// run. In-memory stores and SQLite URIs are returned unchanged.
fn absolute_db(db: String) -> String {
    if db.is_empty() || db == ":memory:" || db.starts_with("file:") || db.starts_with("sqlite:") {
        return db;
    }
    std::path::absolute(&db).map_or(db, |path| path.to_string_lossy().into_owned())
}

/// `$XDG_STATE_HOME/hya`, else `$HOME/.local/state/hya` (else
/// `./.local/state/hya`), created if missing: the default database and the
/// bare-`hya` log file live here.
fn state_dir() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| std::path::PathBuf::from(home).join(".local/state"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from(".local/state"))
        .join("hya");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExecOutput {
    Transcript,
    Json,
    TreeJson,
}

/// `hya exec` on `db`, respecting the database lock
/// ([`db_writer`]): through the server that owns a file database, else
/// in process while holding its lock (in-memory stores are not locked).
#[allow(clippy::too_many_arguments)]
async fn exec_entry(
    prompt: String,
    model_override: Option<String>,
    db: &str,
    yolo: bool,
    output: ExecOutput,
    bare: bool,
    pure: bool,
    time_budget: Option<u64>,
) -> anyhow::Result<()> {
    match db_writer::claim_or_memory(db, "exec").await? {
        db_writer::Writer::Server(server) => {
            if output == ExecOutput::TreeJson {
                db_writer::exit_unroutable(
                    "exec",
                    db,
                    &server,
                    "--include-subagents requires an in-process JSON stream",
                );
            }
            if bare {
                db_writer::exit_unroutable(
                    "exec",
                    db,
                    &server,
                    "--bare cannot apply to a running server (it loaded its own bundle profile)",
                );
            }
            if pure {
                db_writer::exit_unroutable(
                    "exec",
                    db,
                    &server,
                    "--pure cannot apply to a running server (it loaded its own context)",
                );
            }
            if time_budget.is_some() {
                db_writer::exit_unroutable(
                    "exec",
                    db,
                    &server,
                    "--time-budget cannot apply to a running server (its clock is not this run's)",
                );
            }
            routed::exec(
                &server.url,
                routed::ExecRequest {
                    prompt,
                    model: model_override,
                    yolo,
                    json: output == ExecOutput::Json,
                },
            )
            .await
        }
        db_writer::Writer::Direct(store) => {
            cmd_exec(
                prompt,
                model_override,
                store,
                yolo,
                output,
                bare,
                pure,
                time_budget,
            )
            .await
        }
    }
}

/// Ensure a Project for `workdir` (F1 design Q3: the process-start-dir tier
/// is gone; an in-process root session — `exec`/`run`/`-p` goal/`loop`/
/// `workflow` — ensures a Project for its cwd instead). Callers must do this
/// before binding runtime for that directory (agent/model resolution) and
/// before creating the session, so the directory's `.hya/bundles` project
/// bundles, project plugins, and skills apply through the Project catalog
/// scope exactly like a routed (daemon) session's server-ensured Project.
pub(crate) async fn ensure_project_for_workdir(
    store: &SessionStore,
    workdir: &std::path::Path,
) -> anyhow::Result<ProjectId> {
    let (project, _created) = store
        .ensure_project_for_path(&workdir.to_string_lossy())
        .await
        .context("ensure project for the working directory")?;
    Ok(project.id)
}

#[allow(clippy::too_many_arguments)]
async fn cmd_exec(
    prompt: String,
    model_override: Option<String>,
    store: SessionStore,
    yolo: bool,
    output: ExecOutput,
    bare: bool,
    pure: bool,
    time_budget: Option<u64>,
) -> anyhow::Result<()> {
    let mut signals = StopSignals::install()?;
    if let Some(seconds) = time_budget {
        hya_tool::deadline::install(std::time::Duration::from_secs(seconds));
    }
    let exit_store = store.clone();
    let mut headless =
        start_headless_with_signals(&mut signals, store, model_override, yolo, bare, pure, None)
            .await?;
    let body_result: anyhow::Result<(
        anyhow::Result<hya_proto::FinishReason>,
        Option<stop_signals::StopSignal>,
    )> = async {
        let engine = headless.built.engine();
        let session = headless.session;
        let agent = &headless.agent;
        let built = &mut headless.built;
        // `--json` streams durable envelopes live from the event bus (db
        // persistence is unchanged and per-event): an abnormally terminated run
        // still leaves a usable partial trajectory on stdout.
        let mut json_printer = (output != ExecOutput::Transcript).then(|| {
            let scope = if output == ExecOutput::TreeJson {
                exec_stream::StreamScope::tree(session, engine.store().clone())
            } else {
                exec_stream::StreamScope::Single(session)
            };
            let replay_scope = scope.clone();
            let final_scope = scope.clone();
            let rx = engine.bus().subscribe();
            let engine_for_stream = engine.clone();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
            let task = tokio::spawn(async move {
                exec_stream::stream_until_done(
                    rx,
                    scope,
                    done_rx,
                    |pass| replay_scope.replay(engine_for_stream.store(), pass),
                    std::io::stdout(),
                )
                .await
            });
            (task, done_tx, final_scope)
        });
        let cancel = CancellationToken::new();
        let resident_supervisor = built.resident_supervisor();
        let (turn, stop) = run_until_stopped(
            built,
            &mut signals,
            &cancel,
            run_headless_lead(
                &engine,
                &resident_supervisor,
                session,
                agent,
                cancel.clone(),
                prompt,
            ),
        )
        .await;
        hya_tool::deadline::shutdown_background_jobs().await;
        // End of the run: nothing may keep streaming after `exec` returns. Every
        // other in-flight member turn is drained before the trajectory is flushed, so stdout and the log end
        // on terminal events.
        let stop_seen = stop.is_some();
        if !stop_seen {
            signals
                .finish_with_store_report(
                    &exit_store,
                    false,
                    built.drain(if turn.is_err() {
                        hya_proto::FinishCause::LeaderFailed
                    } else {
                        hya_proto::FinishCause::Shutdown
                    }),
                )
                .await;
        }
        if let Some((task, done_tx, final_scope)) = json_printer.take() {
            let store_for_replay = engine.store().clone();
            signals
                .finish_with_store_report(&exit_store, stop_seen, async move {
                    let _ = done_tx.send(());
                    let printer = task
                        .await
                        .map_err(|e| anyhow::anyhow!("json stream task failed: {e}"))?
                        .context("stream json envelopes")?;
                    // Authoritative tail flush: anything the bus dropped or that was
                    // appended without a broadcast comes from the durable log, keeping
                    // stdout equal to `tail-session` on healthy disks and warning once
                    // on stderr if a storage fault or timeout intervenes.
                    exec_stream::finish_json_stream(
                        printer,
                        || final_scope.replay(&store_for_replay, exec_stream::ReplayPass::Lag),
                        std::io::stderr(),
                    )
                    .await
                    .context("finish json stream")
                })
                .await?;
        } else if turn.is_ok() {
            let projection = signals
                .finish_with_store_report(
                    &exit_store,
                    stop_seen,
                    engine.read_projection_shared(session),
                )
                .await
                .context("read projection")?;
            print!("{}", render_transcript(&projection));
            let _ = std::io::stdout().flush();
        }
        Ok((turn, stop))
    }
    .await;
    let already_stopped = body_result
        .as_ref()
        .ok()
        .is_some_and(|(_, stop)| stop.is_some());
    let shutdown_res = signals
        .finish_with_store_report(&exit_store, already_stopped, headless.shutdown())
        .await;
    hya_tool::deadline::shutdown_background_jobs().await;
    let (turn, stop) = body_result?;
    shutdown_res?;
    exit_if_stopped(stop);
    // Surface the turn error only after the stdout trajectory is complete.
    turn?;
    Ok(())
}

/// Run a bundle driver headlessly in an in-memory store and print its report.
async fn cmd_driver(
    driver: &str,
    args: serde_json::Value,
    model_override: Option<String>,
    yolo: bool,
    bare: bool,
    pure: bool,
) -> anyhow::Result<()> {
    let mut signals = StopSignals::install()?;
    let headless = start_headless_with_signals(
        &mut signals,
        open_store("").await?,
        model_override,
        yolo,
        bare,
        pure,
        None,
    )
    .await?;
    let (report, stop) = headless
        .run(async move |headless| {
            let cancel = CancellationToken::new();
            let request = hya_runtime_abi::DriverRequest {
                session: headless.session,
                agent: Some(headless.agent.name.clone()),
                model: Some(headless.agent.model.clone()),
                workdir: headless.agent.workdir.clone(),
                args,
            };
            let work = prepare_driver(&headless.built, driver, request, cancel.clone())?;
            let (report, stop) =
                run_until_stopped(&headless.built, &mut signals, &cancel, work).await;
            Ok((report, stop))
        })
        .await?;
    exit_if_stopped(stop);
    let report = report.with_context(|| format!("run driver {driver}"))?;
    print!("{}", report.stdout);
    eprint!("{}", report.stderr);
    let _ = std::io::stdout().flush();
    if report.exit_code != 0 {
        std::process::exit(report.exit_code);
    }
    Ok(())
}

/// Call a bundle's BundleApi endpoint `api` headlessly with `{"args": …}`
/// and print the reply's `{stdout, stderr, exit_code}`.
async fn cmd_bundle_api(
    bundle: &str,
    api: &str,
    args: serde_json::Value,
    model_override: Option<String>,
    yolo: bool,
    bare: bool,
    pure: bool,
) -> anyhow::Result<()> {
    let mut signals = StopSignals::install()?;
    let headless = start_headless_with_signals(
        &mut signals,
        open_store("").await?,
        model_override,
        yolo,
        bare,
        pure,
        None,
    )
    .await?;
    let (reply, stop) = headless
        .run(async move |headless| {
            let engine = headless.built.engine();
            let endpoint = engine
                .runtime_registry()
                .published_bundle_apis()
                .into_iter()
                .find(|published| published.bundle == bundle)
                .and_then(|published| {
                    published
                        .apis
                        .into_iter()
                        .find(|endpoint| endpoint.id == api)
                })
                .with_context(|| format!("bundle `{bundle}` serves no API endpoint `{api}`"))?;
            let call = hya_engine::BundleApiCall {
                bundle: bundle.to_string(),
                method: endpoint.method,
                session: (endpoint.scope == hya_engine::ApiScope::Session)
                    .then_some(headless.session),
                path: endpoint.path.as_str().to_string(),
                query: Default::default(),
                body: serde_json::json!({ "args": args }),
            };
            let cancel = CancellationToken::new();
            let work = async move { engine.invoke_bundle_api(call).await };
            let (reply, stop) =
                run_until_stopped(&headless.built, &mut signals, &cancel, work).await;
            Ok((reply, stop))
        })
        .await?;
    exit_if_stopped(stop);
    let reply = reply.map_err(|error| anyhow::anyhow!("{bundle} {api}: {error}"))?;
    let report: hya_runtime_abi::DriverReport = serde_json::from_value(reply.body.clone())
        .with_context(|| format!("{bundle} {api} replied without stdout/stderr/exit_code"))?;
    print!("{}", report.stdout);
    eprint!("{}", report.stderr);
    let _ = std::io::stdout().flush();
    if report.exit_code != 0 {
        std::process::exit(report.exit_code);
    }
    Ok(())
}

async fn cmd_tail_session(id: String, db: String) -> anyhow::Result<()> {
    let session: SessionId = id.parse().context("parse session id")?;
    let store = SessionStore::connect_read_only(&db)
        .await
        .with_context(|| format!("open existing database read-only at {db}"))?;
    let envelopes = store.replay(session).await.context("replay session")?;
    let mut out = std::io::stdout().lock();
    for env in envelopes {
        let line = serde_json::to_string(&env).context("serialize envelope")?;
        // A downstream `head`/`grep -q` closing the pipe is normal for a filter:
        // exit cleanly on broken pipe instead of panicking in the print machinery.
        if let Err(e) = writeln!(out, "{line}") {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(e).context("write envelope");
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build the Tokio runtime")?
        .block_on(async {
            match bundle_cli::parse(std::env::args_os().collect()).await? {
                bundle_cli::Parsed::Core(cli) => run(cli).await,
                bundle_cli::Parsed::Prompt { cli, route, args } => {
                    run_prompt(cli, route, args).await
                }
                bundle_cli::Parsed::Bundle { cli, route, args } => {
                    run_bundle_command(cli, route, args).await
                }
            }
        })
}

/// `hya -p <PROMPT>`: the prompt driver an enabled bundle declares, else one
/// headless turn whose transcript is printed.
async fn run_prompt(
    cli: Cli,
    route: Option<bundle_cli::Route>,
    args: serde_json::Value,
) -> anyhow::Result<()> {
    let model = cli_args::merge_model_effort(cli.model.clone(), cli.effort.clone())?;
    match route {
        Some(route) => {
            let label = route.label().to_string();
            run_route(route, args, model, cli.yolo, cli.bare, cli.pure)
                .await
                .inspect_err(|error| eprintln!("{label} error: {error:#}"))
        }
        None => {
            exec_entry(
                cli.prompt.clone().unwrap_or_default(),
                model,
                &cli.db,
                cli.yolo,
                ExecOutput::Transcript,
                cli.bare,
                cli.pure,
                None,
            )
            .await
        }
    }
}

/// A bundle-contributed `hya <command>`.
async fn run_bundle_command(
    cli: Cli,
    route: bundle_cli::Route,
    args: serde_json::Value,
) -> anyhow::Result<()> {
    let model = cli_args::merge_model_effort(cli.model.clone(), cli.effort.clone())?;
    run_route(route, args, model, cli.yolo, cli.bare, cli.pure).await
}

async fn run_route(
    route: bundle_cli::Route,
    args: serde_json::Value,
    model: Option<String>,
    yolo: bool,
    bare: bool,
    pure: bool,
) -> anyhow::Result<()> {
    match route {
        bundle_cli::Route::Driver(driver) => {
            cmd_driver(&driver, args, model, yolo, bare, pure).await
        }
        bundle_cli::Route::Api { bundle, api } => {
            cmd_bundle_api(&bundle, &api, args, model, yolo, bare, pure).await
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    // Validate before path absolutization or best-effort daemon discovery can
    // obscure an invalid database argument, including command-local overrides.
    let database = match &cli.command {
        Some(
            Command::Serve { db: Some(db), .. }
            | Command::TailSession { db: Some(db), .. }
            | Command::Sessions { db: Some(db), .. },
        ) => db,
        _ => &cli.db,
    };
    // Validate only the lexical database argument here (no filesystem syscalls on main thread).
    if let Err(error) = hya_store::validate_database_arg(database) {
        return Err(error.into());
    }
    let web_port = cli_args::bare_web_port(&cli)?;
    let backend = cli_args::bare_backend(&cli)?;
    let resume = cli_args::bare_resume(&cli)?;
    let connect = cli_args::bare_connect(&cli)?;
    let allow_hosts = cli_args::bare_allow_hosts(&cli)?;
    let model = cli_args::merge_model_effort(cli.model.clone(), cli.effort.clone())?;
    let yolo = cli.yolo;
    let bare = cli.bare;
    let pure = cli.pure;
    let db = cli.db.clone();
    match cli.command {
        // Bare `hya` on a terminal starts the TUI and the WebUI against the
        // database's backend daemon (frontend.rs, daemon.rs); without a
        // terminal it only points at the other surfaces.
        None => {
            use std::io::IsTerminal as _;
            if frontend::should_launch(
                std::io::stdin().is_terminal(),
                std::io::stdout().is_terminal(),
            ) {
                // The link (read before the TUI owns the terminal) is the
                // credential: only its redacted form is ever printed.
                let (connect, connect_relay_ca) = match connect {
                    Some(bare_conn) => {
                        if bridge::exposed_in_argv(&bare_conn.source) {
                            eprintln!("hya: {}", bridge::ARGV_WARNING);
                        }
                        let link = bridge::read_link(&bare_conn.source, "--connect")?;
                        (
                            Some(bridge::with_transport(link, bare_conn.transport)),
                            bare_conn.relay_ca,
                        )
                    }
                    None => (None, None),
                };
                return frontend::run(frontend::LaunchRequest {
                    port: web_port,
                    db: absolute_db(resolve_interactive_db(&db)),
                    backend,
                    resume,
                    model,
                    yolo,
                    bare,
                    pure,
                    state_dir: state_dir(),
                    connect,
                    connect_relay_ca,
                    allow_hosts,
                })
                .await;
            }
            println!(
                "hya {} — a multi-agent coding agent",
                env!("CARGO_PKG_VERSION")
            );
            println!(
                "Run `hya` in a terminal to start the TUI and the WebUI \
                 (http://127.0.0.1:{web_port}; needs Bun). Without a terminal, try \
                 `hya serve`, `hya exec \"<prompt>\"`, `hya -p \"<goal>\"`, or `hya --help`."
            );
            Ok(())
        }
        Some(Command::Acp) => {
            anyhow::ensure!(!yolo, "hya acp uses the daemon's permission policy");
            acp::run(absolute_db(resolve_interactive_db(&db)), model, bare, pure).await
        }
        Some(Command::Exec {
            mut prompt,
            json,
            include_subagents,
            time_budget,
        }) => {
            if prompt == "-" {
                prompt.clear();
                std::io::stdin()
                    .read_to_string(&mut prompt)
                    .context("read the exec prompt from stdin as UTF-8")?;
                if prompt.ends_with('\n') {
                    prompt.pop();
                }
                anyhow::ensure!(!prompt.is_empty(), "empty exec prompt on stdin");
            }
            let output = if include_subagents {
                ExecOutput::TreeJson
            } else if json {
                ExecOutput::Json
            } else {
                ExecOutput::Transcript
            };
            exec_entry(prompt, model, &db, yolo, output, bare, pure, time_budget).await
        }
        Some(Command::Serve {
            action: Some(action),
            db: command_db,
            relay,
            allow_host,
            ..
        }) => {
            let path = command_db.unwrap_or_else(|| db.clone());
            serve::cmd_serve_action(
                action,
                absolute_db(resolve_interactive_db(&path)),
                model,
                yolo,
                bare,
                pure,
                relay,
                allow_host,
            )
            .await
        }
        Some(Command::Serve {
            bind,
            db: command_db,
            action: None,
            relay,
            allow_host,
            ..
        }) => {
            serve::cmd_serve(
                bind,
                command_db.unwrap_or_else(|| db.clone()),
                model,
                yolo,
                bare,
                pure,
                relay,
                allow_host,
            )
            .await
        }
        Some(Command::TailSession { id, db: command_db }) => {
            let path = command_db.unwrap_or_else(|| db.clone());
            cmd_tail_session(id, resolve_interactive_db(&path)).await
        }
        Some(Command::Login { provider, token }) => auth_cmd::login(provider, token).await,
        Some(Command::Oauth { command }) => auth_cmd::run_oauth(command).await,
        Some(Command::Provider { command }) => {
            provider_cmd::run(command, absolute_db(resolve_interactive_db(&db))).await
        }
        Some(Command::Agent { command }) => agent_cmd::run(command, bare),
        Some(Command::Bundle { command }) => {
            bundle_cmd::run(command, absolute_db(resolve_interactive_db(&db))).await
        }
        Some(Command::Workflow { command }) => {
            workflow_cmd::run(command, model, &db, yolo, bare, pure).await
        }
        Some(Command::Models {
            provider,
            verbose,
            refresh,
        }) => {
            first_run_config_bootstrap(false)?;
            // With a running backend the refresh goes through it (one fetch,
            // and its TUI/WebUI see the new models); `resolve_runtime` then
            // reads the model cache it wrote, so those providers are not
            // fetched again as pending.
            let refreshed = if refresh {
                provider_cmd::refresh_through_backend(
                    &absolute_db(resolve_interactive_db(&db)),
                    provider.as_deref(),
                )
                .await?
            } else {
                None
            };
            let mut runtime = resolve_runtime(model).await;
            if let Some(refreshed) = &refreshed {
                runtime
                    .pending_discovery
                    .retain(|entry| !refreshed.contains(&entry.provider_id));
            }
            if refresh && refreshed.is_none() {
                let ids = provider
                    .clone()
                    .map(|id| std::collections::BTreeSet::from([id]));
                let rebuilt = config::rebuild_providers(
                    ids.as_ref(),
                    config::DiscoverMode::Always,
                    runtime.catalog.as_ref(),
                    &runtime.router,
                )
                .await
                .context("refresh provider catalog")?;
                for (provider_id, discovery) in &rebuilt.discovery {
                    if let Some(error) = discovery.error_message() {
                        eprintln!(
                            "hya: {provider_id}: model list {}: {error}",
                            discovery.label()
                        );
                    }
                }
                runtime.router = rebuilt.router;
                runtime.catalog = rebuilt.catalog;
            } else if !runtime.pending_discovery.is_empty() {
                let pending = std::mem::take(&mut runtime.pending_discovery);
                let (router, catalog) = config::refresh_pending_catalogs(
                    pending,
                    runtime.catalog.as_ref(),
                    &runtime.router,
                )
                .await
                .context("refresh provider catalog")?;
                runtime.router = router;
                runtime.catalog = catalog;
            }
            models_cmd::cmd_models(&runtime.catalog, provider, verbose)
        }
        Some(Command::Sessions {
            db: command_db,
            all,
            archived,
            action,
        }) => {
            let path = command_db.unwrap_or_else(|| db.clone());
            let listing = if archived {
                sessions_cmd::Listing::Archived
            } else if all {
                sessions_cmd::Listing::All
            } else {
                sessions_cmd::Listing::Active
            };
            sessions_cmd::run(resolve_interactive_db(&path), action, listing).await
        }
        // Install without composing a runtime, then notify the database daemon.
        Some(Command::Update { command, install }) => match command {
            Some(command) => {
                let mut stdout = std::io::stdout().lock();
                hya_updater::cli::run(command, &mut stdout)
                    .map_err(|error| anyhow::anyhow!("hya update: {error}"))
            }
            None => update_cmd::run(&install, &resolve_interactive_db(&db)).await,
        },
        // The proxy composes no runtime either: no config, providers, MCP,
        // or session store — it only opens network connections.
        Some(Command::Proxy { args }) => proxy_cmd::cmd_proxy(args).await,
        // The bridge composes no runtime either: it only relays connections.
        Some(Command::Bridge { args }) => bridge::cmd_bridge(args).await,
        // The doctor also composes no runtime: it only probes the relay.
        Some(Command::Relay { command }) => {
            let code = relay_doctor::run(command).await?;
            std::process::exit(code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stop_signals::StopSignal;
    use hya_proto::{FinishCause, ModelRef};
    use std::sync::Arc;

    struct InterruptProvider(tokio::sync::mpsc::UnboundedSender<SessionId>);

    #[async_trait::async_trait]
    impl hya_provider::Provider for InterruptProvider {
        fn id(&self) -> &str {
            "hya"
        }
        fn capabilities(&self, model: &ModelRef) -> Option<hya_provider::Capabilities> {
            hya_provider::Provider::capabilities(&hya_provider::DevProvider::new(), model)
        }
        async fn stream(
            &self,
            _request: hya_provider::CompletionRequest,
            session: SessionId,
            _message: hya_proto::MessageId,
        ) -> Result<hya_provider::EventStream, hya_provider::ProviderError> {
            let _ = self.0.send(session);
            std::future::pending().await
        }
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn headless_interrupt_drain_preserves_user_cancel_without_restore() {
        let (entered, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let router = hya_provider::ProviderRouter::new().with(Arc::new(InterruptProvider(entered)));
        let agent = agent_with_model_pure("hya/offline", None);
        let mut built = hya_app::build_session_engine_pure(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            Default::default(),
            Vec::new(),
            (
                InvocationPolicy::default(),
                config::EngineSettings::default(),
            ),
        )
        .await
        .unwrap();
        let engine = built.engine();
        let root = engine
            .create(hya_engine::CreateSession {
                parent: None,
                agent: agent.name.clone(),
                model: agent.model.clone(),
                workdir: agent.workdir.to_string_lossy().into_owned(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            })
            .await
            .unwrap();
        let _drivers = built
            .resident_supervisor()
            .start_recovered_headless(root)
            .await
            .unwrap();
        let binding = engine.bind_runtime(&agent.workdir).unwrap();
        let worker = engine
            .agent_spec_for_binding(&binding, &agent, "hya-scout")
            .unwrap();
        let resources = binding.agent_resource_policy("hya-scout").unwrap();
        let (child, _) = built
            .resident_supervisor()
            .spawn_resident(
                root,
                worker,
                (binding, Arc::from([]), resources, None),
                "member work".into(),
                None,
                None,
            )
            .await
            .unwrap();
        let prompt = engine
            .admit_user_prompt(root, "root work".into())
            .await
            .unwrap();
        let turn = tokio::spawn({
            let engine = engine.clone();
            async move {
                engine
                    .run_turn_with_external_dirs_and_guidance(
                        root,
                        &agent,
                        CancellationToken::new(),
                        &[],
                        None,
                        None,
                        Some(prompt),
                    )
                    .await
            }
        });
        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_secs(5), requests.recv())
                .await
                .unwrap()
                .unwrap();
        }
        built.drain(StopSignal::Interrupt.cause()).await;
        turn.await.unwrap().unwrap();
        assert!(
            engine
                .store()
                .list_pending_resumes()
                .await
                .unwrap()
                .is_empty()
        );
        for session in [root, child] {
            let projection = engine.read_projection(session).await.unwrap();
            let answer = projection
                .session
                .messages
                .iter()
                .rev()
                .find(|message| message.role == hya_proto::Role::Assistant)
                .unwrap();
            assert_eq!(answer.cause, Some(FinishCause::UserCancel));
        }
        let member =
            hya_tool::extension::members_projection(&engine.read_projection(root).await.unwrap())
                .pop()
                .unwrap();
        assert_eq!(member.summary, "stopped: cancelled by the user");
        built.shutdown().await.unwrap();
    }
}
