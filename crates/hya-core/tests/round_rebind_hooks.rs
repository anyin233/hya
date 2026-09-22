//! Root round rebinding refreshes the captured hook chain with the runtime generation.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use hya_bundle::{BundleCatalog, BundleSource, SourceFile, prepare_package};
use hya_core::{
    AgentCatalog, AgentSpec, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CoreError, CreateSession, EventBus, HookDispatcher,
    MessageUserBeforeInput, MessageUserBeforeOutcome, RuntimeCatalogRefresh, RuntimeRegistry,
    RuntimeSource, RuntimeSourceId, SessionEngine, TextCompleteInput, TextCompleteOutcome,
    ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome,
};
use hya_proto::{AgentName, Envelope, FinishReason, MessageId, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct TemperatureHook(f32);

#[async_trait]
impl HookDispatcher for TemperatureHook {
    fn dispatch_event(&self, _envelope: &Envelope) {}
    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }
    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: input.text }
    }
    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        MessageUserBeforeOutcome::Continue { text: input.text }
    }
    async fn chat_params(&self, mut input: ChatParamsInput) -> ChatParamsOutcome {
        input.request.temperature = Some(self.0);
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }
    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }
    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

struct TwoRounds {
    calls: AtomicUsize,
    temperatures: Mutex<Vec<Option<f32>>>,
}

#[async_trait]
impl Provider for TwoRounds {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }
    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.temperatures.lock().unwrap().push(request.temperature);
        let steps = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                FakeStep::ToolCall {
                    name: "marker".into(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        } else {
            vec![FakeStep::Finish(FinishReason::Stop)]
        };
        Ok(Box::pin(stream::iter(
            FakeProvider::materialize(&steps, session, message)
                .into_iter()
                .map(Ok),
        )))
    }
}

struct ReplaceHook {
    calls: AtomicUsize,
}

#[async_trait]
impl RuntimeCatalogRefresh for ReplaceHook {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) != 1 {
            return Ok(false);
        }
        runtime.refresh(|candidate| candidate.upsert_sources(vec![hook_source(0.9, 2)]))?;
        Ok(true)
    }
}

fn hook_source(temperature: f32, digest: u8) -> RuntimeSource {
    RuntimeSource::new(
        RuntimeSourceId::bundle("acme/hot-hook"),
        [digest; 32],
        Arc::new(()),
        Vec::new(),
    )
    .with_hooks(Arc::new(TemperatureHook(temperature)))
}

#[tokio::test]
async fn root_second_round_adopts_new_hook_when_first_round_had_none() {
    let prepared = prepare_package(BundleSource::new(
        "hook",
        vec![SourceFile::new(
            "bundle.yaml",
            "kind: Plugin\nidentity: { id: acme/hot-hook, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let bundles = BundleCatalog::from_verified_catalogs(&[&prepared]).unwrap();
    let catalog = Arc::new(AgentCatalog::new(Arc::new(bundles)).unwrap());
    let tools = ToolRegistry::builtins();
    tools.register(support::MarkerTool::new("marker")).unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(tools, catalog));
    let provider = Arc::new(TwoRounds {
        calls: AtomicUsize::new(0),
        temperatures: Mutex::new(Vec::new()),
    });
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        "*",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(provider.clone())),
        runtime,
        permission,
        EventBus::default(),
    );
    let workdir = support::TestDir::new("round-hook-refresh");
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("general"),
            model: ModelRef::new("fake"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "two rounds".into())
        .await
        .unwrap();
    let engine = engine.with_catalog_refresh(Arc::new(ReplaceHook {
        calls: AtomicUsize::new(0),
    }));
    let agent = AgentSpec {
        name: AgentName::new("general"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        *provider.temperatures.lock().unwrap(),
        vec![None, Some(0.9)]
    );
}
