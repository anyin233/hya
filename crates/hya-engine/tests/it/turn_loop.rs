//! Core turn loop, streaming delta/snapshot parity, context reduction/eviction,
//! single-active-turn serialization, and event-sourced todo projection.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use hya_engine::{
    AgentSpec, CompactionConfig, CoreError, CreateSession, EventBus, ResidentSupervisor,
    SessionEngine, SubagentGovernor, SubagentLimits, Summarizer, TokenAccountingMode, TokenSource,
};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MailEndpoint, MailKind, Message, MessageId,
    MessageProjection, ModelRef, Part, PartId, PartProjection, Projection, Role, RosterStatus,
    SessionId, SessionProjection, TodoItem, TodoStatus, TokenUsage, ToolPartState,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule};
use serde_json::json;
use tokio::sync::{Barrier, Notify, broadcast};
use tokio_util::sync::CancellationToken;

struct NoSummarizer;

#[async_trait]
impl Summarizer for NoSummarizer {
    async fn summarize(
        &self,
        _messages: &[Message],
        _options: hya_engine::SummarizeOptions,
    ) -> Result<String, CoreError> {
        Err(CoreError::Invalid("no summarizer in this test".into()))
    }
}

fn rewrite_count(envelopes: &[Envelope]) -> usize {
    envelopes
        .iter()
        .filter(|e| matches!(e.event, Event::ContextCompacted { .. }))
        .count()
}

fn eviction_count(envelopes: &[Envelope]) -> usize {
    envelopes
        .iter()
        .filter(|e| {
            matches!(
                hya_tool::extension::ExtensionEvent::from_event(&e.event),
                Some(hya_tool::extension::ExtensionEvent::ContextEvicted { .. })
            )
        })
        .count()
}

