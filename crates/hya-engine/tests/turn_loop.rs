//! Integration tests for `hya-engine`: turn loop.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
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

struct Recording(Arc<AtomicBool>);

#[async_trait::async_trait]
impl Summarizer for Recording {
    async fn summarize(
        &self,
        _messages: &[Message],
        _options: hya_engine::SummarizeOptions,
    ) -> Result<String, CoreError> {
        self.0.store(true, Ordering::SeqCst);
        Ok("SUMMARY".to_string())
    }
}

#[tokio::test]
async fn compaction_auto_triggers_when_over_threshold() {
    let dir = tempdir();
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(Recording(called.clone())),
        CompactionConfig {
            // The fake route advertises a 200k window, so the threshold is
            // window-scaled; 0.001 clamps to MIN_RESOLVED_THRESHOLD (1000).
            token_threshold: 1_000_000,
            keep_recent: 1,
            context_fraction: 0.001,
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
    for _ in 0..3 {
        engine
            .admit_user_prompt(session, "earlier ".repeat(500))
            .await
            .unwrap();
    }
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
    assert!(
        called.load(Ordering::SeqCst),
        "summarizer must be invoked when over threshold"
    );
}

/// E1: a route advertising a context window must drive the threshold, instead of
/// the flat configured constant.
#[tokio::test]
async fn compaction_threshold_scales_to_the_advertised_context_window() {
    let dir = tempdir();
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    )
    // FakeProvider advertises max_context = 200_000, so 0.001 resolves to a
    // threshold of 200 tokens — far below the flat 1_000_000 fallback. If the
    // window were ignored, nothing here would ever compact.
    .with_compaction(
        Arc::new(Recording(called.clone())),
        CompactionConfig {
            token_threshold: 1_000_000,
            keep_recent: 1,
            context_fraction: 0.001,
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
    // ~2000 estimated tokens: under the flat fallback, over the scaled window.
    for _ in 0..2 {
        engine
            .admit_user_prompt(session, "p".repeat(4000))
            .await
            .unwrap();
    }
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
    assert!(
        called.load(Ordering::SeqCst),
        "the advertised context window must scale the compaction threshold"
    );
}

/// Records the text of every transcript the summarizer is asked to fold.
struct FoldRecorder(Arc<Mutex<Vec<Vec<String>>>>);

#[async_trait::async_trait]
impl Summarizer for FoldRecorder {
    async fn summarize(
        &self,
        messages: &[Message],
        _options: hya_engine::SummarizeOptions,
    ) -> Result<String, CoreError> {
        let folded = messages
            .iter()
            .map(|m| match m {
                Message::User { parts, .. } | Message::Assistant { parts, .. } => parts
                    .iter()
                    .filter_map(|p| match p {
                        hya_proto::Part::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                Message::System { content, .. } => content.clone(),
            })
            .collect();
        self.0.lock().unwrap().push(folded);
        Ok("LOCAL SUMMARY".to_string())
    }
}

/// AC3 + AC1/AC2/AC4: a local-summarizer compaction must persist its summary
/// behind the HYA_COMPACTED_CONTEXT marker, record one `ContextCompacted` whose
/// range covers exactly `folded_count` messages, and stop the next round from
/// re-folding the history it already summarized.
#[tokio::test]
async fn local_compaction_persists_and_is_not_repeated_next_round() {
    let dir = tempdir();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("first".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("second".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let folds = Arc::new(Mutex::new(Vec::new()));
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(FoldRecorder(folds.clone())),
        CompactionConfig {
            // The fake route advertises a 200k window, so the threshold is
            // window-scaled; 0.001 clamps to MIN_RESOLVED_THRESHOLD (1000).
            token_threshold: 1_000_000,
            keep_recent: 1,
            context_fraction: 0.001,
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
    for i in 0..3 {
        engine
            .admit_user_prompt(session, format!("ORIGINAL_PROMPT_{i} {}", "p".repeat(3000)))
            .await
            .unwrap();
    }
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
    assert_eq!(
        records.len(),
        1,
        "exactly one ContextCompacted per compaction"
    );
    let (marker_id, strategy, from_message, to_message, folded_count, threshold) = records[0];
    assert_eq!(strategy, hya_proto::CompactionStrategy::LocalSummarizer);
    // The record carries the RESOLVED threshold (window-scaled, clamped to the
    // floor of 1,000 tokens), not the flat configured fallback — that is the
    // number that actually drove the decision.
    assert_eq!(threshold, 1_000, "records the threshold in force");

    // AC1: the recorded output message really is the marker System message.
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
        "local summary must persist behind the shared marker: {marker_text}"
    );
    assert!(
        marker_text.contains("LOCAL SUMMARY"),
        "the summary text itself must be recoverable: {marker_text}"
    );

    // AC4: the recorded range covers exactly `folded_count` messages.
    let ids: Vec<_> = projection.session.messages.iter().map(|m| m.id).collect();
    let from_idx = ids.iter().position(|id| *id == from_message).unwrap();
    let to_idx = ids.iter().position(|id| *id == to_message).unwrap();
    assert_eq!(
        to_idx - from_idx + 1,
        folded_count as usize,
        "from_message..=to_message must span exactly folded_count messages"
    );

    // AC3: a second turn must not re-fold the history already summarized.
    engine
        .admit_user_prompt(session, "NEW_PROMPT after compaction".to_string())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    let folds = folds.lock().unwrap();
    for (round, folded) in folds.iter().enumerate().skip(1) {
        assert!(
            !folded.iter().any(|t| t.contains("ORIGINAL_PROMPT_0")),
            "round {round} re-folded pre-marker history: {folded:?}"
        );
    }
}

/// Records the message count and handoff flag of every call.
struct HandoffRecorder(Arc<Mutex<Vec<(usize, bool)>>>);

#[async_trait::async_trait]
impl Summarizer for HandoffRecorder {
    async fn summarize(
        &self,
        messages: &[Message],
        options: hya_engine::SummarizeOptions,
    ) -> Result<String, CoreError> {
        self.0
            .lock()
            .unwrap()
            .push((messages.len(), options.handoff));
        Ok("HANDOFF DOC".to_string())
    }
}

/// The handoff rung commits a handoff document as its compaction summary, and
/// the model writing it reads the whole live transcript, not a rendered slice.
#[tokio::test]
async fn handoff_rung_folds_via_a_call_over_the_whole_transcript() {
    let dir = tempdir();
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let method_order =
        hya_engine::parse_method_order(&["handoff", "snapcompact", "remote", "shake", "soft"])
            .unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(HandoffRecorder(calls.clone())),
        CompactionConfig {
            // The fake route advertises a 200k window, so the threshold is
            // window-scaled; 0.001 clamps to MIN_RESOLVED_THRESHOLD (1000).
            token_threshold: 1_000_000,
            keep_recent: 1,
            context_fraction: 0.001,
            method_order,
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
    for i in 0..3 {
        engine
            .admit_user_prompt(session, format!("PROMPT_{i} {}", "p".repeat(3000)))
            .await
            .unwrap();
    }
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

    let envelopes = engine.replay(session).await.unwrap();
    let strategies: Vec<_> = envelopes
        .iter()
        .filter_map(|e| match &e.event {
            Event::ContextCompacted { strategy, .. } => Some(*strategy),
            _ => None,
        })
        .collect();
    assert_eq!(
        strategies,
        vec![hya_proto::CompactionStrategy::Handoff],
        "the configured handoff-first order must run the handoff rung"
    );
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "exactly one handoff model call");
    assert!(calls[0].1, "the call is a handoff call");
    assert_eq!(
        calls[0].0, 3,
        "the handoff sees the whole transcript (the trailing handoff prompt is \
         appended by the ModelSummarizer itself, not by the rung)"
    );
    let projection = hya_proto::Projection::from_events(&envelopes);
    let doc: String = projection
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .flat_map(|m| {
            m.parts
                .iter()
                .filter_map(|p| match p {
                    PartProjection::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("");
    assert!(
        doc.contains("HYA_COMPACTED_CONTEXT"),
        "handoff persists behind the shared marker: {doc}"
    );
    assert!(
        doc.contains("HANDOFF DOC"),
        "the committed summary is the handoff document: {doc}"
    );
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

/// E3: eviction alone must be enough to skip summarizing, and it must not touch
/// the event log — the full tool output stays recoverable offline.
///
/// Spans two turns deliberately: within one turn every tool part lands in the
/// same assistant message, which sits inside `keep_recent`. Eviction is a
/// cross-turn reduction.
#[tokio::test]
async fn tool_output_eviction_avoids_summarizing_and_preserves_the_log() {
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
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(Recording(called.clone())),
        CompactionConfig {
            token_threshold: 1_000_000,
            keep_recent: 1,
            // 200k window * 0.16 = 32,000 tokens. The line-numbered Read result
            // for this file serializes to ~65KB of JSON, which really is about
            // 30,100 tokens, so turn 1 stays just under it on its own. Turn 2's
            // prompt adds ~3,300 and pushes it over, and evicting turn 1's
            // now-stale tool output drops it back to a few thousand.
            context_fraction: 0.16,
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
        .admit_user_prompt(session, "now summarize ".repeat(1_500))
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
    assert!(
        !called.load(Ordering::SeqCst),
        "eviction alone was enough; the summarizer must not have run"
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
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
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
    ]));
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
            Arc::new(Recording(Arc::new(AtomicBool::new(false)))),
            // Sized as in the eviction test above: turn 2 crosses the
            // threshold and evicting turn 1's output brings it far below.
            CompactionConfig {
                token_threshold: 1_000_000,
                keep_recent: 1,
                context_fraction: 0.16,
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
        "now summarize ".repeat(1_500),
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
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(Recording(called.clone())),
        CompactionConfig {
            token_threshold: 1_000_000,
            keep_recent: 1,
            // Same sizing as the E3 test above: turn 2 crosses the threshold
            // and evicting turn 1's read output brings it far below.
            context_fraction: 0.16,
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
        "now summarize ".repeat(1_500),
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
    assert!(
        !called.load(Ordering::SeqCst),
        "a re-applied eviction keeps the request under the threshold; no summary"
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
    .with_compaction(
        Arc::new(Recording(Arc::new(AtomicBool::new(false)))),
        CompactionConfig::default(),
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
        threshold, 150_000,
        "fake route advertises a 200k window: min(0.75 * window, window - reserve)"
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

    // Three read rounds, then a final answer, all in one turn.
    let provider = FakeProvider::scripted_turns(vec![
        vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
        vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
        vec![read(), FakeStep::Finish(FinishReason::ToolCalls)],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(Recording(called.clone())),
        CompactionConfig {
            token_threshold: 1_000_000,
            keep_recent: 1,
            // ~46,000-token threshold: fixed tool schemas (~14,000) plus one
            // ~30,100-token read output fit (~44,100), while two read outputs
            // (~74,200) cross the threshold and spilling the stale one brings
            // the request back to ~44,100 without summarizing.
            context_fraction: 0.23,
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
    assert!(
        !called.load(Ordering::SeqCst),
        "spilling was enough; nothing was summarized"
    );
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

/// Records, per summarizer call, the reasoning texts it was shown and the
/// summary it was anchored on.
type StepFolds = Arc<Mutex<Vec<(Vec<String>, Option<String>)>>>;
struct StepFoldRecorder(StepFolds);

#[async_trait::async_trait]
impl Summarizer for StepFoldRecorder {
    async fn summarize(
        &self,
        messages: &[Message],
        options: hya_engine::SummarizeOptions,
    ) -> Result<String, CoreError> {
        let reasoning = messages
            .iter()
            .flat_map(|message| match message {
                Message::Assistant { parts, .. } => parts.as_slice(),
                _ => &[],
            })
            .filter_map(|part| match part {
                hya_proto::Part::Reasoning { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        let mut calls = self.0.lock().unwrap();
        calls.push((reasoning, options.previous_summary));
        Ok(format!("STEP SUMMARY {}", calls.len()))
    }
}

/// A headless run is one prompt and one assistant message holding every step,
/// so no message prefix exists to fold. Once the replayed reasoning passes the
/// threshold, whole old steps (reasoning, calls and results together) must
/// fold into the summary while the prompt and the recent steps stay verbatim,
/// reasoning included; otherwise the run dies at the window.
#[tokio::test]
async fn single_turn_folds_old_steps_and_replays_the_kept_reasoning() {
    const STEPS: usize = 9;
    let dir = tempdir();
    let file = dir.join("small.txt");
    tokio::fs::write(&file, "ok").await.unwrap();
    let path = file.to_string_lossy().into_owned();

    // Each replayed item is ~20k tokens; the window holds seven of them, and
    // the threshold (0.55 of it) a little under four.
    let tokenizer = hya_engine::tokens::TokenAccounting::default();
    let unit = "lorem ipsum dolor sit amet ".repeat(100);
    let per_unit = tokenizer.tokenizer().count_text(&unit);
    let blob = unit.repeat(20_000 / per_unit + 1);
    let item_tokens = tokenizer.tokenizer().count_text(&blob);
    let max_context = u32::try_from(item_tokens * 7).unwrap();

    let mut scripts: Vec<_> = (0..STEPS)
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
    let route = OpaqueReasoningRoute {
        inner: FakeProvider::scripted_turns(scripts),
        blob,
        max_context: Arc::new(std::sync::atomic::AtomicU32::new(max_context)),
        requests: Arc::clone(&requests),
    };
    let router = Arc::new(ProviderRouter::new().with(Arc::new(route)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let folds: StepFolds = Arc::default();
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(StepFoldRecorder(Arc::clone(&folds))),
        CompactionConfig {
            keep_recent: 1,
            context_fraction: 0.55,
            reserve_tokens: 0,
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
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let requests = std::mem::take(&mut *requests.lock().unwrap());
    assert_eq!(requests.len(), STEPS + 1, "every round's request was sent");
    for request in &requests {
        assert!(
            tokenizer.estimate_with_reasoning_policy(
                &request.messages,
                hya_provider::ReasoningReplayPolicy::ProviderData
            ) <= max_context as usize,
            "every request fits the window"
        );
    }

    let envelopes = engine.replay(session).await.unwrap();
    let step_folds = envelopes
        .iter()
        .filter(|e| {
            matches!(
                &e.event,
                Event::ContextCompacted {
                    step_cut: Some(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(step_folds, 2, "old steps fold twice over nine steps");

    // The stored session keeps every step's reasoning.
    let projection = hya_proto::Projection::from_events(&envelopes);
    let stored = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    let stored_reasoning: Vec<_> = stored
        .parts
        .iter()
        .filter_map(|part| match part {
            PartProjection::Reasoning {
                id,
                text,
                provider_data: Some(data),
                ..
            } => Some((*id, text.clone(), data.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(stored_reasoning.len(), STEPS);

    // The last request: the summary, the prompt verbatim, and the kept steps
    // with their reasoning exactly as stored.
    let last = requests.last().unwrap();
    let markers: Vec<_> = last
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::System { content, .. }
                if content.starts_with(hya_provider::COMPACT_CONTEXT_MARKER) =>
            {
                Some(content.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(markers.len(), 1, "exactly one summary marker is sent");
    assert!(markers[0].contains("STEP SUMMARY 2"), "{}", markers[0]);
    assert!(
        last.messages.iter().any(|m| matches!(m,
            Message::User { parts, .. } if parts.iter().any(|p| matches!(p,
                hya_proto::Part::Text { text, .. } if text == "work through it")))),
        "the turn's prompt stays verbatim"
    );
    let sent_reasoning: Vec<_> = last
        .messages
        .iter()
        .flat_map(|message| match message {
            Message::Assistant { parts, .. } => parts.as_slice(),
            _ => &[],
        })
        .filter_map(|part| match part {
            hya_proto::Part::Reasoning {
                id,
                text,
                provider_data,
                ..
            } => Some((*id, text.clone(), provider_data.clone().unwrap())),
            _ => None,
        })
        .collect();
    assert!(!sent_reasoning.is_empty());
    assert_eq!(
        sent_reasoning.as_slice(),
        &stored_reasoning[STEPS - sent_reasoning.len()..],
        "the kept steps are the newest ones, reasoning replayed byte-identical"
    );

    // Folded steps reach the summarizer instead; the second fold updates the
    // first summary rather than starting over.
    let folds = folds.lock().unwrap();
    assert_eq!(folds.len(), 2);
    let folded: Vec<_> = folds.iter().flat_map(|(texts, _)| texts.clone()).collect();
    for (_, text, _) in &stored_reasoning[..STEPS - sent_reasoning.len()] {
        assert!(folded.contains(text), "{text} was summarized: {folded:?}");
        assert!(
            !sent_reasoning.iter().any(|(_, sent, _)| sent == text),
            "{text} left the request"
        );
    }
    assert_eq!(folds[0].1, None);
    assert_eq!(folds[1].1.as_deref(), Some("STEP SUMMARY 1"));
}

/// A fold in the same ladder walk as a spill must not bring the spilled
/// outputs back: the request still carries their placeholders.
#[tokio::test]
async fn a_fold_after_a_spill_keeps_the_spilled_placeholders() {
    let dir = tempdir();
    let big_file = dir.join("big.txt");
    // ~30,100 tokens per read (see the spill tests above).
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
    // spilling the oldest read is not enough and the prompt must fold.
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
    let called = Arc::new(AtomicBool::new(false));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(route))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(Recording(called.clone())),
        CompactionConfig {
            keep_recent: 2,
            reserve_tokens: 0,
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
    // Threshold 82,500: prompt (~40k) + three reads (~90k) is over, and still
    // over with the oldest read spilled; folding the prompt leaves ~60k.
    window.store(110_000, Ordering::SeqCst);
    engine
        .admit_user_prompt(session, "go on".to_string())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert!(called.load(Ordering::SeqCst), "the prompt was folded");
    let envelopes = engine.replay(session).await.unwrap();
    let reductions: Vec<_> = envelopes
        .iter()
        .filter_map(|e| match e.event {
            Event::ContextEvicted { .. } => Some("spill"),
            Event::ContextCompacted { .. } => Some("fold"),
            _ => None,
        })
        .collect();
    assert_eq!(
        reductions,
        ["spill", "fold"],
        "one walk spilled, then folded"
    );

    let requests = requests.lock().unwrap();
    let last = requests.last().unwrap();
    let outputs: Vec<String> = last
        .messages
        .iter()
        .flat_map(|message| match message {
            Message::Assistant { parts, .. } => parts.as_slice(),
            _ => &[],
        })
        .filter_map(|part| match part {
            hya_proto::Part::Tool {
                state: ToolPartState::Completed { output, .. },
                ..
            } => Some(
                output
                    .as_str()
                    .map_or_else(|| output.to_string(), ToString::to_string),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(outputs.len(), 3, "the turn's three reads are kept");
    assert!(
        outputs[0].starts_with("[tool output moved to "),
        "the spilled read stays spilled: {}",
        &outputs[0][..outputs[0].len().min(200)]
    );
    assert!(outputs[1].contains(&"R".repeat(100)));
    assert!(outputs[2].contains(&"R".repeat(100)));
}

/// When a provider's reported prompt size is plausible (`~0.70` of the local
/// estimate, e.g. dense JSON/reasoning tokenization) and still sits below
/// `resolved_threshold` (`0.75 * max_context`) while the local estimate has
/// already crossed `max_context`, the reduction ladder must still run before
/// `check_request_context` refuses the request.
#[tokio::test]
async fn step_fold_runs_before_preflight_rejects_an_under_threshold_provider_count() {
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
            // Plausible (`0.68 >= 0.50`) yet stays below `0.75 * max_context`
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
    // from step 4 is still well below `0.75 * 95k = 71.25k`.
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
    let folds: StepFolds = Arc::default();
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(route))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(StepFoldRecorder(Arc::clone(&folds))),
        CompactionConfig {
            keep_recent: 1,
            context_fraction: 0.75,
            reserve_tokens: 0,
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
        .expect("preflight-bound turn folds instead of failing un-compacted");
    assert_eq!(finish, FinishReason::Stop);
    assert!(
        !folds.lock().unwrap().is_empty(),
        "step fold ran before preflight rejected the request"
    );
}

#[tokio::test]
async fn snapcompact_carries_the_latest_todo_result_verbatim() {
    let dir = tempdir();
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "todo".to_string(),
                input: serde_json::json!({"ops": [
                    {"op": "add", "content": "inspect the inputs"},
                    {"op": "add", "content": "check all outputs"}
                ]}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("detail across the earlier turn ".repeat(600)),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("continued".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]));
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        PermissionPlane::new(PermissionRules::default()).0,
        EventBus::default(),
    )
    .with_compaction(
        Arc::new(FoldRecorder(Arc::default())),
        CompactionConfig {
            context_fraction: 0.001,
            keep_recent: 1,
            method_order: hya_engine::parse_method_order(&[
                "snapcompact",
                "shake",
                "remote",
                "soft",
                "handoff",
            ])
            .unwrap(),
            ..CompactionConfig::default()
        },
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir.clone(),
        reasoning: None,
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: dir.display().to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    for prompt in ["Track the criteria", "Continue the task"] {
        engine
            .admit_user_prompt(session, prompt.to_string())
            .await
            .unwrap();
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap();
    }

    let events = engine.replay(session).await.unwrap();
    let list = events
        .iter()
        .find_map(|event| match &event.event {
            Event::ToolResult { output, .. } => output.as_str(),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        list,
        "Current todos:\n- #1 [pending] inspect the inputs\n- #2 [pending] check all outputs"
    );
    let markers: Vec<_> = events
        .iter()
        .filter_map(|event| match event.event {
            Event::ContextCompacted {
                message,
                strategy: hya_proto::CompactionStrategy::SnapCompact,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(markers.len(), 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].messages.iter().any(|msg| matches!(
        msg, Message::System { id, content } if *id == markers[0] && content.ends_with(list)
    )));
    assert!(!requests[2].messages.iter().any(|msg| matches!(
        msg, Message::Assistant { parts, .. } if parts.iter().any(|part| matches!(
            part, hya_proto::Part::Tool { name, .. } if name.as_str() == "todo"
        ))
    )));
    let _ = std::fs::remove_dir_all(dir);
}
