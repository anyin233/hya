//! Runtime composition for headless CLI commands.

use anyhow::Context as _;
use hya_app::BuiltSessionEngine;
use hya_engine::{AgentSpec, CreateSession};
use hya_proto::SessionId;
use hya_store::SessionStore;

pub(crate) struct StartedEngine {
    pub(crate) built: BuiltSessionEngine,
    pub(crate) agent: AgentSpec,
    pub(crate) default_agent: Option<String>,
}

pub(crate) async fn start(
    store: SessionStore,
    model: Option<String>,
    yolo: bool,
    bare: bool,
    pure: bool,
) -> anyhow::Result<StartedEngine> {
    let exit_store = store.clone();
    if let Err(error) = crate::first_run_config_bootstrap(false) {
        exit_store
            .drain_and_report_exit(hya_store::EXIT_DRAIN_TIMEOUT)
            .await;
        return Err(error);
    }
    let runtime = hya_app::resolve_runtime(model).await;
    hya_app::startup_trace::mark("runtime_resolved", None);
    start_with(store, runtime, yolo, bare, pure).await
}

/// [`start`] on an already resolved runtime configuration.
async fn start_with(
    store: SessionStore,
    runtime: hya_app::RuntimeConfig,
    yolo: bool,
    bare: bool,
    pure: bool,
) -> anyhow::Result<StartedEngine> {
    let exit_store = store.clone();
    let runtime = runtime.with_yolo(yolo).with_pure(pure).with_bare(bare);
    let agent = hya_app::agent_for_runtime(&runtime, None);
    let tool_config = (runtime.permission, runtime.settings);
    let built_res = if pure {
        hya_app::build_session_engine_pure(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            tool_config,
        )
        .await
    } else {
        hya_app::build_session_engine(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            tool_config,
        )
        .await
    };
    let built = match built_res {
        Ok(built) => built,
        Err(error) => {
            exit_store
                .drain_and_report_exit(hya_store::EXIT_DRAIN_TIMEOUT)
                .await;
            return Err(error);
        }
    };
    Ok(StartedEngine {
        built,
        agent,
        default_agent: runtime.default_agent,
    })
}

/// A headless runtime whose permission asks and questions are rejected,
/// before or without a session.
pub(crate) struct HeadlessRuntime {
    pub(crate) built: BuiltSessionEngine,
    pub(crate) agent: AgentSpec,
    default_agent: Option<String>,
    explicit_model: bool,
    responder: RejectResponder,
    root_drivers: Vec<hya_engine::resident::RootDriverActivity>,
}

impl HeadlessRuntime {
    pub(crate) async fn start(
        store: SessionStore,
        model: Option<String>,
        yolo: bool,
        bare: bool,
        pure: bool,
    ) -> anyhow::Result<Self> {
        let explicit_model = model.is_some();
        Self::from_started(start(store, model, yolo, bare, pure).await?, explicit_model).await
    }

    /// A runtime on the offline provider that reads no `config.yaml`
    /// providers, MCP servers, or plugins and writes no config file.
    pub(crate) async fn start_offline(
        store: SessionStore,
        model: Option<String>,
        bare: bool,
        pure: bool,
    ) -> anyhow::Result<Self> {
        let explicit_model = model.is_some();
        let runtime = hya_app::offline_runtime(model, false);
        Self::from_started(
            start_with(store, runtime, false, bare, pure).await?,
            explicit_model,
        )
        .await
    }

    async fn from_started(started: StartedEngine, explicit_model: bool) -> anyhow::Result<Self> {
        let StartedEngine {
            mut built,
            agent,
            default_agent,
        } = started;
        let responder = match built.take_asks().context("asks receiver missing") {
            Ok(asks) => RejectResponder(hya_app::permission::spawn_reject_responder(asks)),
            Err(error) => {
                shutdown(&mut built).await?;
                return Err(error);
            }
        };
        let _ = built.take_questions();
        Ok(Self {
            built,
            agent,
            default_agent,
            explicit_model,
            responder,
            root_drivers: Vec::new(),
        })
    }

