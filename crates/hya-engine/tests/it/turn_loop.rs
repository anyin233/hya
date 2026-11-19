//! Integration tests for `hya-engine`: turn loop.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use hya_engine::{
    AgentSpec, CompactionConfig, CoreError, CreateSession, EventBus, SessionEngine, Summarizer,
    TokenAccountingMode, TokenSource,
};
use hya_proto::Event;
use hya_proto::{
    AgentName, FinishReason, Message, ModelRef, PartProjection, Role, TokenUsage, ToolPartState,
};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("hya-core-test-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn text_tool_result_text_round_trip() {
    let dir = tempdir();
    let file = dir.join("foo.txt");
    tokio::fs::write(&file, "42 lines").await.unwrap();
    let path = file.to_string_lossy().into_owned();
    let captured_read_input = json!({
        "filePath": path.clone(),
        "path": path.clone(),
        "offset": 0,
        "limit": 2000
    });

    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("I'll read it".to_string()),
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: captured_read_input.clone(),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("The file says 42 lines".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);

    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    engine
        .admit_user_prompt(session, "read foo.txt".to_string())
        .await
        .unwrap();

    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "you are build".to_string(),
        workdir: dir.clone(),
        reasoning: None,
    };
    let finish = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let events = engine.store().replay(session).await.unwrap();
    let correlated_read_result = events.iter().any(|result| {
        let Event::ToolResult {
            call: result_call, ..
        } = &result.event
        else {
            return false;
        };
        events.iter().any(|requested| {
            matches!(
                &requested.event,
                Event::ToolCallRequested {
                    call: requested_call,
                    name,
                    input,
                    ..
                } if requested_call == result_call
                    && name.as_str() == "read"
                    && input == &captured_read_input
            )
        })
    });
    assert!(
        correlated_read_result,
        "expected the captured Read call to complete with a correlated ToolResult; events={events:?}"
    );

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant message");

    let text_index = assistant
        .parts
        .iter()
        .position(
            |part| matches!(part, PartProjection::Text { text, .. } if text == "I'll read it"),
        )
        .expect("text before read call");
    let tool_index = assistant
        .parts
        .iter()
        .position(|part| matches!(part, PartProjection::Tool { .. }))
        .expect("read call");
    assert!(
        text_index < tool_index,
        "the provider's text must precede its tool call in durable replay"
    );

    let completed_read = assistant.parts.iter().any(|p| {
        matches!(
            p,
            PartProjection::Tool { name, state: ToolPartState::Completed { output, .. }, .. }
                if name.as_str() == "read" && output["content"] == "42 lines"
        )
    });
    assert!(completed_read, "expected a completed read tool part");

    let final_text = assistant
        .parts
        .iter()
        .any(|p| matches!(p, PartProjection::Text { text, .. } if text.contains("42 lines")));
    assert!(final_text, "expected final assistant text");
}

