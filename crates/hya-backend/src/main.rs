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

mod agent_cmd;
mod auth_cmd;
mod bridge;
mod bundle_cmd;
mod cli_args;
mod daemon;
mod db_lock;
mod db_writer;
mod exec_stream;
mod frontend;
mod lead_wrapup;
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
mod update_cmd;
mod workflow_cmd;

pub use hya_app::{auth, config, formatter_config, permission, plugins};

use std::io::Write as _;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use hya_core::completion::{PluginGoalEvaluator, render_transcript};
use hya_core::hooks::HookDispatcher;
use hya_core::loop_mode::{LoopConfig, ModelLoopPlanner, ModelLoopVerifier, run_loop};
use hya_core::{
    GoalEvaluator, HookChain, HookLoopPlanner, HookLoopVerifier, ModelGoalEvaluator, RunOutcome,
    SafetyCaps, TurnBinding, run_goal,
};
use hya_proto::{FinishCause, ModelRef, ProjectId, SessionId};
use hya_store::SessionStore;
use tokio_util::sync::CancellationToken;

use cli_args::{Cli, Command};

pub use hya_app::{
    InvocationPolicy, RuntimeConfig, WebSearchConfig, agent_base_with_model, agent_with_model,
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

/// `hya exec` on `db`, respecting the database lock
/// ([`db_writer`]): through the server that owns a file database, else
/// in process while holding its lock (in-memory stores are not locked).
async fn exec_entry(
    prompt: String,
    model_override: Option<String>,
    db: &str,
    yolo: bool,
    json: bool,
    pure: bool,
    time_budget: Option<u64>,
) -> anyhow::Result<()> {
    match db_writer::claim(db, "exec").await? {
        db_writer::Writer::Server(server) => {
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
                    json,
                },
            )
            .await
        }
        db_writer::Writer::Direct(store) => {
            cmd_exec(prompt, model_override, store, yolo, json, pure, time_budget).await
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

async fn cmd_exec(
    prompt: String,
    model_override: Option<String>,
    store: SessionStore,
    yolo: bool,
    json: bool,
    pure: bool,
    time_budget: Option<u64>,
) -> anyhow::Result<()> {
    if let Some(seconds) = time_budget {
        hya_tool::deadline::install(std::time::Duration::from_secs(seconds));
    }
    let headless = startup::HeadlessSession::start(store, model_override, yolo, pure, None).await?;
    let (turn, stop) = headless
        .run(async move |headless| {
            let engine = headless.built.engine();
            let session = headless.session;
            let agent = &headless.agent;
            let built = &mut headless.built;
            // `--json` streams durable envelopes live from the event bus (db
            // persistence is unchanged and per-event): an abnormally terminated run
            // still leaves a usable partial trajectory on stdout.
            let mut json_printer = json.then(|| {
                let rx = engine.bus().subscribe();
                let engine_for_stream = engine.clone();
                let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
                let task = tokio::spawn(exec_stream::stream_until_done(
                    rx,
                    session,
                    done_rx,
                    move || {
                        let engine = engine_for_stream.clone();
                        async move { engine.replay(session).await.map_err(anyhow::Error::from) }
                    },
                    std::io::stdout(),
                ));
                (task, done_tx)
            });
            let prompt = engine
                .admit_user_prompt(session, prompt)
                .await
                .context("admit prompt")?;
            if let Some(deadline) = hya_tool::deadline::current() {
                engine
                    .inject_system_message(session, deadline.opening())
                    .await
                    .context("announce the time budget")?;
            }
            let cancel = CancellationToken::new();
            let (turn, stop) = run_until_stopped(
                built,
                &cancel,
                lead_wrapup::run_lead(
                    &engine,
                    &built.resident_supervisor(),
                    session,
                    agent,
                    cancel.clone(),
                    prompt,
                ),
            )
            .await?;
            // End of the run: nothing may keep streaming after `exec` returns. Every
            // other in-flight member turn is drained before the trajectory is flushed, so stdout and the log end
            // on terminal events.
            if stop.is_none() {
                built
                    .drain(if turn.is_err() {
                        FinishCause::LeaderFailed
                    } else {
                        FinishCause::Shutdown
                    })
                    .await;
            }
            if let Some((task, done_tx)) = json_printer.take() {
                let _ = done_tx.send(());
                let printer = task
                    .await
                    .map_err(|e| anyhow::anyhow!("json stream task failed: {e}"))?
                    .context("stream json envelopes")?;
                // Authoritative tail flush: anything the bus dropped or that was
                // appended between the last broadcast and turn completion comes from
                // the durable log, keeping stdout exactly equal to `tail-session`.
                let envelopes = engine.replay(session).await.context("replay session")?;
                let mut printer = printer;
                for env in &envelopes {
                    printer.print(env, session).context("write json envelope")?;
                }
                printer.flush().context("flush json stream")?;
            } else if turn.is_ok() {
                let projection = engine
                    .read_projection(session)
                    .await
                    .context("read projection")?;
                print!("{}", render_transcript(&projection));
            }
            Ok((turn, stop))
        })
        .await?;
    exit_if_stopped(stop);
    // Surface the turn error only after the stdout trajectory is complete.
    turn.context("run turn")?;
    Ok(())
}

/// A stop signal that ended a one-shot run early.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopSignal {
    /// SIGINT (Ctrl-C): the user stopped the run.
    Interrupt,
    /// SIGTERM: the process is being stopped.
    Terminate,
}

impl StopSignal {
    fn cause(self) -> FinishCause {
        match self {
            Self::Interrupt => FinishCause::UserCancel,
            Self::Terminate => FinishCause::Shutdown,
        }
    }

    /// Conventional `128 + signal` exit status.
    fn exit_code(self) -> i32 {
        match self {
            Self::Interrupt => 130,
            Self::Terminate => 143,
        }
    }
}

/// Drive a one-shot run's `work` (`exec`/`run`, `-p` goal, `loop`).
///
/// SIGINT/SIGTERM starts a graceful drain instead of killing the process:
/// every in-flight turn in every session is cancelled with cause
/// `user_cancel` (SIGINT) or `shutdown` (SIGTERM) and closes its messages,
/// members go terminal, and `work` — still polled — observes the cancel and
/// returns. The drain is bounded by [`hya_core::DRAIN_DEADLINE`]; a second
/// SIGINT during the drain exits immediately (crash recovery closes what is
/// left on the next start).
async fn run_until_stopped<T>(
    built: &hya_app::BuiltSessionEngine,
    cancel: &CancellationToken,
    work: impl std::future::Future<Output = T>,
) -> anyhow::Result<(T, Option<StopSignal>)> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    let mut terminate = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    tokio::pin!(work);
    let stop = tokio::select! {
        output = &mut work => return Ok((output, None)),
        _ = interrupt.recv() => StopSignal::Interrupt,
        _ = terminate.recv() => StopSignal::Terminate,
    };
    eprintln!("hya: stopping — draining in-flight turns (Ctrl-C again to exit now)");
    // Record the cause on every in-flight turn first, then stop the driver.
    built.engine().begin_drain(stop.cause());
    cancel.cancel();
    let drained = async { tokio::join!(&mut work, built.drain(stop.cause())) };
    tokio::select! {
        (output, _) = drained => Ok((output, Some(stop))),
        _ = interrupt.recv() => {
            eprintln!("hya: interrupted again; exiting without finishing the drain");
            std::process::exit(StopSignal::Interrupt.exit_code());
        }
    }
}

