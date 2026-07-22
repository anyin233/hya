//! `hya-backend` — backend umbrella binary. Bare `hya-backend` launches the `hya` frontend TUI;
//! subcommands cover headless `exec`, `-p` goal mode, HTTP/SSE
//! `serve`, and `tail-session`.
//!
//! Models come from hya's config (`~/.config/hya/config.yaml`): its providers +
//! keys build real OpenAI/Anthropic/Google routes. With no usable config, an
//! offline echo provider keeps the whole stack runnable.

// allow: SIZE_OK — Phase 1 only extracts bootstrap glue; CLI command bodies stay here unchanged.

mod agent_cmd;
mod auth_cmd;
mod cli_args;
mod models_cmd;
mod rpc;
mod serve;

pub use hya_app::{auth, config, formatter_config, permission, plugins};

use std::collections::BTreeMap;
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
    InvocationPolicy, RuntimeConfig, WebSearchConfig, agent_with_model, build_session_engine,
    compaction_config, discover_context_files, host_info, offline_router, open_store,
    resolve_runtime, spawn_team_supervisor, today,
};

pub(crate) fn first_run_config_bootstrap(interactive: bool) -> anyhow::Result<()> {
    config::first_run_config_bootstrap(interactive)
}

/// Resolve the SQLite path for bare interactive `hya-backend` startup.
///
/// Empty `--db` (CLI default) maps to `$XDG_STATE_HOME/hya/sessions.db` so
/// `hya --continue` / `hya -s` can resume after restarts. Explicit `--db ""`
/// is not distinguishable from the clap default here; use a real path or the
/// `hya` frontend's `HYA_DB=` empty override for intentional in-memory runs.
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
) -> anyhow::Result<()> {
    first_run_config_bootstrap(false)?;
    let store = open_store(db).await?;
    let runtime = resolve_runtime(model_override).with_yolo(yolo);
    let agent = agent_with_model(&runtime.model, runtime.reasoning);
    let (engine, asks, _, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        runtime.router,
        &agent,
        runtime.mcp,
        runtime.plugins,
        (runtime.websearch, runtime.permission),
        true,
    )
    .await;
    let _responder = spawn_reject_responder(asks);
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .context("create session")?;
    engine
        .admit_user_prompt(session, prompt)
        .await
        .context("admit prompt")?;
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .context("run turn")?;
    if json {
        let envelopes = engine.replay(session).await.context("replay session")?;
        let mut out = std::io::stdout().lock();
        for env in &envelopes {
            let line = serde_json::to_string(env).context("serialize envelope")?;
            writeln!(out, "{line}").context("write envelope")?;
        }
    } else {
        let projection = engine
            .read_projection(session)
            .await
            .context("read projection")?;
        print!("{}", render_transcript(&projection));
    }
    Ok(())
}

async fn cmd_rpc(model_override: Option<String>, yolo: bool) -> anyhow::Result<()> {
    use std::io::BufRead as _;
    first_run_config_bootstrap(false)?;
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let runtime = resolve_runtime(model_override).with_yolo(yolo);
    let agent = agent_with_model(&runtime.model, runtime.reasoning);
    let (engine, asks, _, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        runtime.router,
        &agent,
        runtime.mcp,
        runtime.plugins,
        (runtime.websearch, runtime.permission),
        true,
    )
    .await;
    let _responder = spawn_reject_responder(asks);
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
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
    Ok(())
}

async fn cmd_goal(
    goal: String,
    max_iterations: u32,
    model_override: Option<String>,
    yolo: bool,
) -> anyhow::Result<()> {
    first_run_config_bootstrap(false)?;
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let runtime = resolve_runtime(model_override).with_yolo(yolo);
    let evaluator_router = runtime.router.clone();
    let agent = agent_with_model(&runtime.model, runtime.reasoning);
    let (engine, asks, _, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        runtime.router,
        &agent,
        runtime.mcp,
        runtime.plugins,
        (runtime.websearch, runtime.permission),
        true,
    )
    .await;
    let _responder = spawn_reject_responder(asks);
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
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
    Ok(())
}

async fn cmd_tail_session(id: String, db: String) -> anyhow::Result<()> {
    let session: SessionId = id.parse().context("parse session id")?;
    let store = open_store(&db).await?;
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let (engine, _asks, _, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (WebSearchConfig::default(), InvocationPolicy::default()),
        true,
    )
    .await;
    let envelopes = engine.replay(session).await.context("replay session")?;
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
    cli.validate().map_err(anyhow::Error::msg)?;
    let model = cli.model.clone();
    let yolo = cli.yolo;
    let db = cli.db.clone();
    let resume = cli.resume.clone();
    if let Some(goal) = cli.prompt {
        return cmd_goal(goal, cli.max_iterations, model, yolo).await;
    }
    match cli.command {
        // Interactive TUI: default to a durable SQLite path so sessions survive restarts.
        None => {
            let db = resolve_interactive_db(&db);
            serve::cmd_tui_hya(model, db, yolo, resume).await
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
            )
            .await
        }
        Some(Command::Exec { prompt, json }) => cmd_exec(prompt, model, &db, yolo, json).await,
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
        Some(Command::Models {
            provider,
            verbose,
            refresh,
        }) => {
            first_run_config_bootstrap(false)?;
            let runtime = resolve_runtime(model);
            models_cmd::cmd_models(runtime.models, &runtime.model, provider, verbose, refresh)
        }
        Some(Command::Sessions { db: command_db }) => {
            let path = command_db.unwrap_or_else(|| db.clone());
            cmd_sessions(resolve_interactive_db(&path)).await
        }
        Some(Command::Rpc) => cmd_rpc(model, yolo).await,
    }
}
