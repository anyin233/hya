//! `yaca` — umbrella binary. Bare `yaca` launches the interactive TUI (the
//! default entry); subcommands cover headless `exec`, `-p` goal mode, HTTP/SSE
//! `serve`, and `tail-session`.
//!
//! Models come from yaca's config (`~/.config/yaca/config.yaml`): its providers +
//! keys build real OpenAI/Anthropic/Google routes. With no usable config, an
//! offline echo provider keeps the whole stack runnable.

mod auth;
mod commands;
mod config;
mod permission;
mod plugins;
mod rpc;
mod skills;
mod tui;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;
use yaca_core::completion::render_transcript;
use yaca_core::{
    AgentSpec, CompactionConfig, CreateSession, EventBus, GoalEvaluator, MemberSpec, MemberStatus,
    ModelGoalEvaluator, ModelSummarizer, PromptEnv, SafetyCaps, SessionEngine, Summarizer,
    TeamEvidenceEnvelope, build_system_prompt, project_envelope, run_goal, run_team,
};
use yaca_proto::{AgentName, MemberId, ModelRef, SessionId};
use yaca_provider::{DevProvider, ProviderRouter};
use yaca_server::{AppState, router as server_router};
use yaca_store::SessionStore;
use yaca_tool::{
    Action, AskRequest, InteractionPlane, MemberOutcome, Mode, PermissionPlane, PermissionRules,
    QuestionRequest, Rule, SpawnRequest, SpawnerPlane, ToolRegistry,
};

use yaca_mcp::{McpManager, McpServerConfig};
use yaca_plugin::config::PluginSpec;
use yaca_plugin::{HostInfo, PermissionBridge, PluginHost};

use crate::permission::{PermissionPolicy, spawn_auto_responder};

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
    /// Model id to use (overrides config `default_model` + `YACA_MODEL`).
    #[arg(long, global = true, value_name = "MODEL")]
    model: Option<String>,
    /// Auto-approve every tool action (edit/write/shell anywhere). Use with care.
    #[arg(long, global = true)]
    yolo: bool,
    /// SQLite database for the interactive TUI (empty = in-memory).
    #[arg(long, default_value = "")]
    db: String,
    /// Resume an existing session id in the interactive TUI.
    #[arg(long)]
    resume: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run a single prompt headlessly and print the resulting transcript.
    Exec {
        /// The user prompt to send to the agent.
        prompt: String,
        /// Emit the event stream as JSONL instead of a rendered transcript.
        #[arg(long)]
        json: bool,
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
    /// Save an auth token for a provider id (used instead of an inline api_key).
    Login {
        /// Provider id as it appears in your yaca config.
        provider: String,
        /// The bearer/API token to store.
        token: String,
    },
    /// List sessions stored in a database.
    Sessions {
        /// SQLite database path.
        #[arg(long)]
        db: String,
    },
    /// JSONL RPC over stdin/stdout: read {"type":"prompt","text":...} lines, emit event JSONL.
    Rpc,
}

fn today() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    )
}

fn discover_context_files(workdir: &Path) -> Vec<(String, String)> {
    let start = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut chain: Vec<PathBuf> = Vec::new();
    let mut dir = Some(start.as_path());
    while let Some(d) = dir {
        let candidate = d.join("AGENTS.md");
        if candidate.is_file() {
            chain.push(candidate);
        }
        if home.as_deref() == Some(d) {
            break;
        }
        dir = d.parent();
    }
    chain.reverse();
    let mut files = Vec::new();
    for path in chain {
        if let Ok(content) = std::fs::read_to_string(&path) {
            files.push((path.to_string_lossy().into_owned(), content));
        }
    }
    files
}

fn skill_dirs() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from(".yaca/skills")];
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(home).join(".config/yaca/skills"));
    }
    v
}

fn agent_with_model(model: &str) -> AgentSpec {
    let workdir = PathBuf::from(".");
    let env = PromptEnv {
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string()),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let mut context = discover_context_files(&workdir);
    let skills = skills::discover_skills(&skill_dirs());
    if let Some(section) = skills::skills_section(&skills) {
        context.push(("Available skills".to_string(), section));
    }
    let system_prompt = build_system_prompt("You are yaca, a coding agent.", &env, &context);
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new(model),
        system_prompt,
        workdir,
        reasoning: None,
    }
}

