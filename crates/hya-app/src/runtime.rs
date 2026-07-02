// allow: SIZE_OK — reviewed Phase 1 keeps backend bootstrap glue in this public API module.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use hya_core::{
    AgentSpec, CompactionConfig, CreateSession, EventBus, MemberSpec, MemberStatus,
    ModelSummarizer, PromptEnv, SessionEngine, SubagentGovernor, Summarizer, TeamEvidenceEnvelope,
    build_system_prompt, project_envelope, run_team,
};
use hya_mcp::McpServerConfig;
use hya_plugin::HostInfo;
use hya_plugin::config::PluginSpec;
use hya_proto::{AgentName, MemberId, ModelRef, SessionId};
use hya_provider::{DevProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{
    Action, AskRequest, InteractionPlane, MemberOutcome, Mode, PermissionPlane, PermissionRules,
    QuestionRequest, Rule, SpawnRequest, SpawnerPlane, ToolRegistry,
};
use std::collections::BTreeMap;

use crate::config;
use crate::permission::{PermissionPolicy, spawn_auto_responder};
use crate::{formatter_config, plugins};

pub fn today() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    )
}

pub fn discover_context_files(workdir: &Path) -> Vec<(String, String)> {
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

pub fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

pub fn headless_policy(yolo: bool, workdir: &std::path::Path) -> PermissionPolicy {
    if yolo {
        PermissionPolicy::Yolo
    } else {
        PermissionPolicy::Scoped {
            workdir: workdir.to_path_buf(),
        }
    }
}

pub fn offline_router(model_override: Option<String>) -> (ProviderRouter, String) {
    let router = ProviderRouter::new().with(Arc::new(DevProvider::new()));
    (
        router,
        model_override.unwrap_or_else(|| "offline".to_string()),
    )
}

pub fn compaction_config() -> CompactionConfig {
    let default = CompactionConfig::default();
    let token_threshold = std::env::var("HYA_COMPACTION_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default.token_threshold);
    let keep_recent = std::env::var("HYA_COMPACTION_KEEP_RECENT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default.keep_recent);
    CompactionConfig {
        token_threshold,
        keep_recent,
    }
}

pub fn agent_with_model(model: &str) -> AgentSpec {
    let workdir = PathBuf::from(".");
    let env = PromptEnv {
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string()),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let context = discover_context_files(&workdir);
    let system_prompt = build_system_prompt("You are hya, a coding agent.", &env, &context);
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new(model),
        system_prompt,
        workdir,
        reasoning: None,
    }
}

/// First-run guidance produced when no usable config is found and hya falls
/// back to the offline echo provider.
///
/// It is carried as *data* on [`RuntimeConfig`] rather than printed at the point
/// of resolution: that keeps it out of machine-readable surfaces (JSONL RPC,
/// `exec`/`-p` piping, `serve`), which never call [`OfflineNotice::emit`]. Only
/// interactive startup paths surface it, and always to stderr — never stdout.
pub struct OfflineNotice {
    /// Where a config file is expected (and should be created).
    pub config_path: PathBuf,
}

impl OfflineNotice {
    /// Render the multi-line guidance: what happened, that hya is offline, and
    /// how to connect a real model.
    #[must_use]
    pub fn render(&self) -> String {
        let path = self.config_path.display();
        format!(
            "hya: no usable provider config found at {path}\n\
             hya: running in OFFLINE mode — the built-in provider only echoes input, \
             so models cannot reason or use tools.\n\
             hya: to connect a real model, edit {path} (see docs/configuration.md)\n\
             hya:   and/or save a provider token with `hya login <provider> <token>`."
        )
    }

    /// Print the notice to stderr so it never corrupts machine-readable stdout.
    pub fn emit(&self) {
        eprintln!("{}", self.render());
    }
}

pub struct RuntimeConfig {
    pub router: ProviderRouter,
    pub model: String,
    pub models: Vec<config::ModelEntry>,
    pub mcp: BTreeMap<String, McpServerConfig>,
    pub plugins: Vec<PluginSpec>,
    pub default_agent: Option<String>,
    /// Set when no usable config was found and the offline provider was chosen.
    /// Interactive startup emits it; headless/machine-readable modes ignore it.
    pub offline_notice: Option<OfflineNotice>,
}

/// Resolve a provider router + active model from hya's config, falling back
/// to the offline echo provider when no usable config is present.
pub fn resolve_runtime(model_override: Option<String>) -> RuntimeConfig {
    match config::load() {
        Ok(Some(cfg)) => {
            let (fallback_router, fallback_model) = offline_router(model_override.clone());
            let default_model = if cfg.default_model.is_empty() {
                fallback_model
            } else {
                cfg.default_model
            };

            let model = model_override
                .or_else(|| std::env::var("HYA_MODEL").ok())
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
                default_agent: cfg.default_agent,
                offline_notice: None,
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
                default_agent: None,
                offline_notice: Some(OfflineNotice {
                    config_path: config::expected_config_path(),
                }),
            }
        }
        Err(e) => {
            eprintln!("hya: config error ({e:#}); using the offline provider");
            let (router, model) = offline_router(model_override);
            RuntimeConfig {
                router,
                model,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
                default_agent: None,
                offline_notice: None,
            }
        }
    }
}

pub async fn open_store(db: &str) -> anyhow::Result<SessionStore> {
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

pub fn spawn_team_supervisor(
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

pub async fn build_session_engine(
    store: SessionStore,
    router: ProviderRouter,
    model: &str,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
) -> (
    Arc<SessionEngine>,
    tokio::sync::mpsc::UnboundedReceiver<AskRequest>,
    tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>,
    hya_mcp::McpManager,
    Arc<hya_plugin::PluginHost>,
) {
    let router = Arc::new(router);
    let mut registry = ToolRegistry::builtins();
    let mcp_manager = hya_mcp::McpManager::connect_all(mcp).await;
    for tool in mcp_manager.tools() {
        if let Err(error) = registry.register(tool) {
            eprintln!("hya: skipping MCP tool ({error})");
        }
    }
    let plugin_host = Arc::new(hya_plugin::PluginHost::connect_all(plugins, host_info()).await);
    for tool in plugin_host.tools() {
        if let Err(error) = registry.register(tool) {
            eprintln!("hya: skipping plugin tool ({error})");
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
        permission.with_interceptor(Arc::new(hya_plugin::PermissionBridge::new(
            plugin_host.clone(),
        )))
    };
    let (interaction, questions) = InteractionPlane::new();
    let (spawner, spawn_rx) = SpawnerPlane::new();
    let summarizer: Arc<dyn Summarizer> =
        Arc::new(ModelSummarizer::new(router.clone(), ModelRef::new(model)));
    let bus = EventBus::new(crate::config::resolve_event_bus_capacity());
    let governor = SubagentGovernor::new(crate::config::load_subagent_limits());
    let mut engine_builder = SessionEngine::new(store, router, tools, permission, bus)
        .with_compaction(summarizer, compaction_config())
        .with_formatter(formatter_config::load_plane())
        .with_interaction(interaction)
        .with_spawner(spawner)
        .with_governor(governor);
    if !plugin_host.is_empty() {
        engine_builder = engine_builder.with_hooks(plugin_host.clone());
    }
    let engine = Arc::new(engine_builder);
    spawn_team_supervisor(spawn_rx, engine.clone(), agent_with_model(model));
    (engine, asks, questions, mcp_manager, plugin_host)
}

pub struct RuntimeOptions {
    pub model: Option<String>,
    pub db: String,
    pub yolo: bool,
    pub default_agent: Option<String>,
    pub include_global_agents: bool,
    pub force_offline: bool,
}

pub struct HyaRuntime {
    router: axum::Router,
    engine: Arc<SessionEngine>,
    app_state: hya_server::AppState,
    _permission_responder: Option<tokio::task::JoinHandle<()>>,
    _plugin_host: Arc<hya_plugin::PluginHost>,
}

impl HyaRuntime {
    pub async fn start(opts: RuntimeOptions) -> anyhow::Result<Self> {
        let store = open_store(&opts.db).await?;
        let runtime = if opts.force_offline {
            let (router, model) = offline_router(opts.model);
            RuntimeConfig {
                router,
                model,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
                default_agent: opts.default_agent,
                offline_notice: None,
            }
        } else {
            let mut runtime = resolve_runtime(opts.model);
            if opts.default_agent.is_some() {
                runtime.default_agent = opts.default_agent;
            }
            runtime
        };
        let (engine, asks, questions, mcp_manager, plugin_host) = build_session_engine(
            store,
            runtime.router,
            &runtime.model,
            runtime.mcp,
            runtime.plugins,
        )
        .await;
        let mut state =
            hya_server::AppState::new(engine.clone(), Arc::new(agent_with_model(&runtime.model)))
                .with_question_requests(questions)
                .with_mcp_manager(mcp_manager)
                .with_workspace_adapters(plugin_host.workspace_adapters())
                .with_default_agent(runtime.default_agent.clone())
                .with_global_agents(opts.include_global_agents);
        let _permission_responder = if opts.yolo {
            eprintln!("hya: --yolo auto-approves ALL tool actions for the hya frontend (RCE risk)");
            Some(spawn_auto_responder(asks, PermissionPolicy::Yolo))
        } else {
            state = state.with_permission_requests(asks);
            None
        };
        let app_state = state.clone();
        let router = hya_server::router(state);
        Ok(Self {
            router,
            engine,
            app_state,
            _permission_responder,
            _plugin_host: plugin_host,
        })
    }

    pub fn router(&self) -> &axum::Router {
        &self.router
    }

    #[must_use]
    pub fn engine(&self) -> Arc<SessionEngine> {
        self.engine.clone()
    }

    #[must_use]
    pub fn app_state(&self) -> hya_server::AppState {
        self.app_state.clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        home: Option<std::ffi::OsString>,
        xdg_config_home: Option<std::ffi::OsString>,
        current_dir: PathBuf,
    }

    impl EnvGuard {
        fn set(home: &Path, cwd: &Path) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let guard = Self {
                _lock: lock,
                home: std::env::var_os("HOME"),
                xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
                current_dir: std::env::current_dir().unwrap(),
            };
            std::fs::create_dir_all(home).unwrap();
            std::fs::create_dir_all(cwd).unwrap();
            unsafe {
                std::env::set_var("HOME", home);
                std::env::set_var("XDG_CONFIG_HOME", home);
            }
            std::env::set_current_dir(cwd).unwrap();
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.current_dir);
            unsafe {
                if let Some(home) = &self.home {
                    std::env::set_var("HOME", home);
                } else {
                    std::env::remove_var("HOME");
                }
                if let Some(xdg_config_home) = &self.xdg_config_home {
                    std::env::set_var("XDG_CONFIG_HOME", xdg_config_home);
                } else {
                    std::env::remove_var("XDG_CONFIG_HOME");
                }
            }
        }
    }

    fn tempdir() -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let serial = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hya-app-runtime-test-{nanos}-{serial}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn offline_notice_names_path_offline_mode_and_the_fix() {
        let notice = OfflineNotice {
            config_path: PathBuf::from("/home/u/.config/hya/config.yaml"),
        };
        let text = notice.render();
        // (a) where the missing config is expected,
        assert!(text.contains("/home/u/.config/hya/config.yaml"));
        // (b) that we are in offline/echo mode,
        assert!(text.contains("OFFLINE"));
        assert!(text.contains("echoes"));
        // (c) how to fix it.
        assert!(text.contains("hya login"));
        assert!(text.contains("docs/configuration.md"));
    }

    #[test]
    fn resolve_runtime_without_config_carries_but_does_not_print_the_notice() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        let _ = std::fs::remove_file(&config_path);

        let runtime = resolve_runtime(None);

        // Offline fallback selected: the built-in echo provider + "offline" model.
        assert_eq!(runtime.model, "offline");
        // The guidance is returned as DATA — resolve_runtime itself prints
        // nothing, so headless/RPC/serve callers (which never call `emit`) keep
        // a clean machine-readable stdout. Only interactive startup emits it.
        let notice = runtime
            .offline_notice
            .expect("missing-config path must carry an offline notice");
        assert!(notice.config_path.ends_with("hya/config.yaml"));
        assert!(notice.render().contains("OFFLINE"));
    }

    #[test]
    fn agent_with_model_omits_process_cwd_skill_index() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        write_skill(
            &workdir.join(".hya/skills/baseline"),
            "baseline-skill",
            "Baseline skill",
            "baseline body",
        );

        let agent = agent_with_model("fake");

        assert!(!agent.system_prompt.contains("Available skills"));
        assert!(
            !agent
                .system_prompt
                .contains("These skills are available on demand")
        );
        assert!(!agent.system_prompt.contains("baseline-skill"));
        assert!(!agent.system_prompt.contains("Baseline skill"));
    }
}
