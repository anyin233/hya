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

struct RecordingProvider {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "dummy"
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
    session: SessionId,
    seen: Arc<Mutex<Vec<String>>>,
}

impl DummyHarness {
    pub async fn new(models: Vec<&str>) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = RecordingProvider { seen: seen.clone() };
        let router = ProviderRouter::new().with(Arc::new(provider));
        let store = SessionStore::connect_memory().await.expect("memory store");
        let (engine, _asks) = crate::build_session_engine(store, router);
        let model = models.first().copied().unwrap_or("dummy");
        let agent = crate::agent_with_model(model);
        let session = engine
            .create(CreateSession {
                parent: None,
                agent: agent.name.clone(),
                model: agent.model.clone(),
                workdir: agent.workdir.to_string_lossy().into_owned(),
            })
            .await
            .expect("create harness session");
        let app = AppState {
            model: model.to_string(),
            session_label: session.to_string().chars().take(12).collect(),
            projection: engine.read_projection(session).await.expect("projection"),
            ..AppState::default()
        };
        let controller =
            Controller::with_models(app, models.into_iter().map(ToString::to_string).collect());
        Self {
            controller,
            engine,
            agent,
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

    async fn apply_effect(&mut self, effect: TuiEffect) {
        match effect {
            TuiEffect::None => {}
            TuiEffect::SelectModel(model) => {
                self.agent.model = yaca_proto::ModelRef::new(model);
            }
            TuiEffect::SelectAgent(agent) => {
                self.agent.name = yaca_proto::AgentName::new(agent);
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
                    self.agent.model = yaca_proto::ModelRef::new(model);
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
            TuiEffect::Exit
            | TuiEffect::Interrupt
            | TuiEffect::ResumeSession(_)
            | TuiEffect::CompactTranscript
            | TuiEffect::InitProject
            | TuiEffect::ExportTranscript
            | TuiEffect::NewSession => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[tokio::test]
    async fn dummy_harness_switches_model_and_returns_fixed_response() {
        let mut harness = DummyHarness::new(vec!["alpha", "beta"]).await;

        harness.type_text("/model");
        harness.press(key(KeyCode::Enter)).await;
        harness.press(key(KeyCode::Down)).await;
        harness.press(key(KeyCode::Enter)).await;
        harness.type_text("hello");
        harness.press(key(KeyCode::Enter)).await;

        assert_eq!(harness.seen_models(), vec!["beta".to_string()]);
        assert!(harness.transcript().contains("dummy response"));
    }
}
