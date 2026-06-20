//! `yaca` — umbrella binary: headless `exec`, HTTP/SSE `serve`, and `tail-session`.
//!
//! This build ships a dev engine backed by an offline provider so the whole stack
//! (engine -> provider -> store -> projection / HTTP) runs without API keys. Wire
//! real OpenAI / Anthropic providers from config to get live responses (see README).

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
use yaca_proto::{AgentName, FinishReason, ModelRef, SessionId};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

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

fn dev_agent() -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "You are yaca, a coding agent.".to_string(),
        workdir: PathBuf::from("."),
    }
}

fn dev_engine(store: SessionStore) -> Arc<SessionEngine> {
    // Offline provider: deterministic, no network. Matches any model ref.
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text(
            "(yaca dev provider) No live model is configured. \
             Configure a provider to get real responses."
                .to_string(),
        ),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    Arc::new(SessionEngine::new(
        store,
        router,
        tools,
        permission,
        EventBus::default(),
    ))
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

async fn cmd_exec(prompt: String) -> anyhow::Result<()> {
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let engine = dev_engine(store);
    let agent = dev_agent();
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

async fn cmd_goal(goal: String, max_iterations: u32) -> anyhow::Result<()> {
    let store = SessionStore::connect_memory()
        .await
        .context("open in-memory store")?;
    let engine = dev_engine(store);
    let agent = dev_agent();
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
        Arc::new(
            ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(vec![vec![
                FakeStep::Text(
                    "{\"met\":false,\"reason\":\"offline dev provider cannot satisfy goals\"}"
                        .to_string(),
                ),
                FakeStep::Finish(FinishReason::Stop),
            ]]))),
        ),
        ModelRef::new("fake"),
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

async fn cmd_serve(bind: String, db: String) -> anyhow::Result<()> {
    let store = open_store(&db).await?;
    let engine = dev_engine(store);
    let state = AppState {
        engine,
        agent: Arc::new(dev_agent()),
    };
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    let addr = listener.local_addr().context("read local addr")?;
    println!("yaca server listening on http://{addr}");
    axum::serve(listener, router(state))
        .await
        .context("serve http")?;
    Ok(())
}

async fn cmd_tail_session(id: String, db: String) -> anyhow::Result<()> {
    let uuid = uuid::Uuid::parse_str(&id).context("parse session id")?;
    let session = SessionId::from_uuid(uuid);
    let store = open_store(&db).await?;
    let engine = dev_engine(store);
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
    if let Some(goal) = cli.prompt {
        return cmd_goal(goal, cli.max_iterations).await;
    }
    match cli.command {
        None => {
            println!(
                "yaca {} — a multi-agent coding agent",
                env!("CARGO_PKG_VERSION")
            );
            println!("Try `yaca exec \"<prompt>\"`, `yaca serve`, or `yaca --help`.");
            Ok(())
        }
        Some(Command::Exec { prompt }) => cmd_exec(prompt).await,
        Some(Command::Serve { bind, db }) => cmd_serve(bind, db).await,
        Some(Command::TailSession { id, db }) => cmd_tail_session(id, db).await,
    }
}
