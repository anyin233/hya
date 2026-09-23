//! Integration tests for `hya-core`: per-round token usage attributed to the
//! model that served each provider call.
//!
//! Every streaming round that reported usage appends one `UsageRecorded`
//! record carrying the serving model, including rounds of a message that
//! later ends `cancelled` or `error`. The projection folds those records into
//! `SessionProjection.usage` by model without double counting the legacy
//! `MessageFinished.tokens` sum.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::stream;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{
    AgentName, Event, FinishReason, MessageId, ModelRef, Role, SessionId, TokenUsage, UsagePurpose,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// Scripted outcome for one provider `stream()` call.
enum Outcome {
    /// Fail before any stream exists (the fallback chain may advance).
    PreStreamFailure,
    /// Request a `read` of `path`, report `usage`, finish with tool calls.
    Tool(String, TokenUsage),
    /// Stream text, report `usage`, finish with stop.
    Text(TokenUsage),
    /// Deliver the provider's usage frame, then fail mid-stream.
    UsageThenError(TokenUsage),
    /// Cancel the turn and never finish the stream.
    CancelAndHang(CancellationToken),
}

/// Provider claiming exactly one model and playing queued outcomes.
struct UsageProvider {
    claimed: &'static str,
    outcomes: Mutex<VecDeque<Outcome>>,
}

impl UsageProvider {
    fn new(claimed: &'static str, outcomes: Vec<Outcome>) -> Self {
        Self {
            claimed,
            outcomes: Mutex::new(outcomes.into()),
        }
    }
}

fn events_stream(events: Vec<Event>) -> EventStream {
    Box::pin(stream::iter(
        events.into_iter().map(Ok::<Event, ProviderError>),
    ))
}

#[async_trait]
impl Provider for UsageProvider {
    fn id(&self) -> &str {
        "usage"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == self.claimed).then(|| Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Outcome::PreStreamFailure);
        match outcome {
            Outcome::PreStreamFailure => {
                Err(ProviderError::Transport("connection reset".to_string()))
            }
            Outcome::Tool(path, usage) => Ok(events_stream(FakeProvider::materialize(
                &[
                    FakeStep::ToolCall {
                        name: "read".to_string(),
                        input: json!({ "path": path }),
                    },
                    FakeStep::Usage(usage),
                    FakeStep::Finish(FinishReason::ToolCalls),
                ],
                session,
                message,
            ))),
            Outcome::Text(usage) => Ok(events_stream(FakeProvider::materialize(
                &[
                    FakeStep::Text("done".to_string()),
                    FakeStep::Usage(usage),
                    FakeStep::Finish(FinishReason::Stop),
                ],
                session,
                message,
            ))),
            Outcome::UsageThenError(usage) => Ok(Box::pin(stream::iter([
                Ok(Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish: FinishReason::Stop,
                    tokens: Some(usage),
                    cause: None,
                }),
                Err(ProviderError::Transport("stream reset".to_string())),
            ]))),
            Outcome::CancelAndHang(cancel) => {
                cancel.cancel();
                Ok(Box::pin(stream::pending()))
            }
        }
    }
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("hya-usage-attr-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn usage(input: u64, output: u64, reasoning: Option<u64>) -> TokenUsage {
    TokenUsage {
        input,
        output,
        reasoning: reasoning.unwrap_or(0),
        cache_read: 3,
        cache_write: 2,
        reasoning_unknown: reasoning.is_none(),
    }
}

struct Fixture {
    engine: SessionEngine,
    session: SessionId,
    agent: AgentSpec,
}

/// Build an engine whose providers are scripted with the readable file path.
async fn fixture(
    script: impl FnOnce(String) -> Vec<UsageProvider>,
    fallbacks: Option<HashMap<ModelRef, Vec<ModelRef>>>,
) -> Fixture {
    let workdir = tempdir();
    let file = workdir.join("a.txt");
    std::fs::write(&file, "alpha").unwrap();
    let providers = script(file.to_string_lossy().into_owned());
    let router = providers
        .into_iter()
        .fold(ProviderRouter::new(), |router, provider| {
            router.with(Arc::new(provider))
        });
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let mut engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(router),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    );
    if let Some(fallbacks) = fallbacks {
        engine = engine.with_model_fallbacks(fallbacks);
    }
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("model-a"),
        system_prompt: "you are build".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "read the file".to_string())
        .await
        .unwrap();
    Fixture {
        engine,
        session,
        agent,
    }
}

