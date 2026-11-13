//! The CLI's single engine composition path, shared by serve and headless runs.

use anyhow::Context as _;
use hya_app::{BuiltSessionEngine, config};
use hya_core::{AgentSpec, CreateSession};
use hya_proto::SessionId;
use hya_provider::ProviderRouter;
use hya_store::SessionStore;

pub(crate) enum Surface {
    Server,
    Headless,
}

pub(crate) struct StartedEngine {
    pub(crate) built: BuiltSessionEngine,
    pub(crate) agent: AgentSpec,
    pub(crate) default_agent: Option<String>,
    pub(crate) pending_discovery: Vec<config::PendingCatalogDiscovery>,
    pub(crate) evaluator_router: ProviderRouter,
    pub(crate) evaluator_model: String,
}

pub(crate) async fn start(
    store: SessionStore,
    model: Option<String>,
    yolo: bool,
    pure: bool,
    surface: Surface,
) -> anyhow::Result<StartedEngine> {
    crate::first_run_config_bootstrap(false)?;
    let runtime = hya_app::resolve_runtime(model)
        .await
        .with_yolo(yolo)
        .with_pure(pure);
    hya_app::startup_trace::mark("runtime_resolved", None);
    let agent = match surface {
        Surface::Server => hya_app::agent_base_with_model(&runtime.model, None),
        Surface::Headless if pure => hya_app::agent_with_model_pure(&runtime.model, None),
        Surface::Headless => hya_app::agent_with_model(&runtime.model, None),
    };
    let evaluator_router = runtime.router.clone();
    let evaluator_model = config::resolve_evaluator_model(
        None,
        runtime.settings.goal().evaluator_model.as_deref(),
        &runtime.model,
    )
    .to_string();
    let tool_config = (runtime.websearch, runtime.permission, runtime.settings);
    let built = if pure {
        hya_app::build_session_engine_pure(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            tool_config,
        )
        .await?
    } else {
        hya_app::build_session_engine(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            tool_config,
        )
        .await?
    };
    Ok(StartedEngine {
        built,
        agent,
        default_agent: runtime.default_agent,
        pending_discovery: runtime.pending_discovery,
        evaluator_router,
        evaluator_model,
    })
}

pub(crate) struct HeadlessSession {
    pub(crate) built: BuiltSessionEngine,
    pub(crate) agent: AgentSpec,
    pub(crate) session: SessionId,
    pub(crate) evaluator_router: ProviderRouter,
    pub(crate) evaluator_model: String,
    responder: RejectResponder,
    _root_drivers: Vec<hya_core::resident::RootDriverActivity>,
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
    async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.responder.0.abort();
        shutdown(&mut self.built).await
    }

    pub(crate) async fn start(
        store: SessionStore,
        model: Option<String>,
        yolo: bool,
        pure: bool,
        session: Option<SessionId>,
    ) -> anyhow::Result<Self> {
        let explicit_model = model.is_some();
        let StartedEngine {
            mut built,
            mut agent,
            default_agent,
            evaluator_router,
            evaluator_model,
            ..
        } = start(store, model, yolo, pure, Surface::Headless).await?;
        let prepared = async {
            let engine = built.engine();
            let asks = built.take_asks().context("asks receiver missing")?;
            let _ = built.take_questions();
            let responder = RejectResponder(hya_app::permission::spawn_reject_responder(asks));
            let session = if let Some(session) = session {
                let projection = engine.read_projection_shared(session).await?;
                anyhow::ensure!(
                    projection.session.id.is_some(),
                    "workflow Session `{session}` was not found"
                );
                session
            } else {
                let project =
                    crate::ensure_project_for_workdir(engine.store(), &agent.workdir).await?;
                agent.name = hya_app::resolve_headless_agent_name(
                    &engine,
                    &agent.workdir,
                    None,
                    default_agent.as_deref(),
                )
                .await
                .context("resolve default agent")?;
                let model = if explicit_model {
                    agent.model.clone()
                } else {
                    built.effective_root_model(&agent, &agent.workdir).await?
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
            let drivers = built
                .resident_supervisor()
                .start_recovered_headless(session)
                .await?;
            Ok::<_, anyhow::Error>((session, responder, drivers))
        }
        .await;
        let (session, responder, drivers) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                shutdown(&mut built).await?;
                return Err(error);
            }
        };
        Ok(Self {
            built,
            agent,
            session,
            evaluator_router,
            evaluator_model,
            responder,
            _root_drivers: drivers,
        })
    }
}

async fn shutdown(built: &mut BuiltSessionEngine) -> anyhow::Result<()> {
    built
        .shutdown()
        .await
        .context("shutdown headless runtime")?;
    let engine = built.engine();
    engine
        .store()
        .release_runtime_owner(engine.runtime_owner())?;
    Ok(())
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
        use hya_proto::{Event, MailEndpoint, MailKind, Role, SessionId};
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
        let headless =
            super::HeadlessSession::start(store, Some("hya/offline".into()), true, true, session)
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
                        &Event::MailSent {
                            session: headless.session,
                            from: "harness".into(),
                            to: MailEndpoint::Handle(target),
                            kind: MailKind::Message,
                            body: "RECOVER_HEADLESS_WORK".into(),
                        },
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
                let child = projection
                    .team
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
