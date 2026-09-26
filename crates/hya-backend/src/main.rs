//! `hya` — the unified command-line entry point and the workspace's only
//! shipped executable (built from the `hya-backend` package). Bare `hya` on a
//! terminal starts the Bun TUI and WebUI against an in-process server
//! (`frontend.rs`; a guidance banner without a terminal); subcommands select
//! the controlled area: headless `exec`, `-p` goal mode, HTTP/SSE `serve`,
//! `tail-session`, auth, bundles, Workflows, models, sessions, and `update`
//! for the self-update TCB.
//!
//! Models come from Hya's provider declarations. Explicit model lists or bounded
//! startup discovery build authenticated or anonymous routes. With no resolved
//! live model rows, the offline echo provider keeps the whole stack runnable.

// allow: SIZE_OK — Phase 1 only extracts bootstrap glue; CLI command bodies stay here unchanged.

mod agent_cmd;
mod auth_cmd;
mod bundle_cmd;
mod cli_args;
mod db_lock;
mod exec_stream;
mod frontend;
mod models_cmd;
mod proxy_cmd;
mod rpc;
mod serve;
mod sessions_cmd;
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
    CreateSession, GoalEvaluator, HookChain, HookLoopPlanner, HookLoopVerifier, ModelGoalEvaluator,
    RunOutcome, SafetyCaps, TurnBinding, run_goal,
};
use hya_proto::{FinishCause, ModelRef, SessionId};
use hya_store::SessionStore;
use tokio_util::sync::CancellationToken;

use crate::permission::spawn_reject_responder;
use cli_args::{Cli, Command};