    /// Check that `session` exists (`<command> Session … was not found`), or
    /// create a root session in the agent's directory, and recover its tree.
    pub(crate) async fn open_session(
        &mut self,
        session: Option<SessionId>,
        command: &str,
    ) -> anyhow::Result<SessionId> {
        let engine = self.built.engine();
        let session = if let Some(session) = session {
            let projection = engine.read_projection_shared(session).await?;
            anyhow::ensure!(
                projection.session.id.is_some(),
                "{command} Session `{session}` was not found"
            );
            session
        } else {
            let agent = &mut self.agent;
            let project = crate::ensure_project_for_workdir(engine.store(), &agent.workdir).await?;
            agent.name = hya_app::resolve_headless_agent_name(
                &engine,
                &agent.workdir,
                None,
                self.default_agent.as_deref(),
            )
            .await
            .context("resolve default agent")?;
            let model = if self.explicit_model {
                agent.model.clone()
            } else {
                self.built
                    .effective_root_model(agent, &agent.workdir)
                    .await?
            };
            engine
                .create(CreateSession {
                    parent: None,
                    agent: agent.name.clone(),
                    model,
                    workdir: agent.workdir.to_string_lossy().into_owned(),
                    project: Some(project),
                    kind: hya_proto::SessionKind::Project,
                })
                .await
                .context("create session")?
        };
        self.root_drivers = self
            .built
            .resident_supervisor()
            .start_recovered_headless(session)
            .await?;
        Ok(session)
    }

    /// Stop permission intake before awaiting every runtime child.
    pub(crate) async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.responder.0.abort();
        shutdown(&mut self.built).await
    }
}

pub(crate) struct HeadlessSession {
    pub(crate) built: BuiltSessionEngine,
    pub(crate) agent: AgentSpec,
    pub(crate) session: SessionId,
    responder: RejectResponder,
    _root_drivers: Vec<hya_engine::resident::RootDriverActivity>,
}

struct RejectResponder(tokio::task::JoinHandle<()>);

impl Drop for RejectResponder {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl HeadlessSession {
    pub(crate) async fn run<T>(
        mut self,
        command: impl std::ops::AsyncFnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let result = command(&mut self).await;
        let shutdown = self.shutdown().await;
        let output = result?;
        shutdown?;
        Ok(output)
    }

    /// Stop permission intake before awaiting every runtime child, on both
    /// successful and failed headless commands.
    pub(crate) async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.responder.0.abort();
        shutdown(&mut self.built).await
    }

    /// A [`HeadlessRuntime`] with [`HeadlessRuntime::open_session`]: a new
    /// root session, or the existing `session` reopened.
    pub(crate) async fn start(
        store: SessionStore,
        model: Option<String>,
        yolo: bool,
        bare: bool,
        pure: bool,
        session: Option<SessionId>,
    ) -> anyhow::Result<Self> {
        let mut runtime = HeadlessRuntime::start(store, model, yolo, bare, pure).await?;
        let session = match runtime.open_session(session, "headless").await {
            Ok(session) => session,
            Err(error) => {
                runtime.shutdown().await?;
                return Err(error);
            }
        };
        let HeadlessRuntime {
            built,
            agent,
            responder,
            root_drivers,
            ..
        } = runtime;
        Ok(Self {
            built,
            agent,
            session,
            responder,
            _root_drivers: root_drivers,
        })
    }
}