fn compaction_config() -> CompactionConfig {
    let default = CompactionConfig::default();
    let token_threshold = std::env::var("YACA_COMPACTION_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default.token_threshold);
    let keep_recent = std::env::var("YACA_COMPACTION_KEEP_RECENT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default.keep_recent);
    CompactionConfig {
        token_threshold,
        keep_recent,
    }
}

fn spawn_team_supervisor(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<SpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
) {
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            let engine = engine.clone();
            let base = base.clone();
            tokio::spawn(async move {
                let parent = req.parent;
                let members = req.members;
                let cancel = req.cancel;
                let background = req.background;
                let mut reply = Some(req.reply);
                let specs: Vec<MemberSpec> = if background {
                    let mut specs = Vec::new();
                    let mut started = Vec::new();
                    for member in members {
                        let id = MemberId::new();
                        let session = match member
                            .task_id
                            .as_deref()
                            .and_then(|task_id| task_id.parse::<SessionId>().ok())
                        {
                            Some(session) => session,
                            None => {
                                match engine
                                    .create(CreateSession {
                                        parent: Some(parent),
                                        agent: base.name.clone(),
                                        model: base.model.clone(),
                                        workdir: base.workdir.to_string_lossy().into_owned(),
                                    })
                                    .await
                                {
                                    Ok(session) => session,
                                    Err(err) => {
                                        started.push(MemberOutcome {
                                            member: id.to_string(),
                                            session: "-".to_string(),
                                            status: "failed".to_string(),
                                            summary: err.to_string(),
                                        });
                                        continue;
                                    }
                                }
                            }
                        };
                        started.push(MemberOutcome {
                            member: id.to_string(),
                            session: session.to_string(),
                            status: "running".to_string(),
                            summary: "The task is working in the background.".to_string(),
                        });
                        specs.push(MemberSpec {
                            id,
                            agent: base.clone(),
                            directive: member.prompt,
                            session: Some(session),
                        });
                    }
                    if let Some(reply) = reply.take() {
                        let _ = reply.send(started);
                    }
                    specs
                } else {
                    members
                        .into_iter()
                        .map(|m| MemberSpec {
                            id: MemberId::new(),
                            agent: base.clone(),
                            directive: m.prompt,
                            session: m
                                .task_id
                                .as_deref()
                                .and_then(|task_id| task_id.parse::<SessionId>().ok()),
                        })
                        .collect()
                };
                let evidence = run_team(engine.clone(), parent, specs, cancel).await;
                let envelope = TeamEvidenceEnvelope {
                    members: evidence.clone(),
                };
                let _ = project_envelope(&engine, parent, &envelope).await;
                let outcomes: Vec<MemberOutcome> = evidence
                    .into_iter()
                    .map(|e| MemberOutcome {
                        member: e.member,
                        session: e.session,
                        status: match e.status {
                            MemberStatus::Done => "done".to_string(),
                            MemberStatus::Failed => "failed".to_string(),
                        },
                        summary: e.summary,
                    })
                    .collect();
                if !background && let Some(reply) = reply.take() {
                    let _ = reply.send(outcomes);
                }
            });
        }
    });
}

fn build_session_engine(
    store: SessionStore,
    router: ProviderRouter,
    model: &str,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
) -> (
    Arc<SessionEngine>,
    tokio::sync::mpsc::UnboundedReceiver<AskRequest>,
    tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>,
    McpManager,
    Arc<PluginHost>,
) {
    let router = Arc::new(router);
    let mut registry = ToolRegistry::builtins();
    let mcp_manager = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(McpManager::connect_all(mcp))
    });
    for tool in mcp_manager.tools() {
        if let Err(error) = registry.register(tool) {
            eprintln!("yaca: skipping MCP tool ({error})");
        }
    }
    let plugin_host = Arc::new(tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(PluginHost::connect_all(plugins, host_info()))
    }));
    for tool in plugin_host.tools() {
        if let Err(error) = registry.register(tool) {
            eprintln!("yaca: skipping plugin tool ({error})");
        }
    }
    let tools = Arc::new(registry);
    let rules = PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Glob, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]);
    let (permission, asks) = PermissionPlane::new(rules);
    let permission = if plugin_host.is_empty() {
        permission
    } else {
        permission.with_interceptor(Arc::new(PermissionBridge::new(plugin_host.clone())))
    };
    let (interaction, questions) = InteractionPlane::new();
    let (spawner, spawn_rx) = SpawnerPlane::new();
    let summarizer: Arc<dyn Summarizer> =
        Arc::new(ModelSummarizer::new(router.clone(), ModelRef::new(model)));
    let mut engine_builder =
        SessionEngine::new(store, router, tools, permission, EventBus::default())
            .with_compaction(summarizer, compaction_config())
            .with_interaction(interaction)
            .with_spawner(spawner);
    if !plugin_host.is_empty() {
        engine_builder = engine_builder.with_hooks(plugin_host.clone());
    }
    let engine = Arc::new(engine_builder);
    spawn_team_supervisor(spawn_rx, engine.clone(), agent_with_model(model));
    (engine, asks, questions, mcp_manager, plugin_host)
}

