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
    _responder: RejectResponder,
}

struct RejectResponder(tokio::task::JoinHandle<()>);

impl Drop for RejectResponder {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl HeadlessSession {
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
        let engine = built.engine();
        let asks = built.take_asks().context("asks receiver missing")?;
        let _ = built.take_questions();
        let responder = RejectResponder(hya_app::permission::spawn_reject_responder(asks));
        let session = if let Some(session) = session {
            let projection = engine.read_projection(session).await?;
            anyhow::ensure!(
                projection.session.id.is_some(),
                "workflow Session `{session}` was not found"
            );
            session
        } else {
            let project = crate::ensure_project_for_workdir(engine.store(), &agent.workdir).await?;
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
        Ok(Self {
            built,
            agent,
            session,
            evaluator_router,
            evaluator_model,
            _responder: responder,
        })
    }
}