pub use hya_app::{
    InvocationPolicy, RuntimeConfig, WebSearchConfig, agent_base_with_model, agent_with_model,
    agent_with_model_pure, build_session_engine, build_session_engine_pure, compaction_config,
    discover_context_files, host_info, offline_router, open_store, resolve_headless_agent_name,
    resolve_runtime, spawn_team_supervisor, today,
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

async fn cmd_exec(
    prompt: String,
    model_override: Option<String>,
    db: &str,
    yolo: bool,
    json: bool,
    pure: bool,
) -> anyhow::Result<()> {
    first_run_config_bootstrap(false)?;
    let has_explicit_model = model_override.is_some();
    let store = open_store(db).await?;
    let runtime = resolve_runtime(model_override)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
    let mut agent = if pure {
        agent_with_model_pure(&runtime.model, runtime.reasoning)
    } else {
        agent_with_model(&runtime.model, runtime.reasoning)
    };
    let mut built = if pure {
        build_session_engine_pure(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    } else {
        build_session_engine(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    };
    let engine = built.engine();
    // Honor the configured `default_agent` for this headless root session,
    // same precedence and failure mode as `serve`'s root-session resolution:
    // config `default_agent`, then the built-in default; an unselectable id
    // fails clearly instead of silently falling back to the built-in agent.
    agent.name = resolve_headless_agent_name(
        &engine,
        &agent.workdir,
        None,
        runtime.default_agent.as_deref(),
    )
    .await
    .context("resolve default agent")?;
    let session_model = if has_explicit_model {
        agent.model.clone()
    } else {
        built
            .effective_root_model(&agent, &agent.workdir)
            .await
            .context("resolve headless root Agent model")?
    };
    let asks = built
        .take_asks()
        .ok_or_else(|| anyhow::anyhow!("asks receiver missing"))?;
    let _ = built.take_questions();
    let _mcp_manager = built.mcp_control();
    let _plugin_host = built.plugin_host();
    let _responder = spawn_reject_responder(asks);
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: session_model,
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .context("create session")?;
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
    engine
        .admit_user_prompt(session, prompt)
        .await
        .context("admit prompt")?;
    let cancel = CancellationToken::new();
    let (turn, stop) = run_until_stopped(
        &built,
        &cancel,
        engine.run_turn(session, &agent, cancel.clone()),
    )
    .await?;
    // End of the run: nothing may keep streaming after `exec` returns. Every
    // other in-flight turn (members, a deferred synthesis turn on the lead)
    // is drained before the trajectory is flushed, so stdout and the log end
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
    let shutdown = built.shutdown().await.context("shutdown spawn supervisor");
    exit_if_stopped(stop);
    // Surface the turn error only after the stdout trajectory is complete.
    turn.context("run turn")?;
    shutdown?;
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

async fn cmd_rpc(model_override: Option<String>, yolo: bool, pure: bool) -> anyhow::Result<()> {
    use std::io::BufRead as _;
    first_run_config_bootstrap(false)?;
    let has_explicit_model = model_override.is_some();
    // A temp file-backed store, not `connect_memory`: long worker turns (real
    // cargo test runs) can idle out the single in-memory pooled connection,
    // which drops every table mid-run. A file also enables `tail-session`.
    let goal_db = std::env::temp_dir().join(format!(
        "hya-goal-{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let store = SessionStore::connect(goal_db.to_string_lossy().as_ref())
        .await
        .context("open goal store")?;
    let runtime = resolve_runtime(model_override)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
    let mut agent = if pure {
        agent_with_model_pure(&runtime.model, runtime.reasoning)
    } else {
        agent_with_model(&runtime.model, runtime.reasoning)
    };
    let mut built = if pure {
        build_session_engine_pure(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    } else {
        build_session_engine(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    };
    let engine = built.engine();
    // Same `default_agent` precedence/failure mode as `serve` and `exec`.
    agent.name = resolve_headless_agent_name(
        &engine,
        &agent.workdir,
        None,
        runtime.default_agent.as_deref(),
    )
    .await
    .context("resolve default agent")?;
    let session_model = if has_explicit_model {
        agent.model.clone()
    } else {
        built
            .effective_root_model(&agent, &agent.workdir)
            .await
            .context("resolve RPC root Agent model")?
    };
    let asks = built
        .take_asks()
        .ok_or_else(|| anyhow::anyhow!("asks receiver missing"))?;
    let _ = built.take_questions();
    let _mcp_manager = built.mcp_control();
    let _plugin_host = built.plugin_host();
    let _responder = spawn_reject_responder(asks);
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: session_model,
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .context("create session")?;
    let mut emitted = 0usize;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.context("read stdin")?;
        match rpc::parse_rpc(&line) {
            Some(rpc::RpcRequest::Quit) => break,
            Some(rpc::RpcRequest::Prompt { text }) => {
                engine
                    .admit_user_prompt(session, text)
                    .await
                    .context("admit prompt")?;
                if let Err(error) = engine
                    .run_turn(session, &agent, CancellationToken::new())
                    .await
                {
                    // Drain members before surfacing the failure.
                    built.drain(FinishCause::LeaderFailed).await;
                    let _ = built.shutdown().await;
                    return Err(error).context("run turn");
                }
                let envelopes = engine.replay(session).await.context("replay session")?;
                for env in envelopes.iter().skip(emitted) {
                    let line = serde_json::to_string(env).context("serialize envelope")?;
                    writeln!(out, "{line}").context("write envelope")?;
                }
                emitted = envelopes.len();
                writeln!(out, "{{\"type\":\"done\"}}").context("write done marker")?;
                out.flush().context("flush stdout")?;
            }
            None => {}
        }
    }
    built
        .shutdown()
        .await
        .context("shutdown spawn supervisor")?;
    Ok(())
}

async fn cmd_goal(
    goal: String,
    max_iterations: u32,
    evaluator_model_flag: Option<String>,
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
) -> anyhow::Result<()> {
    first_run_config_bootstrap(false)?;
    let has_explicit_model = model_override.is_some();
    // A temp file-backed store, not `connect_memory`: long worker turns (real
    // cargo test runs) can idle out the single in-memory pooled connection,
    // which drops every table mid-run. A file also enables `tail-session`.
    let goal_db = std::env::temp_dir().join(format!(
        "hya-goal-{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let store = SessionStore::connect(goal_db.to_string_lossy().as_ref())
        .await
        .context("open goal store")?;
    let runtime = resolve_runtime(model_override)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
    let evaluator_router = runtime.router.clone();
    // D7 evaluator model: the CLI flag outranks config `goal.evaluator_model`;
    // with neither, the worker's current model judges.
    let evaluator_model = config::resolve_evaluator_model(
        evaluator_model_flag.as_deref(),
        config::load_goal_settings().evaluator_model.as_deref(),
        &runtime.model,
    )
    .to_string();
    let mut agent = if pure {
        agent_with_model_pure(&runtime.model, runtime.reasoning)
    } else {
        agent_with_model(&runtime.model, runtime.reasoning)
    };
    let mut built = if pure {
        build_session_engine_pure(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    } else {
        build_session_engine(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    };
    let engine = built.engine();
    // Same `default_agent` precedence/failure mode as `serve` and `exec`.
    agent.name = resolve_headless_agent_name(
        &engine,
        &agent.workdir,
        None,
        runtime.default_agent.as_deref(),
    )
    .await
    .context("resolve default agent")?;
    let session_model = if has_explicit_model {
        agent.model.clone()
    } else {
        built
            .effective_root_model(&agent, &agent.workdir)
            .await
            .context("resolve goal root Agent model")?
    };
    let binding = engine
        .bind_root_runtime(&agent.workdir)
        .await
        .context("bind goal evaluator resources")?;
    let asks = built
        .take_asks()
        .ok_or_else(|| anyhow::anyhow!("asks receiver missing"))?;
    let _ = built.take_questions();
    let _mcp_manager = built.mcp_control();
    let _plugin_host = built.plugin_host();
    let _responder = spawn_reject_responder(asks);
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: session_model,
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .context("create session")?;
    // Selection point (design §4.5): a registered `goal.evaluate` hook
    // provider outranks the built-in evaluator. The probe is the trait
    // capability probe (default false), so any dispatcher without the hook —
    // or a selection that cannot confirm registration — fails open to the D7
    // model evaluator instead of erroring startup.
    let fallback: Arc<dyn GoalEvaluator> = Arc::new(
        ModelGoalEvaluator::new(Arc::new(evaluator_router), ModelRef::new(&evaluator_model))
            .with_system_prompt(
                binding
                    .bundle_skill_content("hya/goal-loop", "evaluator-prompt")
                    .map(skill_prompt_body)
                    .unwrap_or("You are an independent goal verifier. No tools."),
            ),
    );
    let dispatcher = goal_loop_hook_chain(&binding, agent.name.as_str(), built.plugin_host());
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
        &built,
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
    let shutdown = built.shutdown().await.context("shutdown spawn supervisor");
    exit_if_stopped(stop);
    let outcome = outcome.context("run goal")?;
    println!("goal outcome: {outcome:?}");
    shutdown?;
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
    first_run_config_bootstrap(false)?;
    let has_explicit_model = model_override.is_some();
    // A temp file-backed store, not `connect_memory`: long worker turns (real
    // cargo test runs) can idle out the single in-memory pooled connection,
    // which drops every table mid-run. A file also enables `tail-session`.
    let goal_db = std::env::temp_dir().join(format!(
        "hya-goal-{}.db",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let store = SessionStore::connect(goal_db.to_string_lossy().as_ref())
        .await
        .context("open goal store")?;
    let runtime = resolve_runtime(model_override)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
    let evaluator_router = runtime.router.clone();
    // D7 evaluator model plumbing, shared with goal mode: CLI flag outranks
    // config `goal.evaluator_model`; with neither, the worker's model judges.
    let evaluator_model = config::resolve_evaluator_model(
        evaluator_model_flag.as_deref(),
        config::load_goal_settings().evaluator_model.as_deref(),
        &runtime.model,
    )
    .to_string();
    let mut agent = if pure {
        agent_with_model_pure(&runtime.model, runtime.reasoning)
    } else {
        agent_with_model(&runtime.model, runtime.reasoning)
    };
    let mut built = if pure {
        build_session_engine_pure(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    } else {
        build_session_engine(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?
    };
    let engine = built.engine();
    // Same `default_agent` precedence/failure mode as `serve` and `exec`.
    agent.name = resolve_headless_agent_name(
        &engine,
        &agent.workdir,
        None,
        runtime.default_agent.as_deref(),
    )
    .await
    .context("resolve default agent")?;
    let session_model = if has_explicit_model {
        agent.model.clone()
    } else {
        built
            .effective_root_model(&agent, &agent.workdir)
            .await
            .context("resolve loop root Agent model")?
    };
    let binding = engine
        .bind_root_runtime(&agent.workdir)
        .await
        .context("bind loop evaluator resources")?;
    let asks = built
        .take_asks()
        .ok_or_else(|| anyhow::anyhow!("asks receiver missing"))?;
    let _ = built.take_questions();
    let _mcp_manager = built.mcp_control();
    let _plugin_host = built.plugin_host();
    let _responder = spawn_reject_responder(asks);
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: session_model,
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .context("create session")?;
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
    let evaluator_dispatcher =
        goal_loop_hook_chain(&binding, agent.name.as_str(), built.plugin_host());
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
        &built,
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
    if stop.is_some() {
        let _ = built.shutdown().await;
        exit_if_stopped(stop);
    }
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = built.shutdown().await;
            return Err(error).context("run loop");
        }
    };
    println!("loop outcome: {outcome:?}");
    // A broken deterministic condition stops with a "broken condition"
    // reason: surface it as a failure, never as a finished loop.
    if let RunOutcome::Achieved { reason, .. } = &outcome
        && reason.starts_with("broken condition")
    {
        eprintln!(
            "warning: the loop condition itself is broken, so this is NOT a success: {reason}"
        );
        let _ = built.shutdown().await;
        anyhow::bail!("loop ended on a broken condition; not a success");
    }
    built
        .shutdown()
        .await
        .context("shutdown spawn supervisor")?;
    Ok(())
}

fn skill_prompt_body(content: &str) -> &str {
    content
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n").map(|(_, body)| body.trim()))
        .unwrap_or(content)
}

fn goal_loop_hook_chain(
    binding: &TurnBinding,
    stable_agent_id: &str,
    global: Arc<dyn HookDispatcher>,
) -> Arc<dyn HookDispatcher> {
    let active_is_goal_loop = binding
        .resolve_agent(stable_agent_id)
        .is_some_and(|agent| agent.origin.bundle_id() == Some("hya/goal-loop"));
    let selected = if active_is_goal_loop {
        Vec::new()
    } else {
        binding.bundle_hooks_for_agent(stable_agent_id)
    };
    let agentless = binding
        .bundle_catalog()
        .bundles()
        .iter()
        .filter(|bundle| {
            bundle.plugin_bundle().is_some() && bundle.identity().id != "hya/goal-loop"
        })
        .filter_map(|bundle| binding.bundle_hooks(&bundle.identity().id))
        .collect();
    Arc::new(HookChain::new(ordered_evaluator_hooks(
        global,
        agentless,
        selected,
        binding.bundle_hooks("hya/goal-loop"),
    )))
}

fn ordered_evaluator_hooks(
    global: Arc<dyn HookDispatcher>,
    agentless: Vec<Arc<dyn HookDispatcher>>,
    selected: Vec<Arc<dyn HookDispatcher>>,
    intelligent: Option<Arc<dyn HookDispatcher>>,
) -> Vec<Arc<dyn HookDispatcher>> {
    let mut hooks = vec![global];
    for hook in agentless.into_iter().chain(selected).chain(intelligent) {
        if !hooks.iter().any(|existing| Arc::ptr_eq(existing, &hook)) {
            hooks.push(hook);
        }
    }
    hooks
}

async fn cmd_tail_session(id: String, db: String) -> anyhow::Result<()> {
    let session: SessionId = id.parse().context("parse session id")?;
    let store = open_store(&db).await?;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let web_port = cli_args::bare_web_port(&cli)?;
    let model = cli.model.clone();
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
        // Bare `hya` on a terminal starts the TUI and the WebUI next to an
        // in-process server (frontend.rs); without a terminal it only points
        // at the other surfaces.
        None => {
            use std::io::IsTerminal as _;
            if frontend::should_launch(
                std::io::stdin().is_terminal(),
                std::io::stdout().is_terminal(),
            ) {
                return frontend::run(frontend::LaunchRequest {
                    port: web_port,
                    db: resolve_interactive_db(&db),
                    model,
                    yolo,
                    pure,
                    state_dir: state_dir(),
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
        Some(Command::Run {
            message,
            format,
            json,
        }) => {
            cmd_exec(
                message.join(" "),
                model,
                &db,
                yolo,
                json || format == "json",
                pure,
            )
            .await
        }
        Some(Command::Exec { prompt, json }) => {
            cmd_exec(prompt, model, &db, yolo, json, pure).await
        }
        Some(Command::Serve {
            bind,
            hostname,
            port,
            mdns,
            db: command_db,
            ..
        }) => {
            serve::cmd_serve(
                cli_args::serve_bind(bind, hostname, port, mdns),
                command_db.unwrap_or_else(|| db.clone()),
                model,
                yolo,
                pure,
            )
            .await
        }
        Some(Command::TailSession { id, db: command_db }) => {
            let path = command_db.unwrap_or_else(|| db.clone());
            cmd_tail_session(id, resolve_interactive_db(&path)).await
        }
        Some(Command::Login { provider, token }) => auth_cmd::login(provider, token).await,
        Some(Command::Oauth { command }) => auth_cmd::run_oauth(command).await,
        Some(Command::Auth { command }) => auth_cmd::run(command).await,
        Some(Command::Agent { command }) => agent_cmd::run(command),
        Some(Command::Bundle { command }) => bundle_cmd::run(command).await,
        Some(Command::Workflow { command }) => {
            workflow_cmd::run(command, model, &db, yolo, pure).await
        }
        Some(Command::Models {
            provider,
            verbose,
            refresh,
        }) => {
            first_run_config_bootstrap(false)?;
            let mut runtime = resolve_runtime(model).await;
            if refresh {
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
        Some(Command::Rpc) => cmd_rpc(model, yolo, pure).await,
        // The update TCB runs without composing any runtime: no config,
        // bundles, providers, plugins, MCP, or session store.
        Some(Command::Update { command }) => {
            let mut stdout = std::io::stdout().lock();
            hya_updater::cli::run(command, &mut stdout)
                .map_err(|error| anyhow::anyhow!("hya update: {error}"))
        }
        // The proxy composes no runtime either: no config, providers, MCP,
        // or session store — it only opens network connections.
        Some(Command::Proxy { args }) => proxy_cmd::cmd_proxy(args).await,
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
            Arc::clone(&global),
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
}