#[tokio::test]
async fn text_tool_result_text_round_trip_and_order() {
    let dir = support::TestDir::new("turn-roundtrip");
    let file = dir.path().join("foo.txt");
    tokio::fs::write(&file, "42 lines").await.unwrap();
    let path = file.to_string_lossy().into_owned();
    let read_input = json!({ "filePath": path, "path": path, "offset": 0, "limit": 2000 });

    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("I'll read it".into()),
            FakeStep::ToolCall {
                name: "read".into(),
                input: read_input.clone(),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("The file says 42 lines".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let engine = support::engine_with_provider(
        Arc::new(provider),
        PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Allow)]),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "read foo.txt".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(
                session,
                &support::agent_spec("build", "fake", dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );

    let events = engine.store().replay(session).await.unwrap();
    assert!(events.iter().any(|res| {
        let Event::ToolResult { call: res_call, .. } = &res.event else {
            return false;
        };
        events.iter().any(|req| {
            matches!(
                &req.event,
                Event::ToolCallRequested { call, name, input, .. }
                    if call == res_call && name.as_str() == "read" && input == &read_input
            )
        })
    }));
    let proj = engine.store().read_projection(session).await.unwrap();
    let assistant = proj
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    let kinds: Vec<_> = assistant
        .parts
        .iter()
        .filter_map(|p| match p {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "read" && output["content"] == "42 lines" => Some("<read>"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["I'll read it", "<read>", "The file says 42 lines"]);
}

#[tokio::test]
async fn turn_continues_past_twenty_five_tool_rounds() {
    let dir = support::TestDir::new("turn-26-rounds");
    let mut scripts: Vec<_> = (0..26)
        .map(|_| {
            vec![
                FakeStep::ToolCall {
                    name: "unknown".into(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        })
        .collect();
    scripts.push(vec![
        FakeStep::Text("continued after twenty-five tool rounds".into()),
        FakeStep::Finish(FinishReason::Stop),
    ]);
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted_turns(scripts)),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    assert_eq!(
        engine
            .run_turn(
                session,
                &support::agent_spec("build", "fake", dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(
        support::assistant_text(&engine, session).await,
        "continued after twenty-five tool rounds"
    );
}

#[tokio::test]
async fn cancel_provider_error_and_usage_record_on_assistant_projection() {
    let dir = support::TestDir::new("turn-outcomes");

    // 1. Pre-cancelled turn finishes Cancelled.
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted_turns(vec![vec![
            FakeStep::Text("hi".into()),
            FakeStep::Finish(FinishReason::Stop),
        ]])),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        engine
            .run_turn(
                session,
                &support::agent_spec("build", "fake", dir.path()),
                cancel
            )
            .await
            .unwrap(),
        FinishReason::Cancelled
    );

    // 2. Provider usage is recorded on assistant projection.
    let usage = TokenUsage {
        input: 11,
        output: 3,
        reasoning: 2,
        cache_read: 5,
        cache_write: 0,
        reasoning_unknown: false,
    };
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted_turns(vec![vec![
            FakeStep::Text("hi".into()),
            FakeStep::Usage(usage),
            FakeStep::Finish(FinishReason::Stop),
        ]])),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    engine
        .run_turn(
            session,
            &support::agent_spec("build", "fake", dir.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let proj = engine.store().read_projection(session).await.unwrap();
    assert_eq!(
        proj.session
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .unwrap()
            .tokens,
        Some(usage)
    );

    // 3. Unresolved model surfaces error and still closes assistant message with Error.
    let engine = support::engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new()),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "no-such-model", dir.path()).await;
    engine
        .admit_user_prompt(session, "hello".into())
        .await
        .unwrap();
    assert!(
        engine
            .run_turn(
                session,
                &support::agent_spec("build", "no-such-model", dir.path()),
                CancellationToken::new(),
            )
            .await
            .is_err()
    );
    let proj = engine.store().read_projection(session).await.unwrap();
    assert_eq!(
        proj.session
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .unwrap()
            .finish,
        Some(FinishReason::Error)
    );
}

#[tokio::test]
async fn kernel_rewrite_persists_and_does_not_repeat_next_turn() {
    let dir = support::TestDir::new("turn-rewrite");
    let provider = Arc::new(
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::Text("detail across the earlier turn ".repeat(7_000)),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::Text("second".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::Text("third".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ])
        .with_max_context(40_000),
    );
    let engine = support::engine_with_provider(provider.clone(), PermissionRules::default()).await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    let agent = support::agent_spec("build", "fake", dir.path());

    for prompt in ["ORIGINAL_PROMPT_0", "NEXT_PROMPT_1", "THIRD_PROMPT"] {
        engine
            .admit_user_prompt(session, prompt.into())
            .await
            .unwrap();
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap();
    }

    let envelopes = engine.replay(session).await.unwrap();
    let records: Vec<_> = envelopes
        .iter()
        .filter_map(|e| match &e.event {
            Event::ContextCompacted {
                message,
                strategy,
                from_message,
                to_message,
                folded_count,
                threshold,
                ..
            } => Some((
                *message,
                *strategy,
                *from_message,
                *to_message,
                *folded_count,
                *threshold,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(records.len(), 1);
    let (marker_id, strategy, from_msg, to_msg, folded_count, threshold) = records[0];
    assert_eq!(
        (strategy, threshold),
        (hya_proto::CompactionStrategy::Rewrite, 32_000)
    );

    let proj = Projection::from_events(&envelopes);
    let marker = proj
        .session
        .messages
        .iter()
        .find(|m| m.id == marker_id)
        .unwrap();
    assert_eq!(marker.role, Role::System);
    let marker_text: String = marker
        .parts
        .iter()
        .filter_map(|p| match p {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        marker_text.starts_with("HYA_COMPACTED_CONTEXT")
            && marker_text.contains("ORIGINAL_PROMPT_0")
    );
    let ids: Vec<_> = proj.session.messages.iter().map(|m| m.id).collect();
    let from_idx = ids.iter().position(|id| *id == from_msg).unwrap();
    let to_idx = ids.iter().position(|id| *id == to_msg).unwrap();
    assert_eq!(to_idx - from_idx + 1, folded_count as usize);

    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    for req in &requests[1..] {
        assert!(!format!("{req:?}").contains("detail across the earlier turn"));
    }
}

#[tokio::test]
async fn tool_output_eviction_avoids_rewrite_sticks_and_matches_after_restart() {
    let dir = support::TestDir::new("turn-evict-restart");
    let big_file = dir.path().join("big.txt");
    tokio::fs::write(&big_file, format!("{}\n", "R".repeat(100)).repeat(200))
        .await
        .unwrap();
    let provider = Arc::new(
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "read".into(),
                    input: json!({ "path": big_file.to_string_lossy() }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("read it".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![FakeStep::Finish(FinishReason::Stop)],
            vec![
                FakeStep::Text("restarted".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::Text("third turn".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ])
        .with_max_context(26_000),
    );
    let db = dir.path().join("hya.db").to_string_lossy().into_owned();
    let build_engine = |store: SessionStore| {
        let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Read,
            "/**",
            Mode::Allow,
        )]));
        SessionEngine::new(
            store,
            Arc::new(ProviderRouter::new().with(provider.clone())),
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm,
            EventBus::default(),
        )
        .with_compaction(
            Arc::new(NoSummarizer),
            CompactionConfig {
                keep_recent: 1,
                ..CompactionConfig::default()
            },
        )
    };
    let agent = support::agent_spec("build", "fake", dir.path());

    let engine = build_engine(SessionStore::connect(&db).await.unwrap());
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    for prompt in ["read big".to_string(), "now summarize ".repeat(3_000)] {
        engine.admit_user_prompt(session, prompt).await.unwrap();
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap();
    }
    let envelopes = engine.replay(session).await.unwrap();
    assert_eq!(eviction_count(&envelopes), 1);
    assert_eq!(rewrite_count(&envelopes), 0);
    // Log keeps full fidelity even though the request evicted the stale output.
    let proj = Projection::from_events(&envelopes);
    assert!(
        proj.session
            .messages
            .iter()
            .any(|m| m.parts.iter().any(|p| matches!(
                p,
                PartProjection::Tool { state: ToolPartState::Completed { output, .. }, .. }
                    if output.to_string().contains("RRRRRRRRRR")
            )))
    );
    drop(engine);

    // Reopened engine sends the identical evicted request without re-evicting,
    // and a follow-up prompt stays sticky without escalating to rewrite.
    let engine = build_engine(SessionStore::connect(&db).await.unwrap());
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    let requests = provider.requests();
    let [.., before, after] = requests.as_slice() else {
        panic!("expected requests before and after restart");
    };
    let before = format!("{before:?}");
    assert!(before.contains("artifact://"));
    assert_eq!(before, format!("{after:?}"));

    engine
        .admit_user_prompt(session, "and once more".into())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    let after_envelopes = engine.replay(session).await.unwrap();
    assert_eq!(eviction_count(&after_envelopes), 1);
    assert_eq!(rewrite_count(&after_envelopes), 0);
}

#[tokio::test]
async fn context_status_reports_round_occupancy_and_accounting_mode() {
    let dir = support::TestDir::new("turn-ctx-status");
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted_turns(vec![vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ]])),
        PermissionRules::default(),
    )
    .await
    .with_compaction(Arc::new(NoSummarizer), CompactionConfig::default());
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "hello".into())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &support::agent_spec("build", "fake", dir.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let envelopes = engine.store().replay(session).await.unwrap();
    let statuses: Vec<_> = envelopes
        .iter()
        .filter_map(
            |e| match hya_tool::extension::ExtensionEvent::from_event(&e.event) {
                Some(hya_tool::extension::ExtensionEvent::ContextStatus {
                    tokens,
                    source,
                    mode,
                    threshold,
                    ..
                }) => Some((tokens, source, mode, threshold)),
                _ => None,
            },
        )
        .collect();
    assert_eq!(statuses.len(), 1);
    let (tokens, source, mode, threshold) = statuses[0];
    assert!(tokens > 0);
    assert_eq!(
        (mode, source, threshold),
        (TokenAccountingMode::Auto, TokenSource::Estimate, 160_000)
    );
    let projection = engine.read_projection(session).await.unwrap();
    let folded =
        hya_tool::extension::context_status_projection(&projection).expect("context status");
    assert_eq!(
        (folded.tokens, folded.source, folded.mode, folded.threshold),
        (tokens, source, mode, threshold)
    );
}

struct OpaqueReasoningRoute {
    inner: FakeProvider,
    blob: String,
    max_context: Arc<AtomicU32>,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for OpaqueReasoningRoute {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            max_context: self.max_context.load(Ordering::SeqCst),
            ..Capabilities::default()
        })
    }

    fn reasoning_replay_policy(&self) -> hya_provider::ReasoningReplayPolicy {
        hya_provider::ReasoningReplayPolicy::ProviderData
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req.clone());
        let blob = self.blob.clone();
        let stream = self.inner.stream(req, session, message).await?;
        Ok(Box::pin(stream.map(move |event| match event {
            Ok(Event::ReasoningEnd {
                session,
                message,
                part,
                ..
            }) => Ok(Event::ReasoningEnd {
                session,
                message,
                part,
                provider_data: Some(json!({
                    "type": "reasoning",
                    "id": part.to_string(),
                    "encrypted_content": blob,
                })),
            }),
            other => other,
        })))
    }
}

#[tokio::test]
async fn stale_tool_outputs_spill_mid_turn_and_escalate_to_rewrite_when_needed() {
    let dir = support::TestDir::new("turn-spill");
    let big_file = dir.path().join("big.txt");
    tokio::fs::write(&big_file, format!("{}\n", "R".repeat(100)).repeat(200))
        .await
        .unwrap();
    let path = big_file.to_string_lossy().into_owned();
    let read = || FakeStep::ToolCall {
        name: "read".into(),
        input: json!({ "path": path }),
    };
    let read_rules = || PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Allow)]);

    // 1. Mid-turn spilling inside one assistant message keeps rewrites at 0.
    let provider = FakeProvider::scripted_turns(vec![
        vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
        vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
        vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ])
    .with_max_context(30_000);
    let engine = support::engine_with_provider(Arc::new(provider), read_rules())
        .await
        .with_compaction(
            Arc::new(NoSummarizer),
            CompactionConfig {
                keep_recent: 1,
                ..CompactionConfig::default()
            },
        );
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "read three times".into())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &support::agent_spec("build", "fake", dir.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let envelopes = engine.replay(session).await.unwrap();
    assert_eq!(
        Projection::from_events(&envelopes)
            .session
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count(),
        1
    );
    assert!(eviction_count(&envelopes) > 0 && rewrite_count(&envelopes) == 0);

    // 2. When spilling alone is insufficient, kernel rewrites before sending the round.
    let tokenizer = hya_engine::tokens::TokenAccounting::default();
    let unit = "earlier context ".repeat(100);
    let prompt = unit.repeat(40_000 / tokenizer.tokenizer().count_text(&unit) + 1);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let window = Arc::new(AtomicU32::new(10_000_000));
    let route = OpaqueReasoningRoute {
        inner: FakeProvider::scripted_turns(vec![
            vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
            vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
            vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
            vec![
                FakeStep::Text("read".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::Text("done".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]),
        blob: String::new(),
        max_context: Arc::clone(&window),
        requests: Arc::clone(&requests),
    };
    let engine = support::engine_with_provider(Arc::new(route), read_rules())
        .await
        .with_compaction(
            Arc::new(NoSummarizer),
            CompactionConfig {
                keep_recent: 2,
                ..CompactionConfig::default()
            },
        );
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    let agent = support::agent_spec("build", "fake", dir.path());
    engine.admit_user_prompt(session, prompt).await.unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    window.store(80_000, Ordering::SeqCst);
    engine
        .admit_user_prompt(session, "go on".into())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    let reductions: Vec<_> = engine
        .replay(session)
        .await
        .unwrap()
        .iter()
        .filter_map(|e| {
            if matches!(
                hya_tool::extension::ExtensionEvent::from_event(&e.event),
                Some(hya_tool::extension::ExtensionEvent::ContextEvicted { .. })
            ) {
                return Some("spill");
            }
            match e.event {
                Event::ContextCompacted {
                    strategy: hya_proto::CompactionStrategy::Rewrite,
                    ..
                } => Some("rewrite"),
                Event::StepStarted { .. } => Some("step"),
                _ => None,
            }
        })
        .collect();
    assert_eq!(
        reductions,
        ["step", "step", "step", "step", "spill", "rewrite", "step"]
    );
    let last = format!("{:?}", requests.lock().unwrap().last().unwrap());
    assert!(!last.contains(&"R".repeat(100)) && last.contains("HYA_COMPACTED_CONTEXT"));
}

#[tokio::test]
async fn kernel_rewrite_runs_before_preflight_rejects_under_threshold_provider_count() {
    struct CompactTokenizerRoute {
        inner: OpaqueReasoningRoute,
    }
    #[async_trait]
    impl Provider for CompactTokenizerRoute {
        fn id(&self) -> &str {
            "fake"
        }
        fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
            let mut caps = self.inner.capabilities(model)?;
            caps.usage_reporting = true;
            Some(caps)
        }
        fn reasoning_replay_policy(&self) -> hya_provider::ReasoningReplayPolicy {
            self.inner.reasoning_replay_policy()
        }
        async fn stream(
            &self,
            req: CompletionRequest,
            session: SessionId,
            message: MessageId,
        ) -> Result<EventStream, ProviderError> {
            let tokenizer = hya_engine::tokens::TokenAccounting::default();
            let estimated = tokenizer.estimate_with_reasoning_policy(
                &req.messages,
                hya_provider::ReasoningReplayPolicy::ProviderData,
            );
            let reported = (estimated * 68) / 100;
            let stream = self.inner.stream(req, session, message).await?;
            Ok(Box::pin(stream.map(move |event| match event {
                Ok(Event::MessageFinished {
                    session,
                    message,
                    role,
                    finish,
                    cause,
                    ..
                }) => Ok(Event::MessageFinished {
                    session,
                    message,
                    role,
                    finish,
                    tokens: Some(TokenUsage {
                        input: reported as u64,
                        output: 100,
                        ..TokenUsage::default()
                    }),
                    cause,
                }),
                other => other,
            })))
        }
    }

    let dir = support::TestDir::new("turn-preflight-rewrite");
    let file = dir.path().join("small.txt");
    tokio::fs::write(&file, "ok").await.unwrap();
    let path = file.to_string_lossy().into_owned();
    let tokenizer = hya_engine::tokens::TokenAccounting::default();
    let unit = "lorem ipsum dolor sit amet ".repeat(100);
    let blob = unit.repeat(20_000 / tokenizer.tokenizer().count_text(&unit) + 1);
    let item_tokens = tokenizer.tokenizer().count_text(&blob);
    let max_context = u32::try_from(item_tokens * 4 + item_tokens * 3 / 4).unwrap();
    let mut scripts: Vec<_> = (0..6)
        .map(|step| {
            vec![
                FakeStep::Reasoning(format!("thinking step {step}")),
                FakeStep::ToolCall {
                    name: "read".into(),
                    input: json!({ "path": path }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        })
        .collect();
    scripts.push(vec![
        FakeStep::Text("done".into()),
        FakeStep::Finish(FinishReason::Stop),
    ]);
    let route = CompactTokenizerRoute {
        inner: OpaqueReasoningRoute {
            inner: FakeProvider::scripted_turns(scripts),
            blob,
            max_context: Arc::new(AtomicU32::new(max_context)),
            requests: Arc::new(Mutex::new(Vec::new())),
        },
    };
    let engine = support::engine_with_provider(
        Arc::new(route),
        PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Allow)]),
    )
    .await
    .with_compaction(
        Arc::new(NoSummarizer),
        CompactionConfig {
            keep_recent: 1,
            ..CompactionConfig::default()
        },
    );
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(
                session,
                &support::agent_spec("build", "fake", dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert!(rewrite_count(&engine.replay(session).await.unwrap()) > 0);
}

#[tokio::test]
async fn invalid_tool_argument_json_fails_without_running() {
    use hya_provider::Protocol as _;
    let decoded = |args: &str| {
        let args = serde_json::to_string(args).unwrap();
        let mut decoder =
            hya_provider::OpenAiChatProtocol.decoder(SessionId::new(), MessageId::new(), None);
        let mut events = decoder
            .push(&format!(
                r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"id":"c","function":{{"name":"x","arguments":{args}}}}}]}},"finish_reason":"length"}}]}}"#
            ))
            .unwrap();
        events.extend(decoder.finish().unwrap());
        events
            .into_iter()
            .find_map(|e| match e {
                Event::ToolCallRequested { input, .. } => Some(input),
                _ => None,
            })
            .unwrap()
    };

    #[derive(Default)]
    struct Optional(AtomicUsize);
    #[async_trait]
    impl hya_tool::Tool for Optional {
        fn name(&self) -> &str {
            "optional"
        }
        fn schema(&self) -> hya_proto::ToolSchema {
            hya_proto::ToolSchema {
                name: hya_proto::ToolName::new("optional"),
                description: "optional args".into(),
                input_schema: json!({"type": "object", "properties": {"note": {"type": "string"}}}),
                output_schema: None,
            }
        }
        async fn execute(
            &self,
            _: &hya_tool::ToolCtx,
            _: serde_json::Value,
        ) -> Result<serde_json::Value, hya_tool::ToolError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!("ran"))
        }
    }

    let dir = support::TestDir::new("turn-bad-json");
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".into(),
                input: decoded(r#"{"filePath":"a"#),
            },
            FakeStep::Finish(FinishReason::Length),
        ],
        vec![
            FakeStep::ToolCall {
                name: "optional".into(),
                input: decoded(r#"{"note":"a"}}"#),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]));
    let tools = hya_tool_loader::builtins();
    let optional = Arc::new(Optional::default());
    tools.register(optional.clone()).unwrap();
    let engine = support::engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(provider.clone())),
        support::test_runtime(Arc::new(tools)),
        PermissionRules::new(vec![
            Rule::new(Action::Tool, "*", Mode::Allow),
            Rule::new(Action::Read, "/**", Mode::Allow),
        ]),
    )
    .await;
    let session = support::create_root_session(&engine, "general", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "call".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(
                session,
                &support::agent_spec("general", "fake", dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(optional.0.load(Ordering::SeqCst), 0);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    for (request, err_msg) in [
        (
            &requests[1],
            "EOF while parsing a string at line 1 column 14",
        ),
        (&requests[2], "trailing characters at line 1 column 13"),
    ] {
        let msg = request
            .messages
            .iter()
            .filter_map(|m| match m {
                Message::Assistant { parts, .. } => Some(parts),
                _ => None,
            })
            .flatten()
            .filter_map(|p| match p {
                Part::Tool {
                    state: ToolPartState::Error { message, .. },
                    ..
                } => Some(message.as_str()),
                _ => None,
            })
            .next_back()
            .unwrap();
        assert_eq!(
            msg,
            format!(
                "arguments are not valid JSON: {err_msg}. Nothing was run; call the tool \
                 again with its arguments as one valid JSON object."
            )
        );
    }
}

// ---------------------------------------------------------------------------
// Stream round: live deltas, snapshots, partial failure, reasoning replay
// ---------------------------------------------------------------------------

struct GatedDeltaProvider {
    release: Arc<Notify>,
    request: Arc<Mutex<Option<CompletionRequest>>>,
}

#[async_trait]
impl Provider for GatedDeltaProvider {
    fn id(&self) -> &str {
        "gated-delta"
    }
    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
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
        *self.request.lock().unwrap() = Some(req);
        let part = PartId::new();
        let events = vec![
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: "hello".into(),
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: " world".into(),
            },
            Event::TextEnd {
                session,
                message,
                part,
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        let release = Arc::clone(&self.release);
        Ok(Box::pin(stream::unfold(
            (events.into_iter(), 0usize),
            move |(mut events, index)| {
                let release = Arc::clone(&release);
                async move {
                    let event = events.next()?;
                    if index == 2 {
                        release.notified().await;
                    }
                    Some((Ok(event), (events, index + 1)))
                }
            },
        )))
    }
}

struct PartialFailureProvider;

#[async_trait]
impl Provider for PartialFailureProvider {
    fn id(&self) -> &str {
        "partial-failure"
    }
    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
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
        let reasoning = PartId::new();
        let part = PartId::new();
        Ok(Box::pin(stream::iter([
            Ok(Event::ReasoningStart {
                session,
                message,
                part: reasoning,
                reason: None,
            }),
            Ok(Event::ReasoningDelta {
                session,
                message,
                part: reasoning,
                delta: "partial thought".into(),
            }),
            Ok(Event::TextStart {
                session,
                message,
                part,
            }),
            Ok(Event::TextDelta {
                session,
                message,
                part,
                delta: "partial".into(),
            }),
            Err(ProviderError::Decode("invalid event payload".into())),
        ])))
    }
}

async fn next_text_delta(live: &mut broadcast::Receiver<Envelope>, expected: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Event::TextDelta { delta, .. } = live.recv().await.unwrap().event
                && delta == expected
            {
                return;
            }
        }
    })
    .await
    .expect("live delta");
}

