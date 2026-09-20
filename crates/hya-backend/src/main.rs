//! `hya-backend` — backend umbrella binary and the workspace's only shipped binary.
//! Bare `hya-backend` prints a guidance banner (no interactive frontend is bundled);
//! subcommands cover headless `exec`, `-p` goal mode, HTTP/SSE `serve`, and
//! `tail-session`.
//!
//! Models come from Hya's provider declarations. Explicit model lists or bounded
//! startup discovery build authenticated or anonymous routes. With no resolved
//! live model rows, the offline echo provider keeps the whole stack runnable.

// allow: SIZE_OK — Phase 1 only extracts bootstrap glue; CLI command bodies stay here unchanged.

mod agent_cmd;
mod auth_cmd;
mod bundle_cmd;
mod cli_args;
mod exec_stream;
mod models_cmd;
mod rpc;
mod serve;
mod workflow_cmd;

pub use hya_app::{auth, config, formatter_config, permission, plugins};

use std::io::Write as _;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use hya_core::completion::render_transcript;
use hya_core::{CreateSession, GoalEvaluator, ModelGoalEvaluator, SafetyCaps, run_goal};
use hya_proto::{ModelRef, SessionId};
use hya_store::SessionStore;
use tokio_util::sync::CancellationToken;

use crate::permission::spawn_reject_responder;
use cli_args::{Cli, Command};

pub use hya_app::{
    InvocationPolicy, RuntimeConfig, WebSearchConfig, agent_base_with_model, agent_with_model,
    agent_with_model_pure, build_session_engine, build_session_engine_pure, compaction_config,
    discover_context_files, host_info, offline_router, open_store, resolve_runtime,
    spawn_team_supervisor, today,
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
    dir.join("sessions.db").to_string_lossy().into_owned()
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
    let agent = if pure {
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
    let session_model = if has_explicit_model {
        agent.model.clone()
    } else {
        built
            .effective_root_model(&agent, &agent.workdir)
            .await
            .context("resolve headless root Agent model")?
    };
    let engine = built.engine();
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
    let turn = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await;
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
    // Surface the turn error only after the stdout trajectory is complete.
    turn.context("run turn")?;
    built
        .shutdown()
        .await
        .context("shutdown spawn supervisor")?;
    Ok(())
}

async fn cmd_rpc(model_override: Option<String>, yolo: bool, pure: bool) -> anyhow::Result<()> {
    use std::io::BufRead as _;
    first_run_config_bootstrap(false)?;
    let has_explicit_model = model_override.is_some();
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let runtime = resolve_runtime(model_override)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
    let agent = if pure {
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
    let session_model = if has_explicit_model {
        agent.model.clone()
    } else {
        built
            .effective_root_model(&agent, &agent.workdir)
            .await
            .context("resolve RPC root Agent model")?
    };
    let engine = built.engine();
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
                engine
                    .run_turn(session, &agent, CancellationToken::new())
                    .await
                    .context("run turn")?;
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
    model_override: Option<String>,
    yolo: bool,
    pure: bool,
) -> anyhow::Result<()> {
    first_run_config_bootstrap(false)?;
    let has_explicit_model = model_override.is_some();
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let runtime = resolve_runtime(model_override)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
    let evaluator_router = runtime.router.clone();
    let agent = if pure {
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
    let session_model = if has_explicit_model {
        agent.model.clone()
    } else {
        built
            .effective_root_model(&agent, &agent.workdir)
            .await
            .context("resolve goal root Agent model")?
    };
    let engine = built.engine();
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
        })
        .await
        .context("create session")?;
    let evaluator: Arc<dyn GoalEvaluator> = Arc::new(ModelGoalEvaluator::new(
        Arc::new(evaluator_router),
        ModelRef::new(&runtime.model),
    ));
    let caps = SafetyCaps {
        max_iterations,
        ..SafetyCaps::default()
    };
    let outcome = run_goal(
        engine.clone(),
        session,
        agent,
        goal,
        evaluator,
        caps,
        CancellationToken::new(),
    )
    .await
    .context("run goal")?;
    println!("goal outcome: {outcome:?}");
    built
        .shutdown()
        .await
        .context("shutdown spawn supervisor")?;
    Ok(())
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

async fn cmd_sessions(db: String) -> anyhow::Result<()> {
    let store = open_store(&db).await?;
    let sessions = store.list_sessions().await.context("list sessions")?;
    if sessions.is_empty() {
        println!("no sessions found in {db}");
        return Ok(());
    }
    for s in sessions {
        println!(
            "{}  events={}  started_ms={}",
            s.session, s.events, s.started_millis
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let model = cli.model.clone();
    let yolo = cli.yolo;
    let pure = cli.pure;
    let db = cli.db.clone();
    if let Some(goal) = cli.prompt {
        return cmd_goal(goal, cli.max_iterations, model, yolo, pure).await;
    }
    match cli.command {
        // No interactive frontend is bundled anymore: bare startup only points
        // at the headless and server surfaces.
        None => {
            println!(
                "hya {} — a multi-agent coding agent",
                env!("CARGO_PKG_VERSION")
            );
            println!(
                "No interactive frontend is bundled. Try `hya-backend serve`, \
                 `hya-backend exec \"<prompt>\"`, `hya-backend -p \"<goal>\"`, or `hya-backend --help`."
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
        Some(Command::Models { provider, verbose }) => {
            first_run_config_bootstrap(false)?;
            let mut runtime = resolve_runtime(model).await;
            if !runtime.pending_discovery.is_empty() {
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
        Some(Command::Sessions { db: command_db }) => {
            let path = command_db.unwrap_or_else(|| db.clone());
            cmd_sessions(resolve_interactive_db(&path)).await
        }
        Some(Command::Rpc) => cmd_rpc(model, yolo, pure).await,
    }
}