#[tokio::test]
async fn turn_continues_past_twenty_five_tool_rounds() {
    let dir = tempdir();
    let mut scripts = (0..26)
        .map(|_| {
            vec![
                FakeStep::ToolCall {
                    name: "unknown".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        })
        .collect::<Vec<_>>();
    let final_text = "continued after twenty-five tool rounds";
    scripts.push(vec![
        FakeStep::Text(final_text.to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]);

    let provider = FakeProvider::scripted_turns(scripts);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };

    let finish = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .expect("assistant message");
    assert!(
        assistant
            .parts
            .iter()
            .any(|part| matches!(part, PartProjection::Text { text, .. } if text == final_text)),
        "expected final response after the tool rounds"
    );
}

#[tokio::test]
async fn cancelled_turn_finishes_cancelled() {
    let dir = tempdir();
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("hi".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    let finish = engine.run_turn(session, &agent, cancel).await.unwrap();
    assert_eq!(finish, FinishReason::Cancelled);
}

#[tokio::test]
async fn provider_usage_is_recorded_on_assistant_message_projection() {
    let dir = tempdir();
    let usage = TokenUsage {
        input: 11,
        output: 3,
        reasoning: 2,
        cache_read: 5,
        cache_write: 0,
        reasoning_unknown: false,
    };
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("hi".to_string()),
        FakeStep::Usage(usage),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };

    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .expect("assistant message");

    assert_eq!(assistant.tokens, Some(usage));
}

/// Context reduction never calls the summarizer; it only writes terminal
/// handoffs, which these tests do not reach.
struct NoSummarizer;

#[async_trait::async_trait]
impl Summarizer for NoSummarizer {
    async fn summarize(
        &self,
        _messages: &[Message],
        _options: hya_engine::SummarizeOptions,
    ) -> Result<String, CoreError> {
        Err(CoreError::Invalid("no summarizer in this test".to_string()))
    }
}

/// How many kernel rewrites `envelopes` record.
fn rewrites(envelopes: &[hya_proto::Envelope]) -> usize {
    envelopes
        .iter()
        .filter(|e| matches!(e.event, Event::ContextCompacted { .. }))
        .count()
}

/// Full mode reduces on the kernel's trigger: once the request is over 80% of
/// the window and `shake` has nothing to spill, the kernel's own Rewrite
/// removes the earlier turns behind a marker that repeats their user messages
/// verbatim, and the next turn, back under the threshold, does not rewrite
/// again.
#[tokio::test]
async fn kernel_rewrite_persists_and_is_not_repeated_next_round() {
    let dir = tempdir();
    // A 40k window (threshold 32,000): turn 1's ~40k-token answer puts turn 2
    // over it; after the rewrite the request is the fixed prompt and tools.
    let provider = Arc::new(
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::Text("detail across the earlier turn ".repeat(7_000)),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::Text("second".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::Text("third".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ])
        .with_max_context(40_000),
    );
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    for prompt in ["ORIGINAL_PROMPT_0", "NEXT_PROMPT_1"] {
        engine
            .admit_user_prompt(session, prompt.to_string())
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
            hya_proto::Event::ContextCompacted {
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
    assert_eq!(records.len(), 1, "exactly one rewrite: {records:?}");
    let (marker_id, strategy, from_message, to_message, folded_count, threshold) = records[0];
    assert_eq!(strategy, hya_proto::CompactionStrategy::Rewrite);
    assert_eq!(threshold, 32_000, "records 80% of the window");

    // The recorded output message is the marker, repeating the user message.
    let projection = hya_proto::Projection::from_events(&envelopes);
    let marker = projection
        .session
        .messages
        .iter()
        .find(|m| m.id == marker_id)
        .expect("ContextCompacted.message must point at a real message");
    assert_eq!(marker.role, Role::System);
    let marker_text: String = marker
        .parts
        .iter()
        .filter_map(|p| match p {
            PartProjection::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        marker_text.starts_with("HYA_COMPACTED_CONTEXT"),
        "{marker_text}"
    );
    assert!(marker_text.contains("ORIGINAL_PROMPT_0"), "{marker_text}");

    // The recorded range covers exactly `folded_count` messages.
    let ids: Vec<_> = projection.session.messages.iter().map(|m| m.id).collect();
    let from_idx = ids.iter().position(|id| *id == from_message).unwrap();
    let to_idx = ids.iter().position(|id| *id == to_message).unwrap();
    assert_eq!(to_idx - from_idx + 1, folded_count as usize);

    // A third turn, under the threshold, keeps the context as it is.
    engine
        .admit_user_prompt(session, "THIRD_PROMPT".to_string())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    let rewrites = engine
        .replay(session)
        .await
        .unwrap()
        .iter()
        .filter(|e| matches!(e.event, Event::ContextCompacted { .. }))
        .count();
    assert_eq!(rewrites, 1, "the third turn must not rewrite again");
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests[1..] {
        assert!(!format!("{request:?}").contains("detail across the earlier turn"));
    }
}

#[tokio::test]
async fn provider_error_still_finishes_the_assistant_message() {
    let dir = tempdir();
    let router = Arc::new(ProviderRouter::new());
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("no-such-model"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "hello".to_string())
        .await
        .unwrap();

    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("no-such-model"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    let result = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await;
    assert!(result.is_err(), "an unresolved model must surface an error");

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant message exists");
    assert_eq!(
        assistant.finish,
        Some(FinishReason::Error),
        "the assistant message must be terminally finished on provider error so UI clients never hang"
    );
}

/// E3: eviction alone must be enough to skip the kernel's rewrite, and it must
/// not touch the event log — the full tool output stays recoverable offline.
///
/// A 50k window (threshold 40,000): the line-numbered Read result serializes
/// to ~65KB of JSON (~30,100 tokens), so turn 1 (~37,000 with the fixed
/// request material) stays under it; turn 2's ~6,600-token prompt pushes it
/// over, and evicting turn 1's now-stale output drops it far below. Turn 1
/// over the threshold would have the kernel rewrite its read step away.
///
/// Spans two turns deliberately: within one turn every tool part lands in the
/// same assistant message, which sits inside `keep_recent`. Eviction is a
/// cross-turn reduction.
#[tokio::test]
async fn tool_output_eviction_avoids_a_rewrite_and_preserves_the_log() {
    let dir = tempdir();
    let big_file = dir.join("big.txt");
    let big = format!("{}\n", "R".repeat(100)).repeat(200);
    tokio::fs::write(&big_file, &big).await.unwrap();
    let path = big_file.to_string_lossy().into_owned();

    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "path": path }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("read it".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("second turn".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    // The rendered read fits on its own. The next turn's prompt puts the
    // request above the 20,800-token spill threshold.
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider.with_max_context(26_000))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(NoSummarizer),
        CompactionConfig {
            keep_recent: 1,
            ..CompactionConfig::default()
        },
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };

    // Turn 1 produces a large tool output.
    engine
        .admit_user_prompt(session, "read the big file".to_string())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    // Turn 2 sees turn 1's tool output as stale and should evict it.
    engine
        .admit_user_prompt(session, "now summarize ".repeat(3_000))
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    let envelopes = engine.replay(session).await.unwrap();
    let evicted: Vec<_> = envelopes
        .iter()
        .filter_map(|e| match &e.event {
            hya_proto::Event::ContextEvicted {
                evicted_parts,
                tokens_before,
                tokens_after,
                ..
            } => Some((*evicted_parts, *tokens_before, *tokens_after)),
            _ => None,
        })
        .collect();
    assert!(
        !evicted.is_empty(),
        "a stale large tool output over the threshold must be evicted"
    );
    let (parts, before, after) = evicted[0];
    assert!(parts > 0);
    assert!(after < before, "eviction must reduce the token count");
    assert_eq!(
        rewrites(&envelopes),
        0,
        "eviction alone was enough; the kernel must not have rewritten"
    );

    // The log is untouched: the full tool output is still recoverable.
    let projection = hya_proto::Projection::from_events(&envelopes);
    let logged = projection.session.messages.iter().any(|m| {
        m.parts.iter().any(|p| {
            matches!(
                p,
                PartProjection::Tool { state: ToolPartState::Completed { output, .. }, .. }
                    if output.to_string().contains("RRRRRRRRRR")
            )
        })
    });
    assert!(
        logged,
        "eviction is request-local; the event log must keep full fidelity"
    );
}

/// Invariant 1: the request is a pure function of the log. Turn 2 evicts
/// turn 1's stale output to an artifact handle and answers with nothing, so
/// a request after a restart sees the same transcript. A new engine over the
/// same SQLite file must send the same bytes, eviction notice and handle
/// included, without evicting again.
#[tokio::test]
async fn request_is_identical_after_restart() {
    let dir = tempdir();
    let big_file = dir.join("big.txt");
    tokio::fs::write(&big_file, format!("{}\n", "R".repeat(100)).repeat(200))
        .await
        .unwrap();
    // Sized as in the eviction test above: turn 2 crosses the threshold and
    // evicting turn 1's output brings it far below.
    let provider = Arc::new(
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "read".to_string(),
                    input: json!({ "path": big_file.to_string_lossy() }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("read it".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![FakeStep::Finish(FinishReason::Stop)],
        ])
        .with_max_context(26_000),
    );
    let db = dir.join("hya.db").to_string_lossy().into_owned();
    let engine_over = |store: SessionStore| {
        let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Read,
            "/**",
            Mode::Allow,
        )]));
        SessionEngine::new(
            store,
            Arc::new(ProviderRouter::new().with(provider.clone())),
            support::test_runtime(Arc::new(ToolRegistry::builtins())),
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
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir.clone(),
        reasoning: None,
    };

    let engine = engine_over(SessionStore::connect(&db).await.unwrap());
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    for prompt in [
        "read the big file".to_string(),
        "now summarize ".repeat(3_000),
    ] {
        engine.admit_user_prompt(session, prompt).await.unwrap();
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap();
    }
    let evictions = |envelopes: &[hya_proto::Envelope]| {
        envelopes
            .iter()
            .filter(|e| matches!(e.event, Event::ContextEvicted { .. }))
            .count()
    };
    assert_eq!(evictions(&engine.replay(session).await.unwrap()), 1);
    drop(engine);

    let engine = engine_over(SessionStore::connect(&db).await.unwrap());
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        evictions(&engine.replay(session).await.unwrap()),
        1,
        "the restarted engine must not evict again"
    );
    let requests = provider.requests();
    let [.., before, after] = requests.as_slice() else {
        panic!("expected a request before and after the restart");
    };
    let before = format!("{before:?}");
    assert!(
        before.contains("artifact://"),
        "the request sends the handle"
    );
    assert_eq!(before, format!("{after:?}"));
}

/// Eviction is sticky: once a stale output was evicted, later rounds re-apply
/// the same placeholder before measuring the request. A later turn that is
/// under the threshold with that placeholder must neither re-run the ladder
/// (no second `ContextEvicted`) nor escalate to the summarizer.
#[tokio::test]
async fn evicted_output_stays_evicted_on_later_turns_without_escalating() {
    let dir = tempdir();
    let big_file = dir.join("big.txt");
    let big = format!("{}\n", "R".repeat(100)).repeat(200);
    tokio::fs::write(&big_file, &big).await.unwrap();
    let path = big_file.to_string_lossy().into_owned();

    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "path": path }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("read it".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("second turn".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("third turn".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider.with_max_context(26_000))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(NoSummarizer),
        CompactionConfig {
            keep_recent: 1,
            ..CompactionConfig::default()
        },
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };

    for prompt in [
        "read the big file".to_string(),
        "now summarize ".repeat(3_000),
        "and once more".to_string(),
    ] {
        engine.admit_user_prompt(session, prompt).await.unwrap();
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap();
    }

    let envelopes = engine.replay(session).await.unwrap();
    let evictions = envelopes
        .iter()
        .filter(|e| matches!(e.event, hya_proto::Event::ContextEvicted { .. }))
        .count();
    assert_eq!(
        evictions, 1,
        "the third turn must re-apply the sticky placeholder, not evict again"
    );
    assert_eq!(
        rewrites(&envelopes),
        0,
        "a re-applied eviction keeps the request under the threshold"
    );
}

#[tokio::test]
async fn context_status_reports_round_occupancy_and_accounting_mode() {
    let dir = tempdir();
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("done".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_compaction(Arc::new(NoSummarizer), CompactionConfig::default());

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "hello".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "you are build".to_string(),
        workdir: dir.clone(),
        reasoning: None,
    };
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    let envelopes = engine.store().replay(session).await.unwrap();
    let statuses: Vec<_> = envelopes
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ContextStatus {
                tokens,
                source,
                mode,
                threshold,
                ..
            } => Some((*tokens, *source, *mode, *threshold)),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses.len(),
        1,
        "one streaming round must report window occupancy exactly once"
    );
    let (tokens, source, mode, threshold) = statuses[0];
    assert!(tokens > 0, "occupancy must be a positive estimate");
    assert_eq!(mode, TokenAccountingMode::Auto, "engine default is Auto");
    assert_eq!(
        source,
        TokenSource::Estimate,
        "a round with no reported usage must be estimated, not provider-anchored"
    );
    assert_eq!(
        threshold, 160_000,
        "fake route advertises a 200k window: the kernel's 80%"
    );

    let projection = engine.store().read_projection(session).await.unwrap();
    let folded = projection
        .session
        .context_status
        .expect("projection folds the latest ContextStatus");
    assert_eq!(folded.tokens, tokens);
    assert_eq!(folded.source, source);
    assert_eq!(folded.mode, mode);
    assert_eq!(folded.threshold, threshold);
}