#[tokio::test]
async fn stream_deltas_are_live_while_replay_commits_snapshots_once() {
    let dir = support::TestDir::new("stream-live");

    // 1. Live text deltas arrive before completion; replay commits final snapshot once.
    let release = Arc::new(Notify::new());
    let engine = Arc::new(
        support::engine_with_provider(
            Arc::new(GatedDeltaProvider {
                release: Arc::clone(&release),
                request: Arc::new(Mutex::new(None)),
            }),
            PermissionRules::default(),
        )
        .await,
    );
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "stream".into())
        .await
        .unwrap();
    let mut live = engine.bus().subscribe();
    let runner = engine.clone();
    let agent = support::agent_spec("build", "fake", dir.path());
    let turn = tokio::spawn(async move {
        runner
            .run_turn(session, &agent, CancellationToken::new())
            .await
    });
    next_text_delta(&mut live, "hello").await;
    assert!(!turn.is_finished());
    release.notify_one();
    assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Stop);
    next_text_delta(&mut live, " world").await;

    let replay = engine.store().replay(session).await.unwrap();
    let assistant_id = replay.iter().find_map(|e| match e.event {
        Event::MessageStarted {
            message,
            role: Role::Assistant,
            ..
        } => Some(message),
        _ => None,
    });
    assert_eq!(
        support::assistant_text(&engine, session).await,
        "hello world"
    );
    let durable_deltas = replay
        .iter()
        .filter(|e| matches!(e.event, Event::TextDelta { message, .. } if Some(message) == assistant_id))
        .count();
    assert!(durable_deltas <= 1);

    // 2. Reasoning deltas snapshot once without durable ReasoningDelta entries.
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted(vec![
            FakeStep::Reasoning("think one and two".into()),
            FakeStep::Finish(FinishReason::Stop),
        ])),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "reasoning".into())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &support::agent_spec("build", "fake", dir.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let replay = engine.store().replay(session).await.unwrap();
    assert_eq!(
        replay
            .iter()
            .filter(|e| matches!(
                e.event,
                Event::ReasoningStart { .. }
                    | Event::ReasoningReplace { .. }
                    | Event::ReasoningEnd { .. }
            ))
            .count(),
        3
    );
    assert!(
        !replay
            .iter()
            .any(|e| matches!(e.event, Event::ReasoningDelta { .. }))
    );

    // 3. Partial text and reasoning persist on mid-stream decode failure.
    let engine =
        support::engine_with_provider(Arc::new(PartialFailureProvider), PermissionRules::default())
            .await;
    let session =
        support::create_root_session(&engine, "build", "partial-failure", dir.path()).await;
    engine
        .admit_user_prompt(session, "fail".into())
        .await
        .unwrap();
    assert!(
        engine
            .run_turn(
                session,
                &support::agent_spec("build", "partial-failure", dir.path()),
                CancellationToken::new(),
            )
            .await
            .is_err()
    );
    let replay = engine.store().replay(session).await.unwrap();
    let proj = Projection::from_events(&replay);
    let assistant = proj
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert_eq!(assistant.finish, Some(FinishReason::Error));
    assert!(
        assistant
            .parts
            .iter()
            .any(|p| matches!(p, PartProjection::Text { text, .. } if text == "partial"))
    );
    assert!(
        assistant.parts.iter().any(
            |p| matches!(p, PartProjection::Reasoning { text, .. } if text == "partial thought")
        )
    );
}

