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
mod tui;

pub use hya_app::{auth, config, formatter_config, permission, plugins, skills};

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

use crate::permission::spawn_auto_responder;
use cli_args::{Cli, Command};

pub use hya_app::{
    RuntimeConfig, agent_with_model, build_session_engine, compaction_config,
    discover_context_files, headless_policy, host_info, offline_router, open_store,
    resolve_runtime, skill_dirs, spawn_team_supervisor, today,
};

async fn cmd_exec(
    prompt: String,
    model_override: Option<String>,
    db: &str,
    yolo: bool,
    json: bool,
) -> anyhow::Result<()> {
    let store = open_store(db).await?;
    let runtime = resolve_runtime(model_override);
    let (engine, asks, _, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        runtime.router,
        &runtime.model,
        runtime.mcp,
        runtime.plugins,
    )
    .await;
    let agent = agent_with_model(&runtime.model);
    let _responder = spawn_auto_responder(asks, headless_policy(yolo, &agent.workdir));
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
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let runtime = resolve_runtime(model_override);
    let (engine, asks, _, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        runtime.router,
        &runtime.model,
        runtime.mcp,
        runtime.plugins,
    )
    .await;
    let agent = agent_with_model(&runtime.model);
    let _responder = spawn_auto_responder(asks, headless_policy(yolo, &agent.workdir));
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
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let runtime = resolve_runtime(model_override);
    let evaluator_router = runtime.router.clone();
    let (engine, asks, _, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        runtime.router,
        &runtime.model,
        runtime.mcp,
        runtime.plugins,
    )
    .await;
    let agent = agent_with_model(&runtime.model);
    let _responder = spawn_auto_responder(asks, headless_policy(yolo, &agent.workdir));
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

async fn cmd_tui(
    model_override: Option<String>,
    db: String,
    resume: Option<String>,
    yolo: bool,
) -> anyhow::Result<()> {
    use std::io::IsTerminal as _;
    if !std::io::stdout().is_terminal() {
        println!(
            "hya {} — a multi-agent coding agent",
            env!("CARGO_PKG_VERSION")
        );
        println!(
            "The interactive TUI needs a terminal. Try `hya-backend exec \"<prompt>\"`, \
             `hya-backend -p \"<goal>\"`, or `hya-backend --help`."
        );
        return Ok(());
    }
    let store = open_store(&db).await?;
    let runtime = resolve_runtime(model_override);
    // Interactive startup (stdout is a terminal, checked above): explain the
    // missing config and the offline fallback. Goes to stderr only.
    if let Some(notice) = &runtime.offline_notice {
        notice.emit();
    }
    let (engine, asks, questions, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        runtime.router,
        &runtime.model,
        runtime.mcp,
        runtime.plugins,
    )
    .await;
    let agent = agent_with_model(&runtime.model);
    let session = match resume {
        Some(id) => {
            let raw = id.strip_prefix("ses_").unwrap_or(&id);
            let uuid = uuid::Uuid::parse_str(raw).context("parse resume session id")?;
            SessionId::from_uuid(uuid)
        }
        None => engine
            .create(CreateSession {
                parent: None,
                agent: agent.name.clone(),
                model: agent.model.clone(),
                workdir: agent.workdir.to_string_lossy().into_owned(),
            })
            .await
            .context("create session")?,
    };
    tui::run(
        engine,
        agent,
        tui::RunOptions {
            model: runtime.model,
            models: runtime.models,
            asks,
            questions,
            initial_session: session,
            initial_yolo: yolo,
        },
    )
    .await
}

async fn cmd_tail_session(id: String, db: String) -> anyhow::Result<()> {
    let uuid = uuid::Uuid::parse_str(&id).context("parse session id")?;
    let session = SessionId::from_uuid(uuid);
    let store = open_store(&db).await?;
    let (router, model) = offline_router(None);
    let (engine, _asks, _, _mcp_manager, _plugin_host) =
        build_session_engine(store, router, &model, BTreeMap::new(), Vec::new()).await;
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
        None if cli.mini => cmd_tui(model, db, resume, yolo).await,
        None => serve::cmd_tui_hya(model, db, yolo).await,
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
            db,
            ..
        }) => {
            serve::cmd_serve(
                cli_args::serve_bind(bind, hostname, port, mdns),
                db,
                model,
                yolo,
            )
            .await
        }
        Some(Command::TailSession { id, db }) => cmd_tail_session(id, db).await,
        Some(Command::Login { provider, token }) => auth_cmd::login(provider, token).await,
        Some(Command::Auth { command }) => auth_cmd::run(command).await,
        Some(Command::Agent { command }) => agent_cmd::run(command),
        Some(Command::Models {
            provider,
            verbose,
            refresh,
        }) => {
            let runtime = resolve_runtime(model);
            models_cmd::cmd_models(runtime.models, &runtime.model, provider, verbose, refresh)
        }
        Some(Command::Sessions { db }) => cmd_sessions(db).await,
        Some(Command::Rpc) => cmd_rpc(model, yolo).await,
    }
}