/// A long agentic turn is ONE assistant message: its stale tool outputs must
/// be spilled mid-turn once the request crosses the threshold, keeping the most
/// recent `keep_recent` tool outputs intact (previously nothing compacted
/// until the turn ended, because every rung required a foldable message range).
#[tokio::test]
async fn stale_tool_outputs_spill_inside_one_long_turn() {
    let dir = tempdir();
    let big_file = dir.join("big.txt");
    let big = format!("{}\n", "R".repeat(100)).repeat(200);
    tokio::fs::write(&big_file, &big).await.unwrap();
    let path = big_file.to_string_lossy().into_owned();
    let read = || FakeStep::ToolCall {
        name: "read".to_string(),
        input: json!({ "path": path }),
    };

    // Three read rounds and a final answer share one turn. At a 24,000-token
    // threshold, the fixed overhead (~7.6k) and one read's rendered text
    // (~10k) fit. Two reads cross the threshold until the stale one is spilled.
    let provider = FakeProvider::scripted_turns(vec![
        vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
        vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
        vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider.with_max_context(30_000))));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(NoSummarizer),
        CompactionConfig {
            keep_recent: 1,
            ..CompactionConfig::default()
        },
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    engine
        .admit_user_prompt(session, "read the big file three times".to_string())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    let envelopes = engine.replay(session).await.unwrap();
    let assistant_messages = hya_proto::Projection::from_events(&envelopes)
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .count();
    assert_eq!(
        assistant_messages, 1,
        "the whole turn is one assistant message"
    );
    let evictions: Vec<_> = envelopes
        .iter()
        .filter_map(|e| match &e.event {
            hya_proto::Event::ContextEvicted {
                evicted_parts,
                tokens_before,
                tokens_after,
                ..
            } => Some((*evicted_parts, *tokens_before, *tokens_after)),
            _ => None,
        })
        .collect();
    assert!(
        !evictions.is_empty(),
        "stale tool outputs inside the running turn must be spilled"
    );
    for (parts, before, after) in &evictions {
        assert!(*parts > 0);
        assert!(after < before);
    }
    assert_eq!(rewrites(&envelopes), 0, "spilling was enough");
}