/// Exit with the stop signal's conventional status once teardown is done.
fn exit_if_stopped(stop: Option<StopSignal>) {
    if let Some(stop) = stop {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        std::process::exit(stop.exit_code());
    }
}

async fn cmd_goal(
    goal: String,
    max_iterations: u32,
    evaluator_model_flag: Option<String>,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
) -> anyhow::Result<()> {
    let headless =
        startup::HeadlessSession::start(open_store("").await?, model_override, yolo, pure, None)
            .await?;
    let (outcome, stop) = headless
        .run(async move |headless| {
            let engine = headless.built.engine();
            let session = headless.session;
            let agent = headless.agent.clone();
            let evaluator_router = headless.evaluator_router.clone();
            let evaluator_model = config::resolve_evaluator_model(
                evaluator_model_flag.as_deref(),
                None,
                &headless.evaluator_model,
            )
            .to_string();
            let binding = engine
                .bind_root_runtime(&agent.workdir)
                .await
                .context("bind goal evaluator resources")?;
            let built = &mut headless.built;
            // Selection point (design §4.5): a registered `goal.evaluate` hook
            // provider outranks the built-in evaluator. The probe is the trait
            // capability probe (default false), so any dispatcher without the hook —
            // or a selection that cannot confirm registration — fails open to the D7
            // model evaluator instead of erroring startup.
            let fallback: Arc<dyn GoalEvaluator> = Arc::new(
                ModelGoalEvaluator::new(
                    Arc::new(evaluator_router),
                    ModelRef::new(&evaluator_model),
                )
                .with_system_prompt(
                    binding
                        .bundle_skill_content("hya/goal-loop", "evaluator-prompt")
                        .map(skill_prompt_body)
                        .unwrap_or("You are an independent goal verifier. No tools."),
                ),
            );
            let dispatcher = goal_loop_hook_chain(&binding, agent.name.as_str());
            let evaluator: Arc<dyn GoalEvaluator> = if dispatcher.has_goal_evaluate() {
                Arc::new(PluginGoalEvaluator::new(dispatcher).with_fallback(Arc::clone(&fallback)))
            } else {
                fallback
            };
            let caps = SafetyCaps {
                max_iterations,
                ..SafetyCaps::default()
            };
            let cancel = CancellationToken::new();
            let (outcome, stop) = run_until_stopped(
                built,
                &cancel,
                run_goal(
                    engine.clone(),
                    session,
                    agent,
                    goal,
                    evaluator,
                    caps,
                    cancel.clone(),
                ),
            )
            .await?;
            Ok((outcome, stop))
        })
        .await?;
    exit_if_stopped(stop);
    let outcome = outcome.context("run goal")?;
    println!("goal outcome: {outcome:?}");
    Ok(())
}