#[tokio::test]
async fn tool_arguments_stream_live_and_late_subscribers_recover_full_input() {
    struct GatedArgsProvider {
        inner: FakeProvider,
        resume: Arc<Notify>,
    }
    #[async_trait]
    impl Provider for GatedArgsProvider {
        fn id(&self) -> &str {
            self.inner.id()
        }
        fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
            self.inner.capabilities(model)
        }
        async fn stream(
            &self,
            req: CompletionRequest,
            session: SessionId,
            message: MessageId,
        ) -> Result<EventStream, ProviderError> {
            let resume = Arc::clone(&self.resume);
            let events = self.inner.stream(req, session, message).await?;
            Ok(Box::pin(events.then(move |event| {
                let resume = Arc::clone(&resume);
                async move {
                    if matches!(&event, Ok(Event::ToolCallRequested { .. })) {
                        resume.notified().await;
                    }
                    event
                }
            })))
        }
    }

    let dir = support::TestDir::new("stream-tool-args");
    let path = dir.path().join("input.txt");
    std::fs::write(&path, "content").unwrap();
    let input = json!({ "path": path });
    let resume = Arc::new(Notify::new());
    let provider = GatedArgsProvider {
        inner: FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "read".into(),
                    input: input.clone(),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("done".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]),
        resume: Arc::clone(&resume),
    };
    let engine = Arc::new(
        support::engine_with_provider(
            Arc::new(provider),
            PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Allow)]),
        )
        .await,
    );
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "read".into())
        .await
        .unwrap();
    let mut live = engine.bus().subscribe();
    let runner = engine.clone();
    let agent = support::agent_spec("build", "fake", dir.path());
    let turn = tokio::spawn(async move {
        runner
            .run_turn(session, &agent, CancellationToken::new())
            .await
    });
    let delta = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let env = live.recv().await.unwrap();
            if matches!(env.event, Event::ToolInputDelta { .. }) {
                break env;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(delta.seq.0, 0);
    assert!(
        matches!(&delta.event, Event::ToolInputDelta { delta, .. } if delta == &input.to_string())
    );
    assert!(!turn.is_finished());

    let mut late = engine.bus().subscribe();
    let pending = engine.store().replay(session).await.unwrap();
    assert_eq!(
        pending
            .iter()
            .filter(|e| matches!(e.event, Event::ToolInputStart { .. }))
            .count(),
        1
    );
    assert!(!pending.iter().any(|e| matches!(
        e.event,
        Event::ToolInputDelta { .. } | Event::ToolCallRequested { .. }
    )));
    resume.notify_one();
    assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Stop);

    let replay = engine.store().replay(session).await.unwrap();
    assert!(
        !replay
            .iter()
            .any(|e| matches!(e.event, Event::ToolInputDelta { .. }))
    );
    let find_input = |events: &[Envelope]| {
        events.iter().find_map(|e| match &e.event {
            Event::ToolCallRequested { input, .. } => Some(input.clone()),
            _ => None,
        })
    };
    assert_eq!(find_input(&replay), Some(input.clone()));
    let mut late_events = Vec::new();
    while let Ok(env) = late.try_recv() {
        late_events.push(env);
    }
    assert_eq!(find_input(&late_events), Some(input));
}