/// Records every request; replays reasoning as an opaque per-part provider
/// item of `blob`'s size (Responses-shaped); the advertised window can change
/// between turns.
struct OpaqueReasoningRoute {
    inner: FakeProvider,
    blob: String,
    max_context: Arc<std::sync::atomic::AtomicU32>,
    requests: Arc<Mutex<Vec<hya_provider::CompletionRequest>>>,
}

#[async_trait::async_trait]
impl hya_provider::Provider for OpaqueReasoningRoute {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<hya_provider::Capabilities> {
        Some(hya_provider::Capabilities {
            streaming_tool_calls: true,
            max_context: self.max_context.load(Ordering::SeqCst),
            ..hya_provider::Capabilities::default()
        })
    }

    fn reasoning_replay_policy(&self) -> hya_provider::ReasoningReplayPolicy {
        hya_provider::ReasoningReplayPolicy::ProviderData
    }

    async fn stream(
        &self,
        req: hya_provider::CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<hya_provider::EventStream, hya_provider::ProviderError> {
        use futures::StreamExt as _;
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

/// When `shake` spills but the request is still over the threshold, the
/// kernel rewrites in the same `before_round`, before the round is sent.
#[tokio::test]
async fn a_spill_that_is_not_enough_is_followed_by_the_kernel_rewrite() {
    let dir = tempdir();
    let big_file = dir.join("big.txt");
    let big = format!("{}\n", "R".repeat(100)).repeat(200);
    tokio::fs::write(&big_file, &big).await.unwrap();
    let path = big_file.to_string_lossy().into_owned();
    let read = || FakeStep::ToolCall {
        name: "read".to_string(),
        input: json!({ "path": path }),
    };
    let tokenizer = hya_engine::tokens::TokenAccounting::default();
    let unit = "earlier context ".repeat(100);
    let prompt = unit.repeat(40_000 / tokenizer.tokenizer().count_text(&unit) + 1);

    let requests = Arc::new(Mutex::new(Vec::new()));
    // Turn 1 runs under a window it cannot fill; turn 2 under one where
    // spilling the oldest read is not enough.
    let window = Arc::new(std::sync::atomic::AtomicU32::new(10_000_000));
    let route = OpaqueReasoningRoute {
        inner: FakeProvider::scripted_turns(vec![
            vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
            vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
            vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
            vec![
                FakeStep::Text("read".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::Text("done".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]),
        blob: String::new(),
        max_context: Arc::clone(&window),
        requests: Arc::clone(&requests),
    };
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(route))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(NoSummarizer),
        CompactionConfig {
            keep_recent: 2,
            ..CompactionConfig::default()
        },
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    engine.admit_user_prompt(session, prompt).await.unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    // The prompt, fixed overhead and two surviving reads total about 68k
    // after the oldest read is spilled, still above the 64,000-token threshold.
    window.store(80_000, Ordering::SeqCst);
    engine
        .admit_user_prompt(session, "go on".to_string())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    let envelopes = engine.replay(session).await.unwrap();
    let reductions: Vec<_> = envelopes
        .iter()
        .filter_map(|e| match e.event {
            Event::ContextEvicted { .. } => Some("spill"),
            Event::ContextCompacted {
                strategy: hya_proto::CompactionStrategy::Rewrite,
                ..
            } => Some("rewrite"),
            Event::StepStarted { .. } => Some("step"),
            _ => None,
        })
        .collect();
    assert_eq!(
        reductions,
        ["step", "step", "step", "step", "spill", "rewrite", "step"],
        "turn 2 spilled, then the kernel rewrote, before its round"
    );

    let requests = requests.lock().unwrap();
    let last = format!("{:?}", requests.last().unwrap());
    assert!(!last.contains(&"R".repeat(100)), "turn 1 left the request");
    assert!(last.contains("HYA_COMPACTED_CONTEXT"));
}

/// When a provider's reported prompt size is plausible (`~0.70` of the local
/// estimate, e.g. dense JSON/reasoning tokenization) and still sits below the
/// threshold while the local estimate has already crossed `max_context`, the
/// kernel must still rewrite before `check_request_context` refuses the
/// request: its trigger counts the local estimate too.
#[tokio::test]
async fn kernel_rewrite_runs_before_preflight_rejects_an_under_threshold_provider_count() {
    struct CompactTokenizerRoute {
        inner: OpaqueReasoningRoute,
    }

    #[async_trait::async_trait]
    impl hya_provider::Provider for CompactTokenizerRoute {
        fn id(&self) -> &str {
            "fake"
        }

        fn capabilities(&self, model: &ModelRef) -> Option<hya_provider::Capabilities> {
            let mut caps = self.inner.capabilities(model)?;
            caps.usage_reporting = true;
            Some(caps)
        }

        fn reasoning_replay_policy(&self) -> hya_provider::ReasoningReplayPolicy {
            self.inner.reasoning_replay_policy()
        }

        async fn stream(
            &self,
            req: hya_provider::CompletionRequest,
            session: hya_proto::SessionId,
            message: hya_proto::MessageId,
        ) -> Result<hya_provider::EventStream, hya_provider::ProviderError> {
            use futures::StreamExt as _;
            let tokenizer = hya_engine::tokens::TokenAccounting::default();
            let estimated = tokenizer.estimate_with_reasoning_policy(
                &req.messages,
                hya_provider::ReasoningReplayPolicy::ProviderData,
            );
            // Plausible (`0.68 >= 0.50`) yet stays below `0.8 * max_context`
            // even when `estimated` just crossed `max_context`.
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
                        reasoning: 0,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning_unknown: false,
                    }),
                    cause,
                }),
                other => other,
            })))
        }
    }

    let dir = tempdir();
    let file = dir.join("small.txt");
    tokio::fs::write(&file, "ok").await.unwrap();
    let path = file.to_string_lossy().into_owned();

    let tokenizer = hya_engine::tokens::TokenAccounting::default();
    let unit = "lorem ipsum dolor sit amet ".repeat(100);
    let per_unit = tokenizer.tokenizer().count_text(&unit);
    let blob = unit.repeat(20_000 / per_unit + 1);
    let item_tokens = tokenizer.tokenizer().count_text(&blob);
    // Five items (~100k estimated) cross a 95k window while `0.68 * 80k = 54.4k`
    // from step 4 is still well below `0.8 * 95k = 76k`.
    let max_context = u32::try_from(item_tokens * 4 + item_tokens * 3 / 4).unwrap();

    let mut scripts: Vec<_> = (0..6)
        .map(|step| {
            vec![
                FakeStep::Reasoning(format!("thinking step {step}")),
                FakeStep::ToolCall {
                    name: "read".to_string(),
                    input: json!({ "path": path }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        })
        .collect();
    scripts.push(vec![
        FakeStep::Text("done".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let route = CompactTokenizerRoute {
        inner: OpaqueReasoningRoute {
            inner: FakeProvider::scripted_turns(scripts),
            blob,
            max_context: Arc::new(std::sync::atomic::AtomicU32::new(max_context)),
            requests: Arc::clone(&requests),
        },
    };
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(route))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(NoSummarizer),
        CompactionConfig {
            keep_recent: 1,
            ..CompactionConfig::default()
        },
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    engine
        .admit_user_prompt(session, "work through it".to_string())
        .await
        .unwrap();
    let finish = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .expect("preflight-bound turn rewrites instead of failing un-compacted");
    assert_eq!(finish, FinishReason::Stop);
    assert!(
        rewrites(&engine.replay(session).await.unwrap()) > 0,
        "the kernel rewrote before preflight rejected the request"
    );
}