/// Loop-mode CLI entry (dev_plan 6.11): drive the lead session with an
/// independent model verifier/planner until the deterministic condition, the
/// `loop.should_stop` hook, or the verifier stops the run. In-memory store,
/// mirroring `cmd_goal`'s setup.
#[allow(clippy::too_many_arguments)]
async fn cmd_loop(
    target: String,
    budget: Option<u32>,
    max_iterations: Option<u32>,
    while_command: Option<String>,
    until_command: Option<String>,
    evaluator_model_flag: Option<String>,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
) -> anyhow::Result<()> {
    let headless =
        startup::HeadlessSession::start(open_store("").await?, model_override, yolo, pure, None)
            .await?;
    let (outcome, stop) = headless
        .run(async move |headless| {
            let engine = headless.built.engine();
            let session = headless.session;
            let agent = headless.agent.clone();
            let evaluator_router = headless.evaluator_router.clone();
            let evaluator_model = config::resolve_evaluator_model(
                evaluator_model_flag.as_deref(),
                None,
                &headless.evaluator_model,
            )
            .to_string();
            let binding = engine
                .bind_root_runtime(&agent.workdir)
                .await
                .context("bind loop evaluator resources")?;
            let built = &mut headless.built;
            let predicate = cli_args::build_loop_predicate(while_command, until_command, &agent.workdir)
                .map_err(anyhow::Error::msg)
                .context("resolve loop condition")?;
            let loop_config = LoopConfig {
                budget: cli_args::loop_budget(budget, max_iterations),
                predicate,
                ..LoopConfig::default()
            };
            // The engine-supplied `loop.should_stop` consult: registered plugin
            // providers may force a legitimate stop; without registrations the
            // dispatcher answers `None` and the gate falls through untouched.
            let evaluator_dispatcher = goal_loop_hook_chain(&binding, agent.name.as_str());
            let verifier_fallback: Arc<dyn hya_core::LoopVerifier> = Arc::new(
                ModelLoopVerifier::new(Arc::new(evaluator_router.clone()), ModelRef::new(&evaluator_model))
                    .with_system_prompt(binding.bundle_skill_content("hya/goal-loop", "loop-verifier-prompt").map(skill_prompt_body).unwrap_or("You are an independent loop verifier. You have no stake in the work and no tools.")),
            );
            let planner_fallback: Arc<dyn hya_core::LoopPlanner> = Arc::new(
                ModelLoopPlanner::new(Arc::new(evaluator_router), ModelRef::new(&evaluator_model))
                    .with_system_prompt(
                        binding
                            .bundle_skill_content("hya/goal-loop", "loop-planner-prompt")
                            .map(skill_prompt_body)
                            .unwrap_or("You are an independent loop planner. No tools."),
                    ),
            );
            let should_stop = Arc::clone(&evaluator_dispatcher);
            let cancel = CancellationToken::new();
            let (outcome, stop) = run_until_stopped(
                built,
                &cancel,
                run_loop(
                    engine.clone(),
                    session,
                    agent,
                    target,
                    Arc::new(HookLoopVerifier::new(
                        Arc::clone(&evaluator_dispatcher),
                        verifier_fallback,
                    )),
                    Arc::new(HookLoopPlanner::new(evaluator_dispatcher, planner_fallback)),
                    loop_config,
                    cancel.clone(),
                    Some(should_stop),
                ),
            )
            .await?;
            Ok((outcome, stop))
        })
        .await?;
    exit_if_stopped(stop);
    let outcome = outcome.context("run loop")?;
    println!("loop outcome: {outcome:?}");
    if let RunOutcome::InvalidCondition { reason, .. } = &outcome {
        eprintln!(
            "warning: the loop condition itself is broken, so this is NOT a success: {reason}"
        );
        anyhow::bail!("loop ended on a broken condition; not a success");
    }
    Ok(())
}