#[tokio::test]
async fn forked_reasoning_provider_data_reaches_next_request() {
    let dir = support::TestDir::new("stream-fork-reasoning");
    let request = Arc::new(Mutex::new(None));
    let release = Arc::new(Notify::new());
    release.notify_one();
    let engine = support::engine_with_provider(
        Arc::new(GatedDeltaProvider {
            release,
            request: Arc::clone(&request),
        }),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    let provider_data = json!({
        "type": "reasoning",
        "id": "rs_123",
        "encrypted_content": "opaque",
    });
    let source = Projection {
        session: SessionProjection {
            messages: vec![MessageProjection {
                id: MessageId::new(),
                role: Role::Assistant,
                agent: None,
                model: None,
                time_created: None,
                time_updated: None,
                config_generation: None,
                last_step_start: None,
                folded_parts: 0,
                finish: Some(FinishReason::Stop),
                tokens: None,
                usage: None,
                files: Vec::new(),
                agents: Vec::new(),
                parts: vec![PartProjection::Reasoning {
                    id: PartId::new(),
                    text: "visible summary".into(),
                    reason: Some("high".into()),
                    provider_data: Some(provider_data.clone()),
                }],
                file_changes: Vec::new(),
                error: None,
                cause: None,
            }],
            ..SessionProjection::default()
        },
        ..Projection::default()
    };
    engine
        .copy_messages_to_session(session, &source, None)
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "continue".into())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &support::agent_spec("build", "fake", dir.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let captured = request.lock().unwrap();
    let sent = captured
        .as_ref()
        .unwrap()
        .messages
        .iter()
        .find_map(|m| match m {
            Message::Assistant { parts, .. } => parts.iter().find_map(|p| match p {
                Part::Reasoning { provider_data, .. } => provider_data.as_ref(),
                _ => None,
            }),
            _ => None,
        });
    assert_eq!(sent, Some(&provider_data));
}

// ---------------------------------------------------------------------------
// Single-active-turn serialization & deferred wakes
// ---------------------------------------------------------------------------

#[derive(Default)]
struct GateProvider {
    gated: Mutex<HashSet<SessionId>>,
    entered: Notify,
    release: Notify,
    streams: Mutex<HashMap<SessionId, usize>>,
    rendezvous: Mutex<Option<(HashSet<SessionId>, Arc<Barrier>)>>,
    rendezvous_met: AtomicBool,
}

impl GateProvider {
    fn gate(&self, session: SessionId) {
        self.gated.lock().unwrap().insert(session);
    }

    fn streams_for(&self, session: SessionId) -> usize {
        self.streams
            .lock()
            .unwrap()
            .get(&session)
            .copied()
            .unwrap_or(0)
    }
}

#[async_trait]
impl Provider for GateProvider {
    fn id(&self) -> &str {
        "hya"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "hya/offline").then_some(Capabilities {
            streaming_tool_calls: true,
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
        *self.streams.lock().unwrap().entry(session).or_default() += 1;
        if self.gated.lock().unwrap().remove(&session) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        let barrier = self
            .rendezvous
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(members, _)| members.contains(&session))
            .map(|(_, b)| Arc::clone(b));
        if let Some(barrier) = barrier {
            match tokio::time::timeout(Duration::from_secs(5), barrier.wait()).await {
                Ok(_) => self.rendezvous_met.store(true, Ordering::SeqCst),
                Err(_) => {
                    return Err(ProviderError::Decode(
                        "members never streamed concurrently".into(),
                    ));
                }
            }
        }
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("ok".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

async fn gate_engine() -> (Arc<SessionEngine>, AgentSpec, Arc<GateProvider>) {
    let provider = Arc::new(GateProvider::default());
    let engine = Arc::new(
        support::engine_with_provider(provider.clone(), PermissionRules::default())
            .await
            .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    let agent = support::agent_spec("hya-task", "hya/offline", "/tmp");
    (engine, agent, provider)
}

async fn make_team_session(
    engine: &SessionEngine,
    parent: Option<SessionId>,
    agent: &str,
) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new(agent),
            model: ModelRef::new("hya/offline"),
            workdir: "/tmp".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap()
}

async fn register_main(
    supervisor: &ResidentSupervisor,
    engine: &SessionEngine,
    agent: &AgentSpec,
    root: SessionId,
) {
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let agents = engine
        .agent_roster_for_binding(&binding, "hya-main")
        .unwrap();
    let resources = engine
        .agent_resource_policy_for_binding(&binding, "hya-main")
        .unwrap();
    supervisor
        .ensure_main(
            root,
            agent.clone(),
            (binding, agents, resources),
            None,
            None,
        )
        .await
        .unwrap();
}

fn max_open_assistant(events: &[Envelope], session: SessionId) -> usize {
    let mut open = HashSet::new();
    let mut max = 0;
    for env in events {
        match &env.event {
            Event::MessageStarted {
                session: s,
                message,
                role: Role::Assistant,
                ..
            } if *s == session => {
                open.insert(*message);
                max = max.max(open.len());
            }
            Event::MessageFinished {
                session: s,
                message,
                role: Role::Assistant,
                ..
            } if *s == session => {
                open.remove(message);
            }
            _ => {}
        }
    }
    max
}

async fn has_quiesced(engine: &SessionEngine, root: SessionId) -> bool {
    engine
        .read_projection(root)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .any(|m| {
            m.role == Role::User
                && m.parts
                    .iter()
                    .any(|p| matches!(p, PartProjection::Text { text, .. } if text.contains("TEAM QUIESCED")))
        })
}

async fn assistant_count(engine: &SessionEngine, session: SessionId, finished: bool) -> usize {
    engine
        .replay(session)
        .await
        .unwrap()
        .iter()
        .filter(|e| {
            if finished {
                matches!(&e.event, Event::MessageFinished { session: s, role: Role::Assistant, .. } if *s == session)
            } else {
                matches!(&e.event, Event::MessageStarted { session: s, role: Role::Assistant, .. } if *s == session)
            }
        })
        .count()
}

#[tokio::test]
async fn quiescence_and_mail_queue_behind_leads_active_turn() {
    for trigger_mail in [false, true] {
        let (engine, agent, provider) = gate_engine().await;
        let root = make_team_session(&engine, None, "hya-main").await;
        let supervisor = ResidentSupervisor::start(engine.clone());
        provider.gate(root);
        engine.admit_user_prompt(root, "lead".into()).await.unwrap();
        let lead = {
            let e = engine.clone();
            let a = agent.clone();
            tokio::spawn(async move { e.run_turn(root, &a, CancellationToken::new()).await })
        };
        tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
            .await
            .unwrap();

        register_main(&supervisor, &engine, &agent, root).await;
        let scout = make_team_session(&engine, Some(root), "hya-task").await;
        supervisor
            .register_existing_resident(
                root,
                scout,
                "scout".into(),
                agent.clone(),
                (!trigger_mail).then(|| "scout the repo".into()),
            )
            .await
            .unwrap();
        if trigger_mail {
            engine
                .mail_send(
                    scout,
                    MailEndpoint::Handle("main".into()),
                    MailKind::Message,
                    "findings".into(),
                )
                .await
                .unwrap();
        } else {
            assert!(
                support::poll_until(Duration::from_secs(5), || async {
                    provider.streams_for(scout) == 1
                        && hya_tool::extension::team_projection(
                            &engine.read_projection(root).await.unwrap(),
                        )
                        .roster
                        .get("main/scout")
                        .map(|e| e.status)
                            == Some(RosterStatus::Idle)
                })
                .await
            );
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(assistant_count(&engine, root, false).await, 1);
        assert!(!has_quiesced(&engine, root).await);

        provider.release.notify_one();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), lead)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            FinishReason::Stop
        );
        assert!(
            support::poll_until(Duration::from_secs(5), || async {
                assistant_count(&engine, root, true).await == 2
                    && (trigger_mail || has_quiesced(&engine, root).await)
            })
            .await
        );
        let events = engine.replay(root).await.unwrap();
        assert_eq!(max_open_assistant(&events, root), 1);
        if !trigger_mail {
            let first_finish = events
                .iter()
                .position(|e| matches!(&e.event, Event::MessageFinished { session, role: Role::Assistant, .. } if *session == root))
                .unwrap();
            let notice = events
                .iter()
                .position(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta.contains("TEAM QUIESCED")))
                .unwrap();
            assert!(notice > first_finish);
        }
    }
}

#[tokio::test]
async fn consumed_results_suppress_redundant_synthesis_while_unseen_mail_wakes() {
    #[derive(Clone, Copy)]
    enum ModeCase {
        ChannelRead,
        Wait,
        LateMailAfterRead,
        OtherChannelUnseen,
    }
    for case in [
        ModeCase::ChannelRead,
        ModeCase::Wait,
        ModeCase::LateMailAfterRead,
        ModeCase::OtherChannelUnseen,
    ] {
        let (engine, agent, provider) = gate_engine().await;
        let root = make_team_session(&engine, None, "hya-main").await;
        let supervisor = ResidentSupervisor::start(engine.clone());
        provider.gate(root);
        engine.admit_user_prompt(root, "lead".into()).await.unwrap();
        let lead = {
            let e = engine.clone();
            let a = agent.clone();
            tokio::spawn(async move { e.run_turn(root, &a, CancellationToken::new()).await })
        };
        tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
            .await
            .unwrap();

        register_main(&supervisor, &engine, &agent, root).await;
        let scout = make_team_session(&engine, Some(root), "hya-task").await;
        supervisor
            .register_existing_resident(root, scout, "scout".into(), agent.clone(), None)
            .await
            .unwrap();
        engine
            .mail_send(
                scout,
                MailEndpoint::Handle("^parent".into()),
                MailKind::Message,
                "findings ready".into(),
            )
            .await
            .unwrap();
        if matches!(case, ModeCase::OtherChannelUnseen) {
            let other = make_team_session(&engine, Some(root), "hya-task").await;
            supervisor
                .register_existing_resident(root, other, "other".into(), agent.clone(), None)
                .await
                .unwrap();
            engine
                .mail_send(
                    other,
                    MailEndpoint::Handle("^parent".into()),
                    MailKind::Message,
                    "other report".into(),
                )
                .await
                .unwrap();
        }
        assert!(
            support::poll_until(Duration::from_secs(2), || async {
                hya_engine::TurnBoundaryObserver::mail_mark(supervisor.as_ref(), root).unwrap_or(0)
                    >= if matches!(case, ModeCase::OtherChannelUnseen) {
                        2
                    } else {
                        1
                    }
            })
            .await
        );

        match case {
            ModeCase::Wait => {
                let (lifecycle, rx) = hya_tool::LifecyclePlane::new();
                let svc = tokio::spawn(hya_engine::run_lifecycle_service(
                    engine.clone(),
                    supervisor.clone(),
                    rx,
                    CancellationToken::new(),
                ));
                let out = lifecycle
                    .for_session(root)
                    .wait(
                        hya_tool::WaitSpec::parse(&json!({"timeout_secs": 0}), true).unwrap(),
                        &CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                assert_eq!(out.mail.len(), 1);
                svc.abort();
            }
            _ => {
                let read = engine
                    .read_channel_history(None, root, "scout", None)
                    .await
                    .unwrap();
                assert_eq!(read.messages[0].1, "findings ready");
            }
        }
        if matches!(case, ModeCase::LateMailAfterRead) {
            engine
                .mail_send(
                    scout,
                    MailEndpoint::Handle("^parent".into()),
                    MailKind::Message,
                    "late".into(),
                )
                .await
                .unwrap();
            assert!(
                support::poll_until(Duration::from_secs(2), || async {
                    hya_engine::TurnBoundaryObserver::mail_mark(supervisor.as_ref(), root)
                        .unwrap_or(0)
                        >= 2
                })
                .await
            );
        }

        provider.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), lead)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if matches!(
            case,
            ModeCase::LateMailAfterRead | ModeCase::OtherChannelUnseen
        ) {
            assert!(
                support::poll_until(Duration::from_secs(5), || async {
                    provider.streams_for(root) == 2
                })
                .await
            );
        } else {
            tokio::time::sleep(Duration::from_millis(40)).await;
            assert!(provider.streams_for(root) <= 1 && !has_quiesced(&engine, root).await);
        }
    }

    // Outgoing directive on another DM does not retain synthesis once incoming mail is read.
    let (engine, agent, provider) = gate_engine().await;
    let root = make_team_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lease = engine.try_begin_turn(root).unwrap();
    register_main(&supervisor, &engine, &agent, root).await;
    let scout = make_team_session(&engine, Some(root), "hya-task").await;
    let other = make_team_session(&engine, Some(root), "hya-task").await;
    supervisor
        .register_existing_resident(root, scout, "scout".into(), agent.clone(), None)
        .await
        .unwrap();
    supervisor
        .register_existing_resident(root, other, "other".into(), agent.clone(), None)
        .await
        .unwrap();
    let other_dm =
        hya_tool::extension::team_projection(&engine.read_projection(root).await.unwrap())
            .channels
            .iter()
            .find(|(_, c)| c.kind == hya_proto::ChannelKind::Dm && c.members.contains("main/other"))
            .unwrap()
            .0
            .clone();
    engine
        .mail_send(
            root,
            MailEndpoint::Channel(other_dm),
            MailKind::Message,
            "outgoing".into(),
        )
        .await
        .unwrap();
    engine
        .mail_send(
            scout,
            MailEndpoint::Handle("^parent".into()),
            MailKind::Message,
            "incoming".into(),
        )
        .await
        .unwrap();
    assert!(
        support::poll_until(Duration::from_secs(2), || async {
            hya_engine::TurnBoundaryObserver::mail_mark(supervisor.as_ref(), root).unwrap_or(0) >= 2
        })
        .await
    );
    engine
        .read_channel_history(None, root, "scout", None)
        .await
        .unwrap();
    drop(lease);
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(!has_quiesced(&engine, root).await && provider.streams_for(root) == 0);
    supervisor
        .drain(hya_proto::FinishCause::Shutdown, Duration::from_secs(2))
        .await;
}

