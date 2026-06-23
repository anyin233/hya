#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::stream;
use tokio_util::sync::CancellationToken;
use yaca_core::completion::render_transcript;
use yaca_core::{AgentSpec, CreateSession, SessionEngine};
use yaca_proto::{Event, FinishReason, MessageId, SessionId};
use yaca_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use yaca_store::SessionStore;
use yaca_tui::AppState;

use super::controller::{Controller, TuiEffect};
use super::history::HistoryStore;

struct RecordingProvider {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "test"
    }

    fn capabilities(&self, _model: &yaca_proto::ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.seen
            .lock()
            .expect("seen models lock")
            .push(req.model.to_string());
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("dummy response".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

pub struct DummyHarness {
    controller: Controller,
    engine: Arc<SessionEngine>,
    agent: AgentSpec,
    history: HistoryStore,
    session: SessionId,
    seen: Arc<Mutex<Vec<String>>>,
}

impl DummyHarness {
    pub async fn new(models: Vec<&str>) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = RecordingProvider { seen: seen.clone() };
        let router = ProviderRouter::new().with(Arc::new(provider));
        let store = SessionStore::connect_memory().await.expect("memory store");
        let model = models.first().copied().unwrap_or("dummy");
        let (engine, _asks, _questions, _team_updates, _mcp_manager) =
            crate::build_session_engine(store, router, model, std::collections::BTreeMap::new())
                .await;
        let agent = crate::agent_with_model(model);
        let history = HistoryStore::new(
            std::env::temp_dir().join(format!("yaca-harness-{}", SessionId::new())),
        );
        let session = engine
            .create(CreateSession {
                parent: None,
                agent: agent.name.clone(),
                model: agent.model.clone(),
                workdir: agent.workdir.to_string_lossy().into_owned(),
            })
            .await
            .expect("create harness session");
        history
            .create_session(
                session,
                agent.model.as_str(),
                agent.name.as_str(),
                &agent.workdir.to_string_lossy(),
            )
            .expect("create harness history");
        let (model, model_provider_label) = super::model_identity::app_fields(model);
        let app = AppState {
            agent: agent.name.as_str().to_string(),
            model,
            model_provider_label,
            session_label: session.to_string().chars().take(12).collect(),
            projection: engine.read_projection(session).await.expect("projection"),
            workspace_workdir: Some(agent.workdir.to_string_lossy().into_owned()),
            ..AppState::default()
        };
        let controller =
            Controller::with_models(app, models.into_iter().map(ToString::to_string).collect());
        Self {
            controller,
            engine,
            agent,
            history,
            session,
            seen,
        }
    }

    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let effect = self
                .controller
                .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
            assert_eq!(effect, TuiEffect::None);
        }
    }

    pub async fn press(&mut self, key: KeyEvent) {
        let effect = self.controller.handle_key(key);
        self.apply_effect(effect).await;
    }

    pub fn seen_models(&self) -> Vec<String> {
        self.seen.lock().expect("seen models lock").clone()
    }

    pub fn transcript(&self) -> String {
        render_transcript(&self.controller.app.projection)
    }

    pub fn input(&self) -> &str {
        &self.controller.app.input
    }

    pub fn set_team(&mut self, team: Vec<(String, String)>) {
        self.controller.app.team = team;
    }

    pub fn team(&self) -> &[(String, String)] {
        &self.controller.app.team
    }

    async fn apply_effect(&mut self, effect: TuiEffect) {
        match effect {
            TuiEffect::None => {}
            TuiEffect::SelectModel { model, provider } => {
                self.agent.model = super::model_identity::selected_ref(&model, provider.as_deref());
            }
            TuiEffect::SelectAgent(agent) => {
                self.agent.name = yaca_proto::AgentName::new(agent);
            }
            TuiEffect::SelectReasoning(level) => {
                self.agent.reasoning = if matches!(level.as_str(), "off" | "none") {
                    None
                } else {
                    yaca_provider::ReasoningEffort::parse(&level)
                };
            }
            TuiEffect::SubmitConfigured {
                prompt,
                agent,
                model,
            } => {
                if let Some(agent) = agent {
                    self.agent.name = yaca_proto::AgentName::new(agent);
                }
                if let Some(model) = model {
                    self.agent.model =
                        super::model_identity::apply_ref(&mut self.controller.app, &model);
                }
                self.engine
                    .admit_user_prompt(self.session, prompt)
                    .await
                    .expect("admit prompt");
                self.engine
                    .run_turn(self.session, &self.agent, CancellationToken::new())
                    .await
                    .expect("run turn");
                self.controller.app.projection = self
                    .engine
                    .read_projection(self.session)
                    .await
                    .expect("read projection");
            }
            TuiEffect::Submit(prompt) => {
                self.engine
                    .admit_user_prompt(self.session, prompt)
                    .await
                    .expect("admit prompt");
                self.engine
                    .run_turn(self.session, &self.agent, CancellationToken::new())
                    .await
                    .expect("run turn");
                self.controller.app.projection = self
                    .engine
                    .read_projection(self.session)
                    .await
                    .expect("read projection");
            }
            TuiEffect::SystemMessage(message) => {
                self.engine
                    .inject_system_message(self.session, message)
                    .await
                    .expect("system message");
            }
            TuiEffect::CopyMessage(_) => {}
            TuiEffect::SelectedBlock(action) => {
                if let Some(forked) = super::session_fork::fork_selected_block(
                    &self.engine,
                    &self.history,
                    self.session,
                    &self.agent,
                    &self.controller.app.projection,
                    action,
                )
                .await
                .expect("fork selected block")
                {
                    self.session = forked.session;
                    super::team_updates::clear(&mut self.controller.app);
                    self.controller.app.projection = forked.projection;
                    self.controller.app.session_label =
                        self.session.to_string().chars().take(12).collect();
                    self.controller.app.input = forked.prompt_input;
                    self.controller.app.scroll_back = 0;
                    self.controller.app.running = false;
                    self.controller.app.selected_message = None;
                }
            }
            TuiEffect::NewSession => {
                let new_session = self
                    .engine
                    .create(CreateSession {
                        parent: None,
                        agent: self.agent.name.clone(),
                        model: self.agent.model.clone(),
                        workdir: self.agent.workdir.to_string_lossy().into_owned(),
                    })
                    .await
                    .expect("create new harness session");
                self.session = new_session;
                super::team_updates::clear(&mut self.controller.app);
                self.history
                    .create_session(
                        self.session,
                        self.agent.model.as_str(),
                        self.agent.name.as_str(),
                        &self.agent.workdir.to_string_lossy(),
                    )
                    .expect("create new harness history");
                self.controller.app.projection = self
                    .engine
                    .read_projection(self.session)
                    .await
                    .expect("read new projection");
                self.controller.app.session_label =
                    self.session.to_string().chars().take(12).collect();
                self.controller.app.input.clear();
                self.controller.app.scroll_back = 0;
                self.controller.app.running = false;
                self.controller.app.selected_message = None;
            }
            TuiEffect::Exit
            | TuiEffect::Interrupt
            | TuiEffect::ResumeSession(_)
            | TuiEffect::CompactTranscript
            | TuiEffect::InitProject
            | TuiEffect::ExportTranscript => {}
        }
    }
}

#[cfg(test)]
mod tests;