/// `(message, step, model, purpose, tokens)` of every `UsageRecorded` record.
async fn usage_records(
    fixture: &Fixture,
) -> Vec<(
    Option<MessageId>,
    Option<u32>,
    ModelRef,
    UsagePurpose,
    TokenUsage,
)> {
    fixture
        .engine
        .replay(fixture.session)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            Event::UsageRecorded {
                message,
                step,
                model,
                purpose,
                tokens,
                ..
            } => Some((message, step, model, purpose, tokens)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn rounds_on_different_models_record_the_serving_model_per_round() {
    let round_a = usage(100, 20, None);
    let round_b = usage(40, 9, Some(4));
    // Round 0 is served by model-a; round 1's model-a attempt fails before a
    // stream exists, so the fallback chain serves it with model-b.
    let fixture = fixture(
        |path| {
            vec![
                UsageProvider::new(
                    "model-a",
                    vec![Outcome::Tool(path, round_a), Outcome::PreStreamFailure],
                ),
                UsageProvider::new("model-b", vec![Outcome::Text(round_b)]),
            ]
        },
        Some(HashMap::from([(
            ModelRef::new("model-a"),
            vec![ModelRef::new("model-a"), ModelRef::new("model-b")],
        )])),
    )
    .await;
    let finish = fixture
        .engine
        .run_turn(fixture.session, &fixture.agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let records = usage_records(&fixture).await;
    assert_eq!(records.len(), 2, "one record per billed round: {records:?}");
    let message = records[0].0.expect("turn rounds carry the message");
    assert_eq!(records[0].1, Some(0));
    assert_eq!(records[0].2, ModelRef::new("model-a"));
    assert_eq!(records[0].3, UsagePurpose::Turn);
    assert_eq!(records[0].4, round_a);
    assert_eq!(records[1].0, Some(message));
    assert_eq!(records[1].1, Some(1));
    assert_eq!(records[1].2, ModelRef::new("model-b"));
    assert_eq!(records[1].4, round_b);

    let projection = fixture
        .engine
        .read_projection(fixture.session)
        .await
        .unwrap();
    let usage = &projection.session.usage;
    assert_eq!(usage.by_model.len(), 2, "{usage:?}");
    let a = usage.by_model[&ModelRef::new("model-a")];
    assert_eq!((a.input, a.cache_read, a.cache_write), (100, 3, 2));
    assert_eq!(
        (a.output, a.reasoning_unknown_output, a.rounds),
        (20, 20, 1)
    );
    let b = usage.by_model[&ModelRef::new("model-b")];
    assert_eq!((b.output, b.reasoning, b.rounds), (9, 4, 1));
    assert_eq!(b.output_split().visible_exact(), Some(5));

    // The legacy per-message sum is unchanged for compatibility.
    let mut sum = round_a;
    sum.add(round_b);
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.id == message)
        .unwrap();
    assert_eq!(assistant.tokens, Some(sum));

    // The ledger names the serving model and counts the whole prompt.
    let ledger = fixture
        .engine
        .store()
        .read_usage(fixture.session)
        .await
        .unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].confidence, "provider");
    assert_eq!(ledger[0].model.as_deref(), Some("model-b"));
    assert_eq!(ledger[0].prompt_tokens, 140 + 6 + 4);
    assert_eq!(ledger[0].completion_tokens, 29);
}

#[tokio::test]
async fn cancelled_message_keeps_the_usage_of_billed_rounds() {
    let cancel = CancellationToken::new();
    let round = usage(70, 11, None);
    let hang = cancel.clone();
    let fixture = fixture(
        |path| {
            vec![UsageProvider::new(
                "model-a",
                vec![Outcome::Tool(path, round), Outcome::CancelAndHang(hang)],
            )]
        },
        None,
    )
    .await;
    let finish = fixture
        .engine
        .run_turn(fixture.session, &fixture.agent, cancel)
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Cancelled);

    let records = usage_records(&fixture).await;
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].2, ModelRef::new("model-a"));
    assert_eq!(records[0].4, round);

    let projection = fixture
        .engine
        .read_projection(fixture.session)
        .await
        .unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert_eq!(assistant.finish, Some(FinishReason::Cancelled));
    assert_eq!(assistant.tokens, None, "MessageFinished stays as before");
    assert_eq!(
        projection.session.usage.by_model[&ModelRef::new("model-a")].output,
        11
    );
    let ledger = fixture
        .engine
        .store()
        .read_usage(fixture.session)
        .await
        .unwrap();
    assert_eq!(ledger[0].confidence, "provider");
    assert_eq!(ledger[0].completion_tokens, 11);
}

#[tokio::test]
async fn errored_round_keeps_the_usage_received_before_the_failure() {
    let round = usage(55, 6, Some(0));
    let fixture = fixture(
        |_| {
            vec![UsageProvider::new(
                "model-a",
                vec![Outcome::UsageThenError(round)],
            )]
        },
        None,
    )
    .await;
    let outcome = fixture
        .engine
        .run_turn(fixture.session, &fixture.agent, CancellationToken::new())
        .await;
    assert!(outcome.is_err(), "mid-stream failure surfaces: {outcome:?}");

    let records = usage_records(&fixture).await;
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].1, Some(0));
    assert_eq!(records[0].4, round);
    let projection = fixture
        .engine
        .read_projection(fixture.session)
        .await
        .unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert_eq!(assistant.finish, Some(FinishReason::Error));
    assert_eq!(projection.session.usage.total().input, 55);
}