#[tokio::test]
async fn concurrent_turns_serialize_queued_cancel_claim_error_and_sibling_concurrency() {
    // 1. Duplicate try_begin_turn is typed TurnAlreadyActive; distinct sessions stay independent.
    let (engine, agent, provider) = gate_engine().await;
    let root = make_team_session(&engine, None, "hya-main").await;
    let other = make_team_session(&engine, None, "hya-main").await;
    let lease = engine.try_begin_turn(root).unwrap();
    assert!(
        matches!(engine.try_begin_turn(root), Err(CoreError::TurnAlreadyActive { session }) if session == root)
    );
    drop(engine.try_begin_turn(other).unwrap());
    drop(lease);
    assert!(!engine.turn_active(root));

    // 2. Concurrent run_turn serializes; a queued turn can be cancelled before starting.
    provider.gate(root);
    engine
        .admit_user_prompt(root, "first".into())
        .await
        .unwrap();
    let first = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move { e.run_turn(root, &a, CancellationToken::new()).await })
    };
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .unwrap();
    let second = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move { e.run_turn(root, &a, CancellationToken::new()).await })
    };
    let cancel = CancellationToken::new();
    let cancelled = {
        let (e, a, c) = (engine.clone(), agent.clone(), cancel.clone());
        tokio::spawn(async move { e.run_turn(root, &a, c).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(provider.streams_for(root), 1);
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), cancelled)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        FinishReason::Cancelled
    );
    provider.release.notify_one();
    for task in [first, second] {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            FinishReason::Stop
        );
    }
    let events = engine.replay(root).await.unwrap();
    assert_eq!(assistant_count(&engine, root, false).await, 2);
    assert_eq!(max_open_assistant(&events, root), 1);

    // 3. Separate member sessions stream concurrently.
    let (engine, agent, provider) = gate_engine().await;
    let root = make_team_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let a = make_team_session(&engine, Some(root), "hya-task").await;
    let b = make_team_session(&engine, Some(root), "hya-task").await;
    *provider.rendezvous.lock().unwrap() = Some((HashSet::from([a, b]), Arc::new(Barrier::new(2))));
    for (s, h) in [(a, "worker-a"), (b, "worker-b")] {
        supervisor
            .register_existing_resident(root, s, h.into(), agent.clone(), Some("work".into()))
            .await
            .unwrap();
    }
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            let team =
                hya_tool::extension::team_projection(&engine.read_projection(root).await.unwrap());
            team.roster.get("main/worker-a").map(|e| e.status) == Some(RosterStatus::Idle)
                && team.roster.get("main/worker-b").map(|e| e.status) == Some(RosterStatus::Idle)
        })
        .await
    );
    assert!(provider.rendezvous_met.load(Ordering::SeqCst));
}

