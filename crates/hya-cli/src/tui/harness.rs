#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::stream;
use hya_core::completion::render_transcript;
use hya_core::{AgentSpec, CreateSession, SessionEngine};
use hya_proto::{Event, FinishReason, MessageId, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_render_tui::AppState;
use hya_store::SessionStore;
use tokio_util::sync::CancellationToken;

use super::controller::{Controller, TuiEffect};

struct RecordingProvider {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "dummy"
    }

    fn capabilities(&self, _model: &hya_proto::ModelRef) -> Option<Capabilities> {
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
        let model = models.first().copied().unwrap_or("dummy");
        let (engine, _asks, _questions, _mcp_manager, _plugin_host) = crate::build_session_engine(
            store,
            router,
            model,
            std::collections::BTreeMap::new(),
            Vec::new(),
        )
        .await;
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
            TuiEffect::SelectModel(entry) => {
                let model = hya_proto::ModelRef::new(entry.model_ref());
                self.agent.model = model.clone();
                self.engine
                    .switch_model(self.session, model)
                    .await
                    .expect("switch model");
                self.controller.app.projection = self
                    .engine
                    .read_projection(self.session)
                    .await
                    .expect("read projection");
            }
            TuiEffect::SelectAgent(agent) => {
                let agent = hya_proto::AgentName::new(agent);
                self.agent.name = agent.clone();
                self.engine
                    .switch_agent(self.session, agent)
                    .await
                    .expect("switch agent");
                self.controller.app.projection = self
                    .engine
                    .read_projection(self.session)
                    .await
                    .expect("read projection");
            }
            TuiEffect::SelectReasoning(level) => {
                self.agent.reasoning = if matches!(level.as_str(), "off" | "none") {
                    None
                } else {
                    hya_provider::ReasoningEffort::parse(&level)
                };
            }
            TuiEffect::SubmitConfigured {
                prompt,
                agent,
                model,
                command,
            } => {
                if let Some(agent) = agent {
                    self.agent.name = hya_proto::AgentName::new(agent);
                }
                if let Some(model) = model {
                    self.agent.model = hya_proto::ModelRef::new(model);
                }
                self.admit_prompt(prompt, command).await;
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
            TuiEffect::SubmitCommand {
                prompt,
                command,
                arguments,
            } => {
                self.admit_prompt(prompt, Some((command, arguments))).await;
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
                self.admit_prompt(prompt, None).await;
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

    async fn admit_prompt(&self, prompt: String, command: Option<(String, String)>) {
        match command {
            Some((name, arguments)) => self
                .engine
                .admit_command_prompt(self.session, name, arguments, prompt)
                .await
                .expect("admit command prompt"),
            None => self
                .engine
                .admit_user_prompt(self.session, prompt)
                .await
                .expect("admit prompt"),
        };
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

        assert_eq!(harness.seen_models(), vec!["test/beta".to_string()]);
        assert!(harness.transcript().contains("dummy response"));
    }

    #[tokio::test]
    async fn dummy_harness_unknown_model_keeps_previous_model_for_prompt() {
        let mut harness = DummyHarness::new(vec!["alpha", "beta"]).await;

        harness.type_text("/model nope");
        harness.press(key(KeyCode::Enter)).await;
        harness.type_text("hello");
        harness.press(key(KeyCode::Enter)).await;

        assert_eq!(harness.seen_models(), vec!["alpha".to_string()]);
        assert!(harness.transcript().contains("unknown model 'nope'"));
        assert!(harness.transcript().contains("dummy response"));
    }
}