fn host_info() -> HostInfo {
    HostInfo {
        name: "yaca".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn headless_policy(yolo: bool, workdir: &std::path::Path) -> PermissionPolicy {
    if yolo {
        PermissionPolicy::Yolo
    } else {
        PermissionPolicy::Scoped {
            workdir: workdir.to_path_buf(),
        }
    }
}

fn offline_router(model_override: Option<String>) -> (ProviderRouter, String) {
    let router = ProviderRouter::new().with(Arc::new(DevProvider::new()));
    (
        router,
        model_override.unwrap_or_else(|| "offline".to_string()),
    )
}

struct RuntimeConfig {
    router: ProviderRouter,
    model: String,
    models: Vec<config::ModelEntry>,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
}

/// Resolve a provider router + active model from yaca's config, falling back
/// to the offline echo provider when no usable config is present.
fn resolve_runtime(model_override: Option<String>) -> RuntimeConfig {
    match config::load() {
        Ok(Some(cfg)) => {
            let (fallback_router, fallback_model) = offline_router(model_override.clone());
            let default_model = if cfg.default_model.is_empty() {
                fallback_model
            } else {
                cfg.default_model
            };
            let model = model_override
                .or_else(|| std::env::var("YACA_MODEL").ok())
                .unwrap_or(default_model);
            RuntimeConfig {
                router: if cfg.has_providers {
                    cfg.router
                } else {
                    fallback_router
                },
                model,
                models: cfg.models,
                mcp: cfg.mcp,
                plugins: plugins::resolve(cfg.plugins, plugins::plugins_dir().as_deref()),
            }
        }
        Ok(None) => {
            let (router, model) = offline_router(model_override);
            RuntimeConfig {
                router,
                model,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
            }
        }
        Err(e) => {
            eprintln!("yaca: config error ({e:#}); using the offline provider");
            let (router, model) = offline_router(model_override);
            RuntimeConfig {
                router,
                model,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
            }
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

async fn cmd_exec(
    prompt: String,
    model_override: Option<String>,
    yolo: bool,
    json: bool,
) -> anyhow::Result<()> {
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
    );
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
    );
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
    );
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
            "yaca {} — a multi-agent coding agent",
            env!("CARGO_PKG_VERSION")
        );
        println!(
            "The interactive TUI needs a terminal. Try `yaca exec \"<prompt>\"`, \
             `yaca -p \"<goal>\"`, or `yaca --help`."
        );
        return Ok(());
    }
    let store = open_store(&db).await?;
    let runtime = resolve_runtime(model_override);
    let (engine, asks, questions, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        runtime.router,
        &runtime.model,
        runtime.mcp,
        runtime.plugins,
    );
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
        runtime.model,
        runtime.models,
        asks,
        questions,
        session,
        yolo,
    )
    .await
}

async fn cmd_serve(
    bind: String,
    db: String,
    model_override: Option<String>,
    yolo: bool,
) -> anyhow::Result<()> {
    let store = open_store(&db).await?;
    let runtime = resolve_runtime(model_override);
    let (engine, asks, _, _mcp_manager, _plugin_host) = build_session_engine(
        store,
        runtime.router,
        &runtime.model,
        runtime.mcp,
        runtime.plugins,
    );
    let policy = if yolo {
        eprintln!("yaca: --yolo on serve auto-approves ALL tool actions for any client (RCE risk)");
        PermissionPolicy::Yolo
    } else {
        PermissionPolicy::ReadOnly
    };
    let _responder = spawn_auto_responder(asks, policy);
    let state = AppState {
        engine,
        agent: Arc::new(agent_with_model(&runtime.model)),
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
    let (router, model) = offline_router(None);
    let (engine, _asks, _, _mcp_manager, _plugin_host) =
        build_session_engine(store, router, &model, BTreeMap::new(), Vec::new());
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

async fn cmd_login(provider: String, token: String) -> anyhow::Result<()> {
    auth::save_token(&provider, &token).with_context(|| format!("save token for {provider}"))?;
    println!("Saved auth token for provider '{provider}'.");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let model = cli.model.clone();
    let yolo = cli.yolo;
    let db = cli.db.clone();
    let resume = cli.resume.clone();
    if let Some(goal) = cli.prompt {
        return cmd_goal(goal, cli.max_iterations, model, yolo).await;
    }
    match cli.command {
        None => cmd_tui(model, db, resume, yolo).await,
        Some(Command::Exec { prompt, json }) => cmd_exec(prompt, model, yolo, json).await,
        Some(Command::Serve { bind, db }) => cmd_serve(bind, db, model, yolo).await,
        Some(Command::TailSession { id, db }) => cmd_tail_session(id, db).await,
        Some(Command::Login { provider, token }) => cmd_login(provider, token).await,
        Some(Command::Sessions { db }) => cmd_sessions(db).await,
        Some(Command::Rpc) => cmd_rpc(model, yolo).await,
    }
}