fn skill_prompt_body(content: &str) -> &str {
    content
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n").map(|(_, body)| body.trim()))
        .unwrap_or(content)
}

fn goal_loop_hook_chain(binding: &TurnBinding, stable_agent_id: &str) -> Arc<dyn HookDispatcher> {
    let active_is_goal_loop = binding
        .resolve_agent(stable_agent_id)
        .is_some_and(|agent| agent.origin.bundle_id() == Some("hya/goal-loop"));
    // The goal-loop owner is appended unfiltered below; retain global/plugin hooks.
    let selected = binding.bundle_hooks_for_agent(if active_is_goal_loop {
        hya_core::MAIN_AGENT_ID
    } else {
        stable_agent_id
    });
    let agentless = binding
        .bundle_catalog()
        .bundles()
        .iter()
        .filter(|bundle| {
            (bundle.agents().is_empty() && bundle.workflow().is_none())
                && bundle.identity().id != "hya/goal-loop"
        })
        .filter_map(|bundle| binding.bundle_hooks(&bundle.identity().id))
        .collect();
    Arc::new(HookChain::new(ordered_evaluator_hooks(
        binding.global_plugin_hooks(),
        agentless,
        selected,
        binding.bundle_hooks("hya/goal-loop"),
    )))
}

fn ordered_evaluator_hooks(
    global: Vec<Arc<dyn HookDispatcher>>,
    agentless: Vec<Arc<dyn HookDispatcher>>,
    selected: Vec<Arc<dyn HookDispatcher>>,
    intelligent: Option<Arc<dyn HookDispatcher>>,
) -> Vec<Arc<dyn HookDispatcher>> {
    let mut hooks = Vec::new();
    for hook in global
        .into_iter()
        .chain(agentless)
        .chain(selected)
        .chain(intelligent)
    {
        if !hooks.iter().any(|existing| Arc::ptr_eq(existing, &hook)) {
            hooks.push(hook);
        }
    }
    hooks
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
    let cli = Cli::parse();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build the Tokio runtime")?
        .block_on(run(cli))
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let web_port = cli_args::bare_web_port(&cli)?;
    let backend = cli_args::bare_backend(&cli)?;
    let resume = cli_args::bare_resume(&cli)?;
    let connect = cli_args::bare_connect(&cli)?;
    let allow_hosts = cli_args::bare_allow_hosts(&cli)?;
    let model = cli_args::merge_model_effort(cli.model.clone(), cli.effort.clone())?;
    let yolo = cli.yolo;
    let pure = cli.pure;
    let db = cli.db.clone();
    if let Some(goal) = cli.prompt {
        return cmd_goal(
            goal,
            cli.max_iterations,
            cli.evaluator_model.clone(),
            model,
            yolo,
            pure,
        )
        .await
        .inspect_err(|error| eprintln!("goal error: {error:#}"));
    }
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
                    Some(bare) => {
                        if bridge::exposed_in_argv(&bare.source) {
                            eprintln!("hya: {}", bridge::ARGV_WARNING);
                        }
                        let link = bridge::read_link(&bare.source, "--connect")?;
                        (
                            Some(bridge::with_transport(link, bare.transport)),
                            bare.relay_ca,
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
        Some(Command::Exec {
            prompt,
            json,
            time_budget,
        }) => exec_entry(prompt, model, &db, yolo, json, pure, time_budget).await,
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
        Some(Command::Agent { command }) => agent_cmd::run(command),
        Some(Command::Bundle { command }) => {
            bundle_cmd::run(command, absolute_db(resolve_interactive_db(&db))).await
        }
        Some(Command::Workflow { command }) => {
            workflow_cmd::run(command, model, &db, yolo, pure).await
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
        Some(Command::Loop {
            target,
            budget,
            max_iterations,
            while_command,
            until_command,
            evaluator_model,
        }) => {
            cmd_loop(
                target,
                budget,
                max_iterations,
                while_command,
                until_command,
                evaluator_model,
                model,
                yolo,
                pure,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hya_core::hooks::NoopHookHost;

    #[test]
    fn evaluator_hook_order_is_global_agentless_selected_intelligent_and_deduplicated() {
        let global: Arc<dyn HookDispatcher> = Arc::new(NoopHookHost);
        let agentless: Arc<dyn HookDispatcher> = Arc::new(NoopHookHost);
        let selected: Arc<dyn HookDispatcher> = Arc::new(NoopHookHost);
        let intelligent: Arc<dyn HookDispatcher> = Arc::new(NoopHookHost);
        let ordered = ordered_evaluator_hooks(
            vec![Arc::clone(&global)],
            vec![Arc::clone(&agentless)],
            vec![Arc::clone(&selected), Arc::clone(&intelligent)],
            Some(Arc::clone(&intelligent)),
        );
        assert_eq!(ordered.len(), 4);
        assert!(Arc::ptr_eq(&ordered[0], &global));
        assert!(Arc::ptr_eq(&ordered[1], &agentless));
        assert!(Arc::ptr_eq(&ordered[2], &selected));
        assert!(Arc::ptr_eq(&ordered[3], &intelligent));
    }

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
                WebSearchConfig::default(),
                InvocationPolicy::default(),
                config::EngineSettings::default(),
            ),
        )
        .await
        .unwrap();
        let engine = built.engine();
        let root = engine
            .create(hya_core::CreateSession {
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
        let member = engine
            .read_projection(root)
            .await
            .unwrap()
            .session
            .members
            .pop()
            .unwrap();
        assert_eq!(member.summary, "stopped: cancelled by the user");
        built.shutdown().await.unwrap();
    }
}
