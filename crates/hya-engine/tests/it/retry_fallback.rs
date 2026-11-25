//! Provider round retry, cross-model fallback chains, `model.fallback` hooks,
//! pre-send context window checks, and one-shot model probes.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream;
use hya_engine::kernel::{self, View, request::AttachmentData};
use hya_engine::{
    AgentSpec, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CoreError, EventBus, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome, ModelFailureClass, ModelFallbackInput, ModelFallbackOutcome,
    SessionEngine, TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, Message, MessageId, ModelRef, Part, PartId,
    PartProjection, Projection, Role, SessionId, ToolCallId, ToolName,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter, ReasoningEffort,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
enum RetryItem {
    Reasoning,
    EmptyReasoning,
    BilledReasoning,
    Text(&'static str),
    ToolInputStart,
    Tool,
    Error,
    Interrupted,
    Fatal,
}

struct ScriptedRetryProvider {
    scripts: Mutex<VecDeque<Vec<RetryItem>>>,
    opens: Arc<Mutex<usize>>,
}

impl ScriptedRetryProvider {
    fn new(scripts: Vec<Vec<RetryItem>>) -> (Self, Arc<Mutex<usize>>) {
        let opens = Arc::new(Mutex::new(0));
        (
            Self {
                scripts: Mutex::new(VecDeque::from(scripts)),
                opens: Arc::clone(&opens),
            },
            opens,
        )
    }
}

#[async_trait]
impl Provider for ScriptedRetryProvider {
    fn id(&self) -> &str {
        "scripted-retry"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        *self.opens.lock().unwrap() += 1;
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted");
        let mut out = Vec::new();
        for item in script {
            match item {
                RetryItem::EmptyReasoning => out.push(Ok(Event::ReasoningStart {
                    session,
                    message,
                    part: PartId::new(),
                    reason: None,
                })),
                RetryItem::BilledReasoning => out.extend(
                    FakeProvider::materialize(
                        &[
                            FakeStep::Usage(hya_proto::TokenUsage {
                                output: 2,
                                reasoning: 2,
                                ..Default::default()
                            }),
                            FakeStep::Finish(FinishReason::Stop),
                        ],
                        session,
                        message,
                    )
                    .into_iter()
                    .map(Ok),
                ),
                RetryItem::Reasoning => out.extend(
                    FakeProvider::materialize(
                        &[FakeStep::Reasoning("thinking".into())],
                        session,
                        message,
                    )
                    .into_iter()
                    .map(|mut event| {
                        if let Event::ReasoningEnd { provider_data, .. } = &mut event {
                            *provider_data = Some(json!({"id": "prior-step"}));
                        }
                        Ok(event)
                    }),
                ),
                RetryItem::Text(text) => out.extend(
                    FakeProvider::materialize(&[FakeStep::Text(text.into())], session, message)
                        .into_iter()
                        .map(Ok),
                ),
                RetryItem::ToolInputStart => out.push(Ok(Event::ToolInputStart {
                    session,
                    message,
                    part: PartId::new(),
                    call: ToolCallId::new(),
                    name: ToolName::new("bash"),
                })),
                RetryItem::Tool => out.push(Ok(Event::ToolCallRequested {
                    session,
                    message,
                    part: PartId::new(),
                    call: ToolCallId::new(),
                    name: ToolName::new("bash"),
                    input: json!({}),
                    provider_data: None,
                })),
                RetryItem::Interrupted => out.push(Err(ProviderError::StreamInterrupted(
                    "connection closed".into(),
                ))),
                RetryItem::Fatal => out.push(Err(ProviderError::Decode("invalid schema".into()))),
                RetryItem::Error => out.push(Err(ProviderError::Decode(
                    "error decoding response body".into(),
                ))),
            }
        }
        if !out
            .iter()
            .any(|e| matches!(e, Ok(Event::MessageFinished { .. })))
            && !out.iter().any(std::result::Result::is_err)
        {
            out.extend(
                FakeProvider::materialize(
                    &[FakeStep::Finish(FinishReason::Stop)],
                    session,
                    message,
                )
                .into_iter()
                .map(Ok),
            );
        }
        Ok(Box::pin(stream::iter(out)))
    }
}

async fn retry_fixture(
    scripts: Vec<Vec<RetryItem>>,
    zero_waits: bool,
) -> (Arc<SessionEngine>, AgentSpec, SessionId, Arc<Mutex<usize>>) {
    let (provider, opens) = ScriptedRetryProvider::new(scripts);
    let mut engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        PermissionPlane::new(PermissionRules::default()).0,
        EventBus::default(),
    );
    if zero_waits {
        engine = engine.with_round_retry_waits(&support::ZERO_RETRY_WAITS);
    }
    let engine = Arc::new(engine);
    let session =
        support::create_root_session(&engine, "build", "fake", std::path::Path::new("/tmp")).await;
    engine
        .admit_user_prompt(session, "go".into())
        .await
        .unwrap();
    let mut agent = support::agent_spec("build", "fake", "/tmp");
    agent.system_prompt = "x".into();
    (engine, agent, session, opens)
}

#[tokio::test]
async fn retries_reasoning_and_interrupted_body_before_text() -> Result<(), CoreError> {
    for (first, expected_deletes, check_steps) in [
        (vec![RetryItem::Reasoning, RetryItem::Error], 1, true),
        (vec![RetryItem::Interrupted], 0, false),
        (vec![RetryItem::Text(""), RetryItem::Interrupted], 1, false),
        (
            vec![RetryItem::ToolInputStart, RetryItem::Interrupted],
            1,
            false,
        ),
    ] {
        let (engine, agent, session, opens) =
            retry_fixture(vec![first, vec![RetryItem::Text("final answer")]], true).await;
        assert_eq!(
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await?,
            FinishReason::Stop
        );
        assert_eq!(*opens.lock().unwrap(), 2);
        let events = engine.replay(session).await?;
        let views: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.event {
                Event::StepStarted { view, .. } => Some(view.as_ref().expect("attempt view")),
                _ => None,
            })
            .collect();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0], views[1]);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, Event::PartDeleted { .. }))
                .count(),
            expected_deletes
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e.event,
                    Event::MessageFinished {
                        role: Role::Assistant,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(events.iter().any(
            |e| matches!(&e.event, Event::TextReplace { text, .. } if text == "final answer")
        ));
        if check_steps {
            let steps: Vec<(&str, u32, Option<FinishReason>)> = events
                .iter()
                .filter_map(|e| match &e.event {
                    Event::StepStarted { step, .. } => Some(("start", *step, None)),
                    Event::StepFinished { step, finish, .. } => {
                        Some(("finish", *step, Some(*finish)))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                steps,
                vec![
                    ("start", 0, None),
                    ("finish", 0, Some(FinishReason::Error)),
                    ("start", 1, None),
                    ("finish", 1, Some(FinishReason::Stop)),
                ]
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn full_mode_request_replays_to_recorded_view() {
    let provider = Arc::new(FakeProvider::scripted_turns(vec![vec![FakeStep::Text(
        "done".into(),
    )]]));
    let engine = support::engine_with_provider(provider.clone(), PermissionRules::default()).await;
    let mut agent = support::agent_spec("build", "fake", "/tmp");
    agent.system_prompt = "full replay system prompt".into();
    let session = support::create_root_session(&engine, "build", "fake", &agent.workdir).await;
    engine
        .admit_user_prompt(session, "go".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let log = engine.store().replay(session).await.unwrap();
    let (index, recorded) = log
        .iter()
        .enumerate()
        .find_map(|(i, e)| match &e.event {
            Event::StepStarted { view, .. } => Some((i, view.as_ref().expect("step view"))),
            _ => None,
        })
        .expect("one provider step");
    assert!(recorded.cut < log[index].seq);
    let end = log.partition_point(|e| e.seq <= recorded.cut);
    let projection = Projection::from_events(&log[..end]);
    let view = View {
        projection: &projection,
    };
    assert_eq!(view.cut(), recorded.cut);
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    let rebuilt = kernel::request::request(
        &view,
        sent.system.as_deref().expect("full-mode system prompt"),
        sent.tools.clone(),
        &agent.model,
        &agent.name,
        &AttachmentData::new(),
        None,
    );
    assert_eq!(
        kernel::request::request_sha256(&rebuilt).unwrap(),
        recorded.request_sha256
    );
    assert_eq!(
        kernel::request::request_sha256(sent).unwrap(),
        recorded.request_sha256
    );
}

#[tokio::test]
async fn does_not_retry_after_committed_text_or_tool_call() -> Result<(), CoreError> {
    let (engine, agent, session, opens) = retry_fixture(
        vec![
            vec![RetryItem::Text("partial"), RetryItem::Interrupted],
            vec![RetryItem::Text("second attempt")],
        ],
        true,
    )
    .await;
    let result = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await;
    assert!(
        matches!(result, Err(CoreError::Provider(ref err)) if matches!(err.as_ref(), ProviderError::StreamInterrupted(_)))
    );
    assert_eq!(*opens.lock().unwrap(), 1);
    assert_eq!(support::assistant_text(&engine, session).await, "partial");

    for error in [RetryItem::Interrupted, RetryItem::Error] {
        let (engine, agent, session, opens) =
            retry_fixture(vec![vec![RetryItem::Tool, error]], true).await;
        assert!(matches!(
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await,
            Err(CoreError::Provider(_))
        ));
        assert_eq!(*opens.lock().unwrap(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn only_reasoning_or_length_requests_empty_turn_rescue() -> Result<(), CoreError> {
    for (first, expected) in [
        (RetryItem::Reasoning, 2),
        (RetryItem::BilledReasoning, 2),
        (RetryItem::EmptyReasoning, 1),
        (RetryItem::Text(""), 1),
    ] {
        let reasoning = matches!(first, RetryItem::Reasoning);
        let (engine, agent, session, opens) =
            retry_fixture(vec![vec![first], vec![RetryItem::Text("rescued")]], true).await;
        assert_eq!(
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await?,
            FinishReason::Stop
        );
        assert_eq!(*opens.lock().unwrap(), expected);
        let projection = engine.store().read_projection(session).await?;
        let notices = projection
            .session
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter(|p| {
                matches!(p, PartProjection::Text { text, .. } if kernel::is_empty_turn_notice(text))
            })
            .count();
        assert_eq!(notices, expected - 1);
        if reasoning {
            let mut parts = projection.session.messages.iter().flat_map(|r| &r.parts);
            assert!(parts.clone().any(|part| matches!(
                part,
                PartProjection::Reasoning { text, provider_data: Some(data), .. }
                    if text == "thinking" && data == &json!({"id": "prior-step"})
            )));
            assert!(parts.any(|part| matches!(
                part,
                PartProjection::Text { text, .. }
                    if text.ends_with("Where your previous thinking left off:\nthinking")
            )));
            let messages = kernel::request::projection_to_messages(
                &agent.name,
                &projection,
                &agent.model,
                &AttachmentData::new(),
            );
            let notice_id = projection
                .session
                .messages
                .iter()
                .find(|row| {
                    row.parts.iter().any(|p| {
                        matches!(p, PartProjection::Text { text, .. } if kernel::is_empty_turn_notice(text))
                    })
                })
                .unwrap()
                .id;
            assert!(messages.iter().all(|m| m.id() != notice_id));
        }
    }
    Ok(())
}

#[tokio::test]
async fn terminal_failure_and_retry_exhaustion_close_once() -> Result<(), Box<dyn std::error::Error>>
{
    let attempts = kernel::ROUND_RETRY_WAITS.len() + 1;
    for (scripts, expected_opens) in [
        (
            vec![
                vec![RetryItem::Reasoning, RetryItem::Error],
                vec![RetryItem::Reasoning, RetryItem::Fatal],
            ],
            2,
        ),
        (
            vec![vec![RetryItem::Reasoning, RetryItem::Interrupted]; attempts],
            attempts,
        ),
    ] {
        let (engine, agent, session, opens) = retry_fixture(scripts, true).await;
        assert!(
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(*opens.lock().unwrap(), expected_opens);
        let events = engine.store().replay(session).await?;
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e.event,
                    Event::MessageFinished {
                        role: Role::Assistant,
                        finish: FinishReason::Error,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_during_backoff_is_prompt() -> Result<(), Box<dyn std::error::Error>> {
    // Uses the real default backoff schedule (5s) and cancels immediately once
    // the failed step's partial part is discarded, verifying prompt abort.
    let (engine, agent, session, opens) = retry_fixture(
        vec![
            vec![RetryItem::Reasoning, RetryItem::Error],
            vec![RetryItem::Reasoning, RetryItem::Error],
        ],
        false,
    )
    .await;
    let cancel = CancellationToken::new();
    let mut events = engine.bus().subscribe();
    let task = {
        let e = engine.clone();
        let c = cancel.clone();
        tokio::spawn(async move { e.run_turn(session, &agent, c).await })
    };
    while !matches!(events.recv().await?.event, Event::PartDeleted { .. }) {}
    let started = Instant::now();
    cancel.cancel();
    assert!(matches!(
        task.await?,
        Ok(FinishReason::Cancelled) | Err(CoreError::Cancelled)
    ));
    assert_eq!(*opens.lock().unwrap(), 1);
    assert!(started.elapsed() < Duration::from_secs(1));
    Ok(())
}

// ---------------------------------------------------------------------------
// Cross-model fallback chains, `model.fallback` hooks, and window preflight
// ---------------------------------------------------------------------------

enum FallbackOutcome {
    PreStreamFailure(ProviderError),
    MidStreamFailure(ProviderError),
    Complete(&'static str),
}

struct ScriptedModelProvider {
    claimed_model: &'static str,
    max_context: u32,
    outcomes: Mutex<VecDeque<FallbackOutcome>>,
    attempts: Arc<Mutex<Vec<String>>>,
    request_reasoning: Arc<Mutex<Vec<Option<ReasoningEffort>>>>,
}

impl ScriptedModelProvider {
    fn new(
        claimed_model: &'static str,
        outcomes: Vec<FallbackOutcome>,
        attempts: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            claimed_model,
            max_context: 200_000,
            outcomes: Mutex::new(outcomes.into()),
            attempts,
            request_reasoning: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Provider for ScriptedModelProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == self.claimed_model).then(|| Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: self.max_context,
            reasoning_request: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        assert_eq!(req.model.as_str(), self.claimed_model);
        self.attempts.lock().unwrap().push(req.model.to_string());
        self.request_reasoning.lock().unwrap().push(req.reasoning);
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                FallbackOutcome::PreStreamFailure(ProviderError::Transport(
                    "script exhausted".into(),
                ))
            });
        match outcome {
            FallbackOutcome::PreStreamFailure(error) => Err(error),
            FallbackOutcome::MidStreamFailure(error) => {
                Ok(Box::pin(stream::once(async move { Err(error) })))
            }
            FallbackOutcome::Complete(text) => {
                let events = FakeProvider::materialize(
                    &[
                        FakeStep::Text(text.into()),
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
    }
}

struct FallbackHook {
    answers: Mutex<VecDeque<ModelFallbackOutcome>>,
    endless_prefix: Option<&'static str>,
    expand_request: bool,
    consults: Arc<Mutex<Vec<ModelFallbackInput>>>,
}

impl FallbackHook {
    fn scripted(
        answers: Vec<ModelFallbackOutcome>,
    ) -> (Arc<Self>, Arc<Mutex<Vec<ModelFallbackInput>>>) {
        let consults = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                endless_prefix: None,
                expand_request: false,
                consults: Arc::clone(&consults),
            }),
            consults,
        )
    }
}

#[async_trait]
impl HookDispatcher for FallbackHook {
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

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        let mut request = input.request;
        if self.expand_request {
            request.system = Some("expanded instructions ".repeat(200_000));
        }
        ChatParamsOutcome::Continue { request }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }

    async fn model_fallback(&self, input: ModelFallbackInput) -> ModelFallbackOutcome {
        let attempt = input.attempt;
        self.consults.lock().unwrap().push(input);
        if let Some(answer) = self.answers.lock().unwrap().pop_front() {
            return answer;
        }
        match self.endless_prefix {
            Some(prefix) => ModelFallbackOutcome::Retry {
                model: ModelRef::new(format!("{prefix}-{attempt}")),
            },
            None => ModelFallbackOutcome::GiveUp,
        }
    }
}

fn chain(preferred: &str, rest: &[&str]) -> HashMap<ModelRef, Vec<ModelRef>> {
    let mut candidates = vec![ModelRef::new(preferred)];
    candidates.extend(rest.iter().map(|m| ModelRef::new(*m)));
    HashMap::from([(ModelRef::new(preferred), candidates)])
}

fn retry_model(model: &str) -> ModelFallbackOutcome {
    ModelFallbackOutcome::Retry {
        model: ModelRef::new(model),
    }
}

fn provider_err(error: &CoreError) -> &ProviderError {
    match error {
        CoreError::Provider(inner) => inner.as_ref(),
        other => panic!("expected provider error, got: {other:?}"),
    }
}

struct FallbackFixture {
    _dir: support::TestDir,
    engine: SessionEngine,
    session: SessionId,
    agent: AgentSpec,
}

async fn fallback_fixture(
    providers: Vec<ScriptedModelProvider>,
    fallbacks: Option<HashMap<ModelRef, Vec<ModelRef>>>,
    session_model: &str,
    hooks: Option<Arc<dyn HookDispatcher>>,
) -> FallbackFixture {
    let dir = support::TestDir::new("fallback");
    let router = providers
        .into_iter()
        .fold(ProviderRouter::new(), |r, p| r.with(Arc::new(p)));
    let mut engine = support::engine_with_router_and_runtime(
        Arc::new(router),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Allow)]),
    )
    .await;
    if let Some(fallbacks) = fallbacks {
        engine = engine.with_model_fallbacks(fallbacks);
    }
    if let Some(hooks) = hooks {
        engine = engine.with_hooks(hooks);
    }
    let session = support::create_root_session(&engine, "build", session_model, dir.path()).await;
    engine
        .admit_user_prompt(session, "say something".into())
        .await
        .unwrap();
    let mut agent = support::agent_spec("build", session_model, dir.path());
    agent.system_prompt = "you are build".into();
    FallbackFixture {
        _dir: dir,
        engine,
        session,
        agent,
    }
}

#[tokio::test]
async fn fallback_chain_advances_on_retryable_unknown_and_carries_reasoning_variants() {
    // 1. Transport failure advances to beta; next turn restarts at recovered alpha.
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let spare_attempts = Arc::new(Mutex::new(Vec::new()));
    let fx = fallback_fixture(
        vec![
            ScriptedModelProvider::new(
                "alpha",
                vec![
                    FallbackOutcome::PreStreamFailure(ProviderError::Transport("reset".into())),
                    FallbackOutcome::Complete("PRIMARY_RECOVERED"),
                ],
                attempts.clone(),
            ),
            ScriptedModelProvider::new(
                "beta",
                vec![FallbackOutcome::Complete("BETA_LANDED")],
                spare_attempts.clone(),
            ),
        ],
        Some(chain("alpha", &["beta"])),
        "alpha",
        None,
    )
    .await;
    assert_eq!(
        fx.engine
            .run_turn(fx.session, &fx.agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert!(
        support::assistant_text(&fx.engine, fx.session)
            .await
            .contains("BETA_LANDED")
    );
    assert_eq!(
        fx.engine
            .run_turn(fx.session, &fx.agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert!(
        support::assistant_text(&fx.engine, fx.session)
            .await
            .contains("PRIMARY_RECOVERED")
    );
    assert_eq!(attempts.lock().unwrap().len(), 2);
    assert_eq!(spare_attempts.lock().unwrap().len(), 1);

    // 2. UnknownModel advances to next routed candidate.
    let rescued_attempts = Arc::new(Mutex::new(Vec::new()));
    let fx = fallback_fixture(
        vec![ScriptedModelProvider::new(
            "rescued",
            vec![FallbackOutcome::Complete("RESCUED_TEXT")],
            rescued_attempts.clone(),
        )],
        Some(chain("ghost", &["rescued"])),
        "ghost",
        None,
    )
    .await;
    assert_eq!(
        fx.engine
            .run_turn(fx.session, &fx.agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert!(
        support::assistant_text(&fx.engine, fx.session)
            .await
            .contains("RESCUED_TEXT")
    );
    assert_eq!(rescued_attempts.lock().unwrap().len(), 1);

    // 3. 503 failure advances and carries each candidate's `#variant` reasoning effort.
    let pref_attempts = Arc::new(Mutex::new(Vec::new()));
    let fb_attempts = Arc::new(Mutex::new(Vec::new()));
    let preferred = ScriptedModelProvider::new(
        "preferred#low",
        vec![FallbackOutcome::PreStreamFailure(
            ProviderError::HttpStatus {
                status: 503,
                message: "unavailable".into(),
                retry_after: None,
            },
        )],
        pref_attempts.clone(),
    );
    let fallback = ScriptedModelProvider::new(
        "fallback#high",
        vec![FallbackOutcome::Complete("FALLBACK_TEXT")],
        fb_attempts.clone(),
    );
    let pref_reasoning = preferred.request_reasoning.clone();
    let fb_reasoning = fallback.request_reasoning.clone();
    let fx = fallback_fixture(
        vec![preferred, fallback],
        Some(chain("preferred#low", &["fallback#high"])),
        "preferred#low",
        None,
    )
    .await;
    assert_eq!(
        fx.engine
            .run_turn(fx.session, &fx.agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert!(
        support::assistant_text(&fx.engine, fx.session)
            .await
            .contains("FALLBACK_TEXT")
    );
    assert_eq!(*pref_attempts.lock().unwrap(), vec!["preferred#low"]);
    assert_eq!(*fb_attempts.lock().unwrap(), vec!["fallback#high"]);
    assert_eq!(
        *pref_reasoning.lock().unwrap(),
        vec![Some(ReasoningEffort::Low)]
    );
    assert_eq!(
        *fb_reasoning.lock().unwrap(),
        vec![Some(ReasoningEffort::High)]
    );
}

#[tokio::test]
async fn auth_post_stream_and_empty_chain_respect_failover_boundaries() {
    // 1. AuthExpired does not consume fallback chain.
    let locked_attempts = Arc::new(Mutex::new(Vec::new()));
    let spare_attempts = Arc::new(Mutex::new(Vec::new()));
    let fx = fallback_fixture(
        vec![
            ScriptedModelProvider::new(
                "locked",
                vec![FallbackOutcome::PreStreamFailure(
                    ProviderError::AuthExpired {
                        provider: "locked".into(),
                        hint: "re-login".into(),
                    },
                )],
                locked_attempts.clone(),
            ),
            ScriptedModelProvider::new(
                "spare",
                vec![FallbackOutcome::Complete("SPARE")],
                spare_attempts.clone(),
            ),
        ],
        Some(chain("locked", &["spare"])),
        "locked",
        None,
    )
    .await;
    let err = fx
        .engine
        .run_turn(fx.session, &fx.agent, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        provider_err(&err),
        ProviderError::AuthExpired { provider, .. } if provider == "locked"
    ));
    assert_eq!(locked_attempts.lock().unwrap().len(), 1);
    assert_eq!(spare_attempts.lock().unwrap().len(), 0);

    // 2. Mid-stream failure is never failed over (no-replay boundary).
    let drop_attempts = Arc::new(Mutex::new(Vec::new()));
    let spare_attempts = Arc::new(Mutex::new(Vec::new()));
    let fx = fallback_fixture(
        vec![
            ScriptedModelProvider::new(
                "dropzone",
                vec![FallbackOutcome::MidStreamFailure(ProviderError::Decode(
                    "sse frame cut".into(),
                ))],
                drop_attempts.clone(),
            ),
            ScriptedModelProvider::new(
                "spare",
                vec![FallbackOutcome::Complete("SPARE")],
                spare_attempts.clone(),
            ),
        ],
        Some(chain("dropzone", &["spare"])),
        "dropzone",
        None,
    )
    .await;
    let err = fx
        .engine
        .run_turn(fx.session, &fx.agent, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        provider_err(&err),
        ProviderError::Decode(reason) if reason == "sse frame cut"
    ));
    assert_eq!(drop_attempts.lock().unwrap().len(), 1);
    assert_eq!(spare_attempts.lock().unwrap().len(), 0);

    // 3. Unconfigured chain retries transient pre-stream error on the original model.
    let solo_attempts = Arc::new(Mutex::new(Vec::new()));
    let unused_attempts = Arc::new(Mutex::new(Vec::new()));
    let fx = fallback_fixture(
        vec![
            ScriptedModelProvider::new(
                "solo",
                vec![
                    FallbackOutcome::PreStreamFailure(ProviderError::Transport("down".into())),
                    FallbackOutcome::Complete("RECOVERED_ON_RETRY"),
                ],
                solo_attempts.clone(),
            ),
            ScriptedModelProvider::new(
                "unused",
                vec![FallbackOutcome::Complete("UNUSED")],
                unused_attempts.clone(),
            ),
        ],
        None,
        "solo",
        None,
    )
    .await;
    assert_eq!(
        fx.engine
            .run_turn(fx.session, &fx.agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert!(
        support::assistant_text(&fx.engine, fx.session)
            .await
            .contains("RECOVERED_ON_RETRY")
    );
    assert_eq!(solo_attempts.lock().unwrap().len(), 2);
    assert_eq!(unused_attempts.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn model_fallback_hook_runs_after_chain_and_enforces_guards() {
    // 1. Non-retryable pre-stream failure consults hook with full metadata.
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let (hook, consults) = FallbackHook::scripted(vec![retry_model("beta")]);
    let fx = fallback_fixture(
        vec![
            ScriptedModelProvider::new(
                "alpha",
                vec![FallbackOutcome::PreStreamFailure(
                    ProviderError::AuthExpired {
                        provider: "alpha".into(),
                        hint: "re-login".into(),
                    },
                )],
                attempts.clone(),
            ),
            ScriptedModelProvider::new(
                "beta",
                vec![FallbackOutcome::Complete("BETA_BY_HOOK")],
                attempts.clone(),
            ),
        ],
        None,
        "alpha",
        Some(hook),
    )
    .await;
    assert_eq!(
        fx.engine
            .run_turn(fx.session, &fx.agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert!(
        support::assistant_text(&fx.engine, fx.session)
            .await
            .contains("BETA_BY_HOOK")
    );
    assert_eq!(*attempts.lock().unwrap(), vec!["alpha", "beta"]);
    {
        let c = consults.lock().unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].session, fx.session);
        assert_eq!(c[0].root_session, fx.session);
        assert_eq!(c[0].agent, Some(AgentName::new("hya-task")));
        assert_eq!(c[0].model, ModelRef::new("alpha"));
        assert_eq!(c[0].error_class, ModelFailureClass::Auth);
        assert_eq!(c[0].attempt, 1);
        assert_eq!(c[0].tried, vec![ModelRef::new("alpha")]);
    }

    // 2. Hook consulted only after configured chain is exhausted.
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let (hook, consults) = FallbackHook::scripted(vec![retry_model("gamma")]);
    let fx = fallback_fixture(
        vec![
            ScriptedModelProvider::new(
                "alpha",
                vec![FallbackOutcome::PreStreamFailure(ProviderError::Transport(
                    "reset".into(),
                ))],
                attempts.clone(),
            ),
            ScriptedModelProvider::new(
                "beta",
                vec![FallbackOutcome::PreStreamFailure(
                    ProviderError::HttpStatus {
                        status: 529,
                        message: "overloaded".into(),
                        retry_after: None,
                    },
                )],
                attempts.clone(),
            ),
            ScriptedModelProvider::new(
                "gamma",
                vec![FallbackOutcome::Complete("GAMMA_BY_HOOK")],
                attempts.clone(),
            ),
        ],
        Some(chain("alpha", &["beta"])),
        "alpha",
        Some(hook),
    )
    .await;
    fx.engine
        .run_turn(fx.session, &fx.agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(*attempts.lock().unwrap(), vec!["alpha", "beta", "gamma"]);
    assert_eq!(consults.lock().unwrap().len(), 1);
    assert_eq!(
        consults.lock().unwrap()[0].tried,
        vec![ModelRef::new("alpha"), ModelRef::new("beta")]
    );

    // 3. GiveUp surfaces latest failure; already-tried model is refused; post-stream never consults.
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let (hook, consults) =
        FallbackHook::scripted(vec![retry_model("beta"), ModelFallbackOutcome::GiveUp]);
    let fx = fallback_fixture(
        vec![
            ScriptedModelProvider::new(
                "alpha",
                vec![FallbackOutcome::PreStreamFailure(ProviderError::Transport(
                    "reset".into(),
                ))],
                attempts.clone(),
            ),
            ScriptedModelProvider::new(
                "beta",
                vec![FallbackOutcome::PreStreamFailure(
                    ProviderError::HttpStatus {
                        status: 400,
                        message: "bad request".into(),
                        retry_after: None,
                    },
                )],
                attempts.clone(),
            ),
        ],
        None,
        "alpha",
        Some(hook),
    )
    .await;
    let err = fx
        .engine
        .run_turn(fx.session, &fx.agent, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        provider_err(&err),
        ProviderError::HttpStatus { status: 400, .. }
    ));
    assert_eq!(consults.lock().unwrap().len(), 2);

    let attempts = Arc::new(Mutex::new(Vec::new()));
    let (hook, consults) = FallbackHook::scripted(vec![retry_model("alpha"), retry_model("beta")]);
    let fx = fallback_fixture(
        vec![ScriptedModelProvider::new(
            "alpha",
            vec![FallbackOutcome::PreStreamFailure(
                ProviderError::Incompatible("wrong".into()),
            )],
            attempts.clone(),
        )],
        None,
        "alpha",
        Some(hook),
    )
    .await;
    assert!(matches!(
        provider_err(
            &fx.engine
                .run_turn(fx.session, &fx.agent, CancellationToken::new())
                .await
                .unwrap_err()
        ),
        ProviderError::Incompatible(_)
    ));
    assert_eq!(*attempts.lock().unwrap(), vec!["alpha"]);
    assert_eq!(consults.lock().unwrap().len(), 1);

    // 4. Capped at 8 attempts per round; mid-stream never consults hook.
    let consults = Arc::new(Mutex::new(Vec::new()));
    let endless = Arc::new(FallbackHook {
        answers: Mutex::new(VecDeque::new()),
        endless_prefix: Some("ghost"),
        expand_request: false,
        consults: Arc::clone(&consults),
    });
    let fx = fallback_fixture(
        vec![ScriptedModelProvider::new(
            "alpha",
            vec![FallbackOutcome::PreStreamFailure(ProviderError::Transport(
                "reset".into(),
            ))],
            Arc::new(Mutex::new(Vec::new())),
        )],
        None,
        "alpha",
        Some(endless),
    )
    .await;
    let err = fx
        .engine
        .run_turn(fx.session, &fx.agent, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(provider_err(&err), ProviderError::UnknownModel(_)));
    assert_eq!(consults.lock().unwrap().len(), 7);

    let (hook, consults) = FallbackHook::scripted(vec![retry_model("spare")]);
    let fx = fallback_fixture(
        vec![
            ScriptedModelProvider::new(
                "dropzone",
                vec![FallbackOutcome::MidStreamFailure(ProviderError::Decode(
                    "cut".into(),
                ))],
                Arc::new(Mutex::new(Vec::new())),
            ),
            ScriptedModelProvider::new(
                "spare",
                vec![FallbackOutcome::Complete("SPARE")],
                Arc::new(Mutex::new(Vec::new())),
            ),
        ],
        None,
        "dropzone",
        Some(hook),
    )
    .await;
    assert!(
        fx.engine
            .run_turn(fx.session, &fx.agent, CancellationToken::new())
            .await
            .is_err()
    );
    assert!(consults.lock().unwrap().is_empty());
}

#[test]
fn model_failure_classes_map_provider_errors() {
    let status = |status| ProviderError::HttpStatus {
        status,
        message: String::new(),
        retry_after: None,
    };
    for (error, class) in [
        (
            ProviderError::Transport(String::new()),
            ModelFailureClass::Retryable,
        ),
        (status(429), ModelFailureClass::Retryable),
        (status(503), ModelFailureClass::Retryable),
        (
            ProviderError::UnknownModel("x".into()),
            ModelFailureClass::UnknownModel,
        ),
        (
            ProviderError::AuthExpired {
                provider: String::new(),
                hint: String::new(),
            },
            ModelFailureClass::Auth,
        ),
        (status(401), ModelFailureClass::Auth),
        (status(403), ModelFailureClass::Auth),
        (status(400), ModelFailureClass::InvalidRequest),
        (status(404), ModelFailureClass::InvalidRequest),
        (
            ProviderError::Incompatible(String::new()),
            ModelFailureClass::InvalidRequest,
        ),
        (
            ProviderError::Decode(String::new()),
            ModelFailureClass::Other,
        ),
        (ProviderError::Http(String::new()), ModelFailureClass::Other),
        (status(302), ModelFailureClass::Other),
    ] {
        assert_eq!(ModelFailureClass::of(&error), class, "{error:?}");
    }
    assert_eq!(ModelFailureClass::UnknownModel.as_str(), "unknown_model");
    assert_eq!(
        ModelFailureClass::InvalidRequest.as_str(),
        "invalid_request"
    );
}

#[tokio::test]
async fn context_window_preflight_blocks_oversized_primary_fallback_and_hook_requests() {
    // 1. Primary over window.
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let mut tiny = ScriptedModelProvider::new(
        "tiny",
        vec![FallbackOutcome::Complete("NO")],
        attempts.clone(),
    );
    tiny.max_context = 1;
    let fx = fallback_fixture(vec![tiny], None, "tiny", None).await;
    let err = fx
        .engine
        .run_turn(fx.session, &fx.agent, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(attempts.lock().unwrap().is_empty());
    assert!(err.to_string().contains("context window"));

    // 2. Smaller fallback candidate checked before send.
    let pref_attempts = Arc::new(Mutex::new(Vec::new()));
    let fb_attempts = Arc::new(Mutex::new(Vec::new()));
    let preferred = ScriptedModelProvider::new(
        "large",
        vec![FallbackOutcome::PreStreamFailure(ProviderError::Transport(
            "offline".into(),
        ))],
        pref_attempts.clone(),
    );
    let mut small = ScriptedModelProvider::new(
        "small",
        vec![FallbackOutcome::Complete("NO")],
        fb_attempts.clone(),
    );
    small.max_context = 1;
    let fx = fallback_fixture(
        vec![preferred, small],
        Some(chain("large", &["small"])),
        "large",
        None,
    )
    .await;
    let err = fx
        .engine
        .run_turn(fx.session, &fx.agent, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(pref_attempts.lock().unwrap().len(), 1);
    assert!(fb_attempts.lock().unwrap().is_empty());
    assert!(err.to_string().contains("context window"));

    // 3. Hook-expanded request rejected before send.
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(FallbackHook {
        answers: Mutex::new(VecDeque::new()),
        endless_prefix: None,
        expand_request: true,
        consults: Arc::new(Mutex::new(Vec::new())),
    });
    let fx = fallback_fixture(
        vec![ScriptedModelProvider::new(
            "hooked",
            vec![FallbackOutcome::Complete("NO")],
            attempts.clone(),
        )],
        None,
        "hooked",
        Some(hook),
    )
    .await;
    let err = fx
        .engine
        .run_turn(fx.session, &fx.agent, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(attempts.lock().unwrap().is_empty());
    assert!(err.to_string().contains("context window"));
}

// ---------------------------------------------------------------------------
// One-shot model probe
// ---------------------------------------------------------------------------

struct ProbeProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    fail: bool,
}

#[async_trait]
impl Provider for ProbeProvider {
    fn id(&self) -> &str {
        "probe"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "probe/m").then(|| Capabilities {
            streaming_tool_calls: true,
            reasoning_request: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        if self.fail {
            return Ok(Box::pin(stream::once(async {
                Err(ProviderError::HttpStatus {
                    status: 401,
                    message: "bad key".into(),
                    retry_after: None,
                })
            })));
        }
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("h".into()),
                FakeStep::Finish(FinishReason::Length),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, _>),
        )))
    }
}

#[tokio::test]
async fn model_probe_sends_minimal_request_and_surfaces_errors() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let engine = support::engine_with_provider(
        Arc::new(ProbeProvider {
            requests: Arc::clone(&requests),
            fail: false,
        }),
        PermissionRules::default(),
    )
    .await;
    let reply = engine
        .probe_model(&ModelRef::new("probe/m"), 1)
        .await
        .expect("length finish is a normal reply");
    assert_eq!(reply.text, "h");
    assert_eq!(reply.finish, Some(FinishReason::Length));
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.max_output_tokens, Some(1));
        assert!(req.tools.is_empty());
        assert_eq!(req.reasoning, None);
        assert_eq!(req.system, None);
        assert!(matches!(
            req.messages.as_slice(),
            [Message::User { parts, .. }]
                if matches!(parts.as_slice(), [Part::Text { text, .. }] if text == "hi")
        ));
    }

    let failing = support::engine_with_provider(
        Arc::new(ProbeProvider {
            requests: Arc::new(Mutex::new(Vec::new())),
            fail: true,
        }),
        PermissionRules::default(),
    )
    .await;
    assert!(matches!(
        failing
            .probe_model(&ModelRef::new("probe/m"), 1)
            .await
            .unwrap_err(),
        ProviderError::HttpStatus { status: 401, .. }
    ));
    assert!(matches!(
        failing
            .probe_model(&ModelRef::new("probe/missing"), 1)
            .await
            .unwrap_err(),
        ProviderError::UnknownModel(_)
    ));
}