#[tokio::test]
async fn todo_list_survives_restart_and_builds_on_prior_state() {
    let dir = support::TestDir::new("turn-todo");
    let store = SessionStore::connect_memory().await.unwrap();
    let call_todo = |input: serde_json::Value| {
        vec![
            vec![
                FakeStep::ToolCall {
                    name: "todo".into(),
                    input,
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("ok".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]
    };
    let make_engine = |script: Vec<Vec<FakeStep>>| {
        let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
        SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(script)))),
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm,
            EventBus::default(),
        )
    };
    async fn run(engine: &SessionEngine, session: SessionId, workdir: &Path) {
        let agent = support::agent_spec("build", "fake", workdir);
        engine
            .admit_user_prompt(session, "go".into())
            .await
            .unwrap();
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap();
    }

    let first = make_engine(call_todo(json!({"ops": [
        {"op": "add", "content": "first"},
        {"op": "add", "content": "second"}
    ]})));
    let session = support::create_root_session(&first, "build", "fake", dir.path()).await;
    run(&first, session, dir.path()).await;
    drop(first);

    let second = make_engine(call_todo(
        json!({"ops": [{"op": "status", "id": "2", "status": "completed"}]}),
    ));
    run(&second, session, dir.path()).await;

    let proj = store.read_projection(session).await.unwrap();
    assert_eq!(
        proj.session.todos,
        Some(vec![
            TodoItem {
                id: "1".into(),
                content: "first".into(),
                status: TodoStatus::Pending
            },
            TodoItem {
                id: "2".into(),
                content: "second".into(),
                status: TodoStatus::Completed
            },
        ])
    );
    assert_eq!(
        store
            .replay(session)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(e.event, Event::TodosUpdated { .. }))
            .count(),
        2
    );
}
