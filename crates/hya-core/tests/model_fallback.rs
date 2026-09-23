//! Integration tests for `hya-core`: cross-model fallback chain streaming.
//!
//! The engine-level failover plane (`SessionEngine::with_model_fallbacks`)
//! advances to the next configured model only while no event stream exists.
//! These tests pin the required recovery semantics: retryable pre-stream
//! failures advance, authentication failures do not consume the chain,
//! mid-stream errors surface exactly once without failover, and an unset
//! plane preserves single-attempt behavior.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::stream;
use hya_core::{
    AgentSpec, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CoreError, CreateSession, EventBus, HookDispatcher,
    MessageUserBeforeInput, MessageUserBeforeOutcome, ModelFailureClass, ModelFallbackInput,
    ModelFallbackOutcome, SessionEngine, TextCompleteInput, TextCompleteOutcome,
    ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome,
};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MessageId, ModelRef, PartProjection, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter, ReasoningEffort,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use tokio_util::sync::CancellationToken;

/// Scripted outcome for one provider `stream()` call.
enum Outcome {
    /// Fail before any stream exists (router/engine may still advance).
    PreStreamFailure(ProviderError),
    /// Succeed in producing a stream whose first item is an error. Per the
    /// no-replay boundary that error must surface once and never fail over.
    MidStreamFailure(ProviderError),
    /// Stream one text part and finish with `Stop`.
    Complete(&'static str),
}

/// Provider that claims exactly one model id and plays queued outcomes.
struct ScriptedModelProvider {
    claimed_model: &'static str,
    outcomes: Mutex<VecDeque<Outcome>>,
    attempts: Arc<Mutex<Vec<String>>>,
    request_reasoning: Arc<Mutex<Vec<Option<ReasoningEffort>>>>,
}

impl ScriptedModelProvider {
    fn new(
        claimed_model: &'static str,
        outcomes: Vec<Outcome>,
        attempts: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            claimed_model,
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
            parallel_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
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
        assert_eq!(
            req.model.as_str(),
            self.claimed_model,
            "engine must only route this provider its own model"
        );
        self.attempts.lock().unwrap().push(req.model.to_string());
        self.request_reasoning.lock().unwrap().push(req.reasoning);
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                Outcome::PreStreamFailure(ProviderError::Transport("script exhausted".to_string()))
            });
        match outcome {
            Outcome::PreStreamFailure(error) => Err(error),
            Outcome::MidStreamFailure(error) => {
                Ok(Box::pin(stream::once(async move { Err(error) })))
            }
            Outcome::Complete(text) => {
                let events = FakeProvider::materialize(
                    &[
                        FakeStep::Text(text.to_string()),
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

fn attempts_counter() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

fn chain(preferred: &str, rest: &[&str]) -> HashMap<ModelRef, Vec<ModelRef>> {
    let mut candidates = vec![ModelRef::new(preferred)];
    candidates.extend(rest.iter().map(|model| ModelRef::new(*model)));
    HashMap::from([(ModelRef::new(preferred), candidates)])
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("hya-fallback-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct TurnFixture {
    engine: SessionEngine,
    session: hya_proto::SessionId,
    agent: AgentSpec,
}

async fn fixture(
    providers: Vec<ScriptedModelProvider>,
    fallbacks: Option<HashMap<ModelRef, Vec<ModelRef>>>,
    session_model: &str,
) -> TurnFixture {
    fixture_with_hooks(providers, fallbacks, session_model, None).await
}

async fn fixture_with_hooks(
    providers: Vec<ScriptedModelProvider>,
    fallbacks: Option<HashMap<ModelRef, Vec<ModelRef>>>,
    session_model: &str,
    hooks: Option<Arc<dyn HookDispatcher>>,
) -> TurnFixture {
    let workdir = tempdir();
    let router = ProviderRouter::new();
    let router = providers
        .into_iter()
        .fold(router, |router, provider| router.with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let agent_name = AgentName::new("build");
    let agent = AgentSpec {
        name: agent_name.clone(),
        model: ModelRef::new(session_model),
        system_prompt: "you are build".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let mut engine = SessionEngine::new(
        store,
        Arc::new(router),
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    if let Some(fallbacks) = fallbacks {
        engine = engine.with_model_fallbacks(fallbacks);
    }
    if let Some(hooks) = hooks {
        engine = engine.with_hooks(hooks);
    }
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent_name,
            model: ModelRef::new(session_model),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "say something".to_string())
        .await
        .unwrap();
    TurnFixture {
        engine,
        session,
        agent,
    }
}

async fn assistant_text(fixture: &TurnFixture) -> String {
    let projection = fixture
        .engine
        .store()
        .read_projection(fixture.session)
        .await
        .unwrap();
    projection
        .session
        .messages
        .iter()
        .filter(|m| m.role == hya_proto::Role::Assistant)
        .flat_map(|m| m.parts.iter())
        .filter_map(|p| match p {
            PartProjection::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn fallback_succeeds_after_retryable_pre_stream_failure() {
    let attempts = attempts_counter();
    let spare_attempts = attempts_counter();
    let primary = ScriptedModelProvider::new(
        "alpha",
        vec![
            Outcome::PreStreamFailure(ProviderError::Transport("connection reset".to_string())),
            Outcome::Complete("PRIMARY_RECOVERED"),
        ],
        attempts.clone(),
    );
    let spare = ScriptedModelProvider::new(
        "beta",
        vec![Outcome::Complete("BETA_LANDED")],
        spare_attempts.clone(),
    );
    let fixt = fixture(
        vec![primary, spare],
        Some(chain("alpha", &["beta"])),
        "alpha",
    )
    .await;

    // Two turns pin both recovery directions. Turn one: alpha fails before
    // any stream exists, so the engine advances to beta for delivery. Turn
    // two restarts at the preferred model, whose queued healthy script now
    // streams directly.
    let finish = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    assert!(assistant_text(&fixt).await.contains("BETA_LANDED"));

    let finish = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    assert!(assistant_text(&fixt).await.contains("PRIMARY_RECOVERED"));

    assert_eq!(attempts.lock().unwrap().len(), 2, "alpha tried twice");
    assert_eq!(spare_attempts.lock().unwrap().len(), 1, "beta served once");
}

#[tokio::test]
async fn fallback_advances_on_first_failure_and_recovers() {
    let attempts = attempts_counter();
    let spare_attempts = attempts_counter();
    let primary = ScriptedModelProvider::new(
        "alpha",
        vec![Outcome::PreStreamFailure(ProviderError::HttpStatus {
            status: 503,
            message: "overloaded".to_string(),
            retry_after: None,
        })],
        attempts.clone(),
    );
    let spare = ScriptedModelProvider::new(
        "beta",
        vec![Outcome::Complete("BETA_LANDED")],
        spare_attempts.clone(),
    );
    let fixt = fixture(
        vec![primary, spare],
        Some(chain("alpha", &["beta"])),
        "alpha",
    )
    .await;

    let finish = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    assert!(assistant_text(&fixt).await.contains("BETA_LANDED"));
    assert_eq!(*attempts.lock().unwrap(), vec!["alpha".to_string()]);
    assert_eq!(*spare_attempts.lock().unwrap(), vec!["beta".to_string()]);
}

#[tokio::test]
async fn auth_failure_does_not_consume_chain() {
    let locked_attempts = attempts_counter();
    let spare_attempts = attempts_counter();
    let locked = ScriptedModelProvider::new(
        "locked",
        vec![Outcome::PreStreamFailure(ProviderError::AuthExpired {
            provider: "locked".to_string(),
            hint: "re-login".to_string(),
        })],
        locked_attempts.clone(),
    );
    let spare = ScriptedModelProvider::new(
        "spare",
        vec![Outcome::Complete("SPARE_TEXT")],
        spare_attempts.clone(),
    );
    let fixt = fixture(
        vec![locked, spare],
        Some(chain("locked", &["spare"])),
        "locked",
    )
    .await;

    let error = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap_err();
    match &error {
        CoreError::Provider(inner) => match inner.as_ref() {
            ProviderError::AuthExpired { provider, .. } => {
                assert_eq!(provider, "locked");
            }
            other => panic!("expected auth expiry, got: {other:?}"),
        },
        other => panic!("expected provider error, got: {other:?}"),
    }
    assert_eq!(
        locked_attempts.lock().unwrap().len(),
        1,
        "non-retryable failure must not be retried on the same model"
    );
    assert_eq!(
        spare_attempts.lock().unwrap().len(),
        0,
        "auth failure must not consume the fallback chain"
    );
}

#[tokio::test]
async fn post_stream_error_is_delivered_once_and_never_failed_over() {
    let drop_attempts = attempts_counter();
    let spare_attempts = attempts_counter();
    let dropper = ScriptedModelProvider::new(
        "dropzone",
        vec![Outcome::MidStreamFailure(ProviderError::Decode(
            "sse frame cut".to_string(),
        ))],
        drop_attempts.clone(),
    );
    let spare = ScriptedModelProvider::new(
        "spare",
        vec![Outcome::Complete("SPARE_TEXT")],
        spare_attempts.clone(),
    );
    let fixt = fixture(
        vec![dropper, spare],
        Some(chain("dropzone", &["spare"])),
        "dropzone",
    )
    .await;

    let error = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap_err();
    match &error {
        CoreError::Provider(inner) => match inner.as_ref() {
            ProviderError::Decode(reason) => assert_eq!(reason, "sse frame cut"),
            other => panic!("expected mid-stream decode error, got: {other:?}"),
        },
        other => panic!("expected provider error, got: {other:?}"),
    }
    assert_eq!(
        drop_attempts.lock().unwrap().len(),
        1,
        "established stream must own delivery until it ends"
    );
    assert_eq!(
        spare_attempts.lock().unwrap().len(),
        0,
        "post-stream errors must never fail over"
    );
}

#[tokio::test]
async fn empty_chain_preserves_single_attempt_behavior() {
    let solo_attempts = attempts_counter();
    let unused_attempts = attempts_counter();
    let solo = ScriptedModelProvider::new(
        "solo",
        vec![
            Outcome::PreStreamFailure(ProviderError::Transport("down".to_string())),
            Outcome::Complete("SHOULD_NEVER_STREAM"),
        ],
        solo_attempts.clone(),
    );
    let unused = ScriptedModelProvider::new(
        "unused",
        vec![Outcome::Complete("UNUSED_TEXT")],
        unused_attempts.clone(),
    );
    // No with_model_fallbacks installed: today's behavior exactly.
    let fixt = fixture(vec![solo, unused], None, "solo").await;

    let error = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap_err();
    match &error {
        CoreError::Provider(inner) => {
            assert!(matches!(inner.as_ref(), ProviderError::Transport(_)));
        }
        other => panic!("expected transport error, got: {other:?}"),
    }
    assert_eq!(
        solo_attempts.lock().unwrap().len(),
        1,
        "unset plane must keep the exact single direct-router attempt"
    );
    assert_eq!(
        unused_attempts.lock().unwrap().len(),
        0,
        "no cross-model step may happen without a configured chain"
    );
}

#[tokio::test]
async fn unknown_model_advances_to_next_chain_candidate() {
    let rescued_attempts = attempts_counter();
    let rescued = ScriptedModelProvider::new(
        "rescued",
        vec![Outcome::Complete("RESCUED_TEXT")],
        rescued_attempts.clone(),
    );
    // No route claims "ghost": the router answers UnknownModel, which is a
    // legitimate pre-stream condition for advancing the chain.
    let fixt = fixture(vec![rescued], Some(chain("ghost", &["rescued"])), "ghost").await;

    let finish = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    assert!(assistant_text(&fixt).await.contains("RESCUED_TEXT"));
    assert_eq!(rescued_attempts.lock().unwrap().len(), 1);
}

/// Ensure each model candidate receives the reasoning variant encoded in its model ref.
#[tokio::test]
async fn fallback_uses_each_candidate_reasoning_variant() {
    let preferred_attempts = attempts_counter();
    let fallback_attempts = attempts_counter();
    let preferred = ScriptedModelProvider::new(
        "preferred#low",
        vec![Outcome::PreStreamFailure(ProviderError::HttpStatus {
            status: 503,
            message: "preferred unavailable".to_string(),
            retry_after: None,
        })],
        preferred_attempts.clone(),
    );
    let fallback = ScriptedModelProvider::new(
        "fallback#high",
        vec![Outcome::Complete("FALLBACK_TEXT")],
        fallback_attempts.clone(),
    );
    let preferred_reasoning = preferred.request_reasoning.clone();
    let fallback_reasoning = fallback.request_reasoning.clone();
    let fixt = fixture(
        vec![preferred, fallback],
        Some(chain("preferred#low", &["fallback#high"])),
        "preferred#low",
    )
    .await;

    let finish = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    assert!(assistant_text(&fixt).await.contains("FALLBACK_TEXT"));
    assert_eq!(
        *preferred_attempts.lock().unwrap(),
        vec!["preferred#low".to_string()],
        "preferred candidate must be attempted first",
    );
    assert_eq!(
        *fallback_attempts.lock().unwrap(),
        vec!["fallback#high".to_string()],
        "fallback candidate must be attempted after the preferred failure",
    );
    assert_eq!(
        *preferred_reasoning.lock().unwrap(),
        vec![Some(ReasoningEffort::Low)],
        "preferred #low request must carry low reasoning",
    );
    assert_eq!(
        *fallback_reasoning.lock().unwrap(),
        vec![Some(ReasoningEffort::High)],
        "fallback #high request must carry high reasoning",
    );
}

/// `model.fallback` test double: answers from a script (then gives up) and
/// records every consult.
struct FallbackHook {
    answers: Mutex<VecDeque<ModelFallbackOutcome>>,
    /// When set, every consult past the script retries `<prefix>-<attempt>`.
    endless_prefix: Option<&'static str>,
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
                consults: Arc::clone(&consults),
            }),
            consults,
        )
    }
}

fn retry(model: &str) -> ModelFallbackOutcome {
    ModelFallbackOutcome::Retry {
        model: ModelRef::new(model),
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

    async fn model_fallback(&self, input: ModelFallbackInput) -> ModelFallbackOutcome {
        let attempt = input.attempt;
        self.consults.lock().unwrap().push(input);
        if let Some(answer) = self.answers.lock().unwrap().pop_front() {
            return answer;
        }
        match self.endless_prefix {
            Some(prefix) => retry(&format!("{prefix}-{attempt}")),
            None => ModelFallbackOutcome::GiveUp,
        }
    }
}

fn provider_error(error: &CoreError) -> &ProviderError {
    match error {
        CoreError::Provider(inner) => inner.as_ref(),
        other => panic!("expected provider error, got: {other:?}"),
    }
}

/// A non-retryable pre-stream failure with no configured chain consults the
/// hook, which picks a model that then streams.
#[tokio::test]
async fn model_fallback_hook_supplies_next_model_after_pre_stream_failure() {
    let alpha_attempts = attempts_counter();
    let beta_attempts = attempts_counter();
    let alpha = ScriptedModelProvider::new(
        "alpha",
        vec![Outcome::PreStreamFailure(ProviderError::AuthExpired {
            provider: "alpha".to_string(),
            hint: "re-login".to_string(),
        })],
        alpha_attempts.clone(),
    );
    let beta = ScriptedModelProvider::new(
        "beta",
        vec![Outcome::Complete("BETA_BY_HOOK")],
        beta_attempts.clone(),
    );
    let (hook, consults) = FallbackHook::scripted(vec![retry("beta")]);
    let fixt = fixture_with_hooks(vec![alpha, beta], None, "alpha", Some(hook)).await;

    let finish = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(finish, FinishReason::Stop);
    assert!(assistant_text(&fixt).await.contains("BETA_BY_HOOK"));
    assert_eq!(*alpha_attempts.lock().unwrap(), vec!["alpha".to_string()]);
    assert_eq!(*beta_attempts.lock().unwrap(), vec!["beta".to_string()]);
    let consults = consults.lock().unwrap();
    assert_eq!(consults.len(), 1);
    let consult = &consults[0];
    assert_eq!(consult.session, fixt.session);
    assert_eq!(consult.root_session, fixt.session);
    assert_eq!(consult.agent, Some(AgentName::new("build")));
    assert_eq!(consult.model, ModelRef::new("alpha"));
    assert_eq!(consult.error_class, ModelFailureClass::Auth);
    assert!(consult.error_message.contains("re-login"));
    assert_eq!(consult.attempt, 1);
    assert_eq!(consult.tried, vec![ModelRef::new("alpha")]);
}

/// The configured chain is walked exactly as before; the hook is consulted
/// only when the chain is exhausted, and sees every model tried this round.
#[tokio::test]
async fn model_fallback_hook_runs_after_configured_chain_is_exhausted() {
    let attempts = attempts_counter();
    let alpha = ScriptedModelProvider::new(
        "alpha",
        vec![Outcome::PreStreamFailure(ProviderError::Transport(
            "reset".to_string(),
        ))],
        attempts.clone(),
    );
    let beta = ScriptedModelProvider::new(
        "beta",
        vec![Outcome::PreStreamFailure(ProviderError::HttpStatus {
            status: 529,
            message: "overloaded".to_string(),
            retry_after: None,
        })],
        attempts.clone(),
    );
    let gamma = ScriptedModelProvider::new(
        "gamma",
        vec![Outcome::Complete("GAMMA_BY_HOOK")],
        attempts.clone(),
    );
    let (hook, consults) = FallbackHook::scripted(vec![retry("gamma")]);
    let fixt = fixture_with_hooks(
        vec![alpha, beta, gamma],
        Some(chain("alpha", &["beta"])),
        "alpha",
        Some(hook),
    )
    .await;

    fixt.engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap();

    assert!(assistant_text(&fixt).await.contains("GAMMA_BY_HOOK"));
    assert_eq!(*attempts.lock().unwrap(), vec!["alpha", "beta", "gamma"]);
    let consults = consults.lock().unwrap();
    assert_eq!(consults.len(), 1, "no consult while the chain can advance");
    assert_eq!(consults[0].model, ModelRef::new("beta"));
    assert_eq!(consults[0].error_class, ModelFailureClass::Retryable);
    assert_eq!(consults[0].attempt, 2);
    assert_eq!(
        consults[0].tried,
        vec![ModelRef::new("alpha"), ModelRef::new("beta")]
    );
}

/// A retry that fails again re-consults with the grown history; `give_up`
/// surfaces the latest failure.
#[tokio::test]
async fn model_fallback_give_up_surfaces_the_latest_failure() {
    let attempts = attempts_counter();
    let alpha = ScriptedModelProvider::new(
        "alpha",
        vec![Outcome::PreStreamFailure(ProviderError::Transport(
            "reset".to_string(),
        ))],
        attempts.clone(),
    );
    let beta = ScriptedModelProvider::new(
        "beta",
        vec![Outcome::PreStreamFailure(ProviderError::HttpStatus {
            status: 400,
            message: "bad request".to_string(),
            retry_after: None,
        })],
        attempts.clone(),
    );
    let (hook, consults) =
        FallbackHook::scripted(vec![retry("beta"), ModelFallbackOutcome::GiveUp]);
    let fixt = fixture_with_hooks(vec![alpha, beta], None, "alpha", Some(hook)).await;

    let error = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(
        provider_error(&error),
        ProviderError::HttpStatus { status: 400, .. }
    ));
    assert_eq!(*attempts.lock().unwrap(), vec!["alpha", "beta"]);
    let consults = consults.lock().unwrap();
    assert_eq!(consults.len(), 2);
    assert_eq!(consults[1].model, ModelRef::new("beta"));
    assert_eq!(consults[1].error_class, ModelFailureClass::InvalidRequest);
    assert_eq!(consults[1].attempt, 2);
}

/// A retry naming a model already tried this round is refused (treated as
/// give-up), so a hook cannot loop the turn.
#[tokio::test]
async fn model_fallback_refuses_a_model_already_tried() {
    let attempts = attempts_counter();
    let alpha = ScriptedModelProvider::new(
        "alpha",
        vec![
            Outcome::PreStreamFailure(ProviderError::Transport("reset".to_string())),
            Outcome::Complete("ALPHA_AGAIN"),
        ],
        attempts.clone(),
    );
    let (hook, consults) = FallbackHook::scripted(vec![retry("alpha"), retry("beta")]);
    let fixt = fixture_with_hooks(vec![alpha], None, "alpha", Some(hook)).await;

    let error = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(
        provider_error(&error),
        ProviderError::Transport(_)
    ));
    assert_eq!(
        *attempts.lock().unwrap(),
        vec!["alpha"],
        "alpha is never replayed"
    );
    assert_eq!(
        consults.lock().unwrap().len(),
        1,
        "a refused retry ends the round"
    );
}

/// At most eight attempts per round, whatever the hook keeps answering.
#[tokio::test]
async fn model_fallback_is_capped_at_eight_attempts_per_round() {
    let attempts = attempts_counter();
    let alpha = ScriptedModelProvider::new(
        "alpha",
        vec![Outcome::PreStreamFailure(ProviderError::Transport(
            "reset".to_string(),
        ))],
        attempts.clone(),
    );
    let consults = Arc::new(Mutex::new(Vec::new()));
    let hook = Arc::new(FallbackHook {
        answers: Mutex::new(VecDeque::new()),
        endless_prefix: Some("ghost"),
        consults: Arc::clone(&consults),
    });
    let fixt = fixture_with_hooks(vec![alpha], None, "alpha", Some(hook)).await;

    let error = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap_err();

    // Every hook model is unrouted, so the last failure is UnknownModel.
    assert!(matches!(
        provider_error(&error),
        ProviderError::UnknownModel(_)
    ));
    let consults = consults.lock().unwrap();
    assert_eq!(
        consults.len(),
        7,
        "alpha plus seven hook picks is eight attempts"
    );
    assert_eq!(consults[6].tried.len(), 7);
    assert_eq!(consults[6].error_class, ModelFailureClass::UnknownModel);
}

/// STRICT NO-REPLAY: once a stream exists the hook is never consulted.
#[tokio::test]
async fn model_fallback_is_never_consulted_after_a_stream_exists() {
    let attempts = attempts_counter();
    let dropper = ScriptedModelProvider::new(
        "dropzone",
        vec![Outcome::MidStreamFailure(ProviderError::Decode(
            "sse frame cut".to_string(),
        ))],
        attempts.clone(),
    );
    let spare = ScriptedModelProvider::new(
        "spare",
        vec![Outcome::Complete("SPARE_TEXT")],
        attempts.clone(),
    );
    let (hook, consults) = FallbackHook::scripted(vec![retry("spare")]);
    let fixt = fixture_with_hooks(vec![dropper, spare], None, "dropzone", Some(hook)).await;

    let error = fixt
        .engine
        .run_turn(fixt.session, &fixt.agent, CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(provider_error(&error), ProviderError::Decode(_)));
    assert_eq!(*attempts.lock().unwrap(), vec!["dropzone"]);
    assert!(consults.lock().unwrap().is_empty());
}

/// Error classes map from provider errors as documented.
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
            ProviderError::UnknownModel("x".to_string()),
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
