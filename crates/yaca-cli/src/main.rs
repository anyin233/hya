//! `yaca` — umbrella binary. Bare `yaca` launches the interactive TUI (the
//! default entry); subcommands cover headless `exec`, `-p` goal mode, HTTP/SSE
//! `serve`, and `tail-session`.
//!
//! Models come from opencode's config (`~/.config/opencode/opencode.json`):
//! its providers + keys are reused to build real OpenAI/Anthropic routes. With no
//! usable config, an offline echo provider keeps the whole stack runnable.

mod config;
mod tui;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;
use yaca_core::completion::render_transcript;
use yaca_core::{
    AgentSpec, CreateSession, EventBus, GoalEvaluator, ModelGoalEvaluator, SafetyCaps,
    SessionEngine, run_goal,
};
use yaca_proto::{AgentName, ModelRef, SessionId};
use yaca_provider::{DevProvider, ProviderRouter};
use yaca_server::{AppState, router as server_router};
use yaca_store::SessionStore;
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

#[derive(Parser)]
#[command(
    name = "yaca",
    version,
    about = "yaca — a multi-agent coding agent",
    long_about = None
)]
struct Cli {
    /// Headless goal mode: iterate the agent until an independent evaluator
    /// reports the goal met, or the iteration cap trips.
    #[arg(short = 'p', long = "prompt", value_name = "GOAL")]
    prompt: Option<String>,
    /// Iteration cap for `-p` goal mode.
    #[arg(long, default_value_t = 6)]
    max_iterations: u32,
    /// Model id to use (overrides opencode default + `YACA_MODEL`).
    #[arg(long, global = true, value_name = "MODEL")]
    model: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run a single prompt headlessly and print the resulting transcript.
    Exec {
        /// The user prompt to send to the agent.
        prompt: String,
    },
    /// Start the HTTP + SSE server.
    Serve {
        /// Address to bind. Use `127.0.0.1:0` for an ephemeral port.
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: String,
        /// SQLite database path. Empty string uses an in-memory store.
        #[arg(long, default_value = "")]
        db: String,
    },
    /// Replay a session's event log from a database as JSON lines.
    TailSession {
        /// Session id (UUID).
        id: String,
        /// SQLite database path the session was written to.
        #[arg(long)]
        db: String,
    },
}

fn agent_with_model(model: &str) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new(model),
        system_prompt: "You are yaca, a coding agent.".to_string(),
        workdir: PathBuf::from("."),
    }
}

fn build_session_engine(store: SessionStore, router: ProviderRouter) -> Arc<SessionEngine> {
    let tools = Arc::new(ToolRegistry::builtins());
    // Auto-allow read-only tools; mutating tools (edit/bash) stay Ask, which errors
    // safely until an interactive permission prompt is wired.
    let rules = PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Glob, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]);
    let (permission, _asks) = PermissionPlane::new(rules);
    Arc::new(SessionEngine::new(
        store,
        Arc::new(router),
        tools,
        permission,
        EventBus::default(),
    ))
}

fn offline_router(model_override: Option<String>) -> (ProviderRouter, String) {
    let router = ProviderRouter::new().with(Arc::new(DevProvider::new()));
    (
        router,
        model_override.unwrap_or_else(|| "offline".to_string()),
    )
}

/// Resolve a provider router + active model from opencode's config, falling back
/// to the offline echo provider when no usable config is present.
fn resolve_router(model_override: Option<String>) -> (ProviderRouter, String) {
    match config::load() {
        Ok(Some(cfg)) => {
            let model = model_override
                .or_else(|| std::env::var("YACA_MODEL").ok())
                .unwrap_or(cfg.default_model);
            (cfg.router, model)
        }
        Ok(None) => offline_router(model_override),
        Err(e) => {
            eprintln!("yaca: opencode config error ({e:#}); using the offline provider");
            offline_router(model_override)
        }
    }
}

async fn open_store(db: &str) -> anyhow::Result<SessionStore> {
    if db.is_empty() {
        SessionStore::connect_memory()
            .await
            .context("open in-memory store")
    } else {
        SessionStore::connect(db)
            .await
            .with_context(|| format!("open store at {db}"))
    }
}

async fn cmd_exec(prompt: String, model_override: Option<String>) -> anyhow::Result<()> {
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let (router, model) = resolve_router(model_override);
    let engine = build_session_engine(store, router);
    let agent = agent_with_model(&model);
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
    let projection = engine
        .read_projection(session)
        .await
        .context("read projection")?;
    print!("{}", render_transcript(&projection));
    Ok(())
}

async fn cmd_goal(
    goal: String,
    max_iterations: u32,
    model_override: Option<String>,
) -> anyhow::Result<()> {
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let (router, model) = resolve_router(model_override);
    let engine = build_session_engine(store, router.clone());
    let agent = agent_with_model(&model);
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
        Arc::new(router),
        ModelRef::new(&model),
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

async fn cmd_tui(model_override: Option<String>) -> anyhow::Result<()> {
    use std::io::IsTerminal as _;
    if !std::io::stdout().is_terminal() {
        println!(
            "yaca {} — a multi-agent coding agent",
            env!("CARGO_PKG_VERSION")
        );
        println!(
            "The interactive TUI needs a terminal. Try `yaca exec \"<prompt>\"`, \
             `yaca -p \"<goal>\"`, or `yaca --help`."
        );
        return Ok(());
    }
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let (router, model) = resolve_router(model_override);
    let engine = build_session_engine(store, router);
    let agent = agent_with_model(&model);
    tui::run(engine, agent, model).await
}

async fn cmd_serve(bind: String, db: String, model_override: Option<String>) -> anyhow::Result<()> {
    let store = open_store(&db).await?;
    let (router, model) = resolve_router(model_override);
    let engine = build_session_engine(store, router);
    let state = AppState {
        engine,
        agent: Arc::new(agent_with_model(&model)),
    };
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    let addr = listener.local_addr().context("read local addr")?;
    println!("yaca server listening on http://{addr}");
    axum::serve(listener, server_router(state))
        .await
        .context("serve http")?;
    Ok(())
}

async fn cmd_tail_session(id: String, db: String) -> anyhow::Result<()> {
    let uuid = uuid::Uuid::parse_str(&id).context("parse session id")?;
    let session = SessionId::from_uuid(uuid);
    let store = open_store(&db).await?;
    let (router, _model) = offline_router(None);
    let engine = build_session_engine(store, router);
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let model = cli.model.clone();
    if let Some(goal) = cli.prompt {
        return cmd_goal(goal, cli.max_iterations, model).await;
    }
    match cli.command {
        None => cmd_tui(model).await,
        Some(Command::Exec { prompt }) => cmd_exec(prompt, model).await,
        Some(Command::Serve { bind, db }) => cmd_serve(bind, db, model).await,
        Some(Command::TailSession { id, db }) => cmd_tail_session(id, db).await,
    }
}