async fn shutdown(built: &mut BuiltSessionEngine) -> anyhow::Result<()> {
    built.shutdown().await.context("shutdown headless runtime")
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn headless_command_errors_still_stop_the_responder_and_runtime() -> anyhow::Result<()> {
        if std::env::var_os("HYA_TEST_SHUTDOWN_CHILD").is_none() {
            let root =
                std::env::temp_dir().join(format!("hya-headless-shutdown-{}", std::process::id()));
            std::fs::create_dir_all(&root)?;
            let output = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "startup::tests::headless_command_errors_still_stop_the_responder_and_runtime",
                    "--nocapture",
                ])
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", &root)
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("HYA_CONFIG_HOME", root.join("hya"))
                .env("HYA_TEST_SHUTDOWN_CHILD", "1")
                .current_dir(&root)
                .output()?;
            std::fs::remove_dir_all(&root)?;
            anyhow::ensure!(
                output.status.success(),
                "shutdown child failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }
        let headless = super::HeadlessSession::start(
            hya_store::SessionStore::connect_memory().await?,
            Some("hya/offline".into()),
            false,
            false,
            true,
            None,
        )
        .await?;
        let responder = headless.responder.0.abort_handle();
        let engine = headless.built.engine();
        let error = headless
            .run(async |_| -> anyhow::Result<()> { anyhow::bail!("command failed") })
            .await;
        assert_eq!(
            error.err().map(|error| error.to_string()).as_deref(),
            Some("command failed")
        );
        assert!(
            responder.is_finished(),
            "responder must be stopped before cleanup returns"
        );
        assert!(
            engine.draining().is_some(),
            "runtime must have drained despite the command error"
        );
        Ok(())
    }

    #[tokio::test]
    async fn o1_headless_start_recovers_crashed_members_without_root_turns() -> anyhow::Result<()> {
        use hya_proto::{MailEndpoint, MailKind, Role, SessionId};
        let test = "startup::tests::o1_headless_start_recovers_crashed_members_without_root_turns";
        let mode = std::env::var("HYA_TEST_RECOVERY_CHILD").unwrap_or_default();
        if mode.is_empty() {
            let dir =
                std::env::temp_dir().join(format!("hya-headless-recovery-{}", SessionId::new()));
            std::fs::create_dir_all(&dir)?;
            let output = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", test, "--nocapture"])
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", &dir)
                .env("XDG_CONFIG_HOME", dir.join("config"))
                .env("HYA_CONFIG_HOME", dir.join("hya"))
                .env("HYA_TEST_RECOVERY_CHILD", "run")
                .current_dir(&dir)
                .output()?;
            std::fs::remove_dir_all(dir)?;
            anyhow::ensure!(
                output.status.success(),
                "headless recovery failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }
        if mode == "run" {
            let output = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", test, "--nocapture"])
                .env("HYA_TEST_RECOVERY_CHILD", "seed")
                .output()?;
            anyhow::ensure!(
                output.status.success(),
                "crash seed failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let store = hya_store::SessionStore::connect("events.db").await?;
        let session = if mode == "run" {
            Some(std::fs::read_to_string("root.id")?.parse::<SessionId>()?)
        } else {
            None
        };
        let headless = super::HeadlessSession::start(
            store,
            Some("hya/offline".into()),
            true,
            false,
            true,
            session,
        )
        .await?;
        let engine = headless.built.engine();
        if mode == "seed" {
            let supervisor = headless.built.resident_supervisor();
            let binding = engine.bind_runtime(&headless.agent.workdir)?;
            let agent = engine.agent_spec_for_binding(&binding, &headless.agent, "hya-scout")?;
            let resources = binding.agent_resource_policy("hya-scout")?;
            let (_, handle, _) = supervisor
                .spawn_resident_parked(
                    headless.session,
                    agent,
                    (binding, std::sync::Arc::from([]), resources, None),
                    "parked".into(),
                    None,
                    None,
                )
                .await?;
            for target in [handle, "main".into()] {
                engine
                    .store()
                    .append_event(
                        headless.session,
                        &hya_tool::extension::ExtensionEvent::MailSent {
                            session: headless.session,
                            from: "harness".into(),
                            to: MailEndpoint::Handle(target),
                            kind: MailKind::Message,
                            body: "RECOVER_HEADLESS_WORK".into(),
                        }
                        .into_event(),
                    )
                    .await?;
            }
            std::fs::write("root.id", headless.session.to_string())?;
            // Exit without Drop/shutdown, preserving the live durable claim.
            std::process::exit(0);
        }
        let root = headless.session;
        let supervisor = headless.built.resident_supervisor();
        headless
            .run(async |headless| {
                anyhow::ensure!(
                    supervisor.episode_count() == 1,
                    "headless startup left the recovered actor staged"
                );
                anyhow::ensure!(
                    supervisor.start_recovered().await? == 0,
                    "startup must install recovery exactly once"
                );
                let mut bus = engine.bus().subscribe();
                let projection = engine.read_projection(root).await?;
                let team = hya_tool::extension::team_projection(&projection);
                let child = team
                    .roster
                    .values()
                    .find(|entry| entry.session != root)
                    .ok_or_else(|| anyhow::anyhow!("recovered member missing"))?
                    .session;
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let p = engine.read_projection(child).await?;
                        if p.session.messages.iter().any(|message| {
                            message.role == Role::Assistant && message.finish.is_some()
                        }) {
                            break Ok::<_, anyhow::Error>(());
                        }
                        bus.recv().await?;
                    }
                })
                .await??;
                let projection = engine.read_projection(root).await?;
                anyhow::ensure!(
                    projection.session.messages.is_empty(),
                    "recovery started an autonomous root turn"
                );
                anyhow::ensure!(headless.session == root);
                Ok(())
            })
            .await
    }
}
