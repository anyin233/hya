//! Integration tests for `hya-core`: turn loop.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{
    AgentSpec, CompactionConfig, CoreError, CreateSession, EventBus, SessionEngine, Summarizer,
};
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

    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("I'll read it".to_string()),
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "path": path }),
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

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant message");

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
        _options: hya_core::SummarizeOptions,
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
        },
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
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
        },
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
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
        _options: hya_core::SummarizeOptions,
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
        },
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
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
    // floor), not the flat configured fallback — that is the number that
    // actually drove the decision.
    assert_eq!(
        threshold,
        hya_core::MIN_RESOLVED_THRESHOLD as u64,
        "records the threshold in force"
    );

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
    let big = "R".repeat(20_000);
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
            // 200k window * 0.02 = 4000 tokens. Turn 1 (~1400) stays under, so the
            // tool output survives into turn 2 — where it is finally stale.
            context_fraction: 0.02,
        },
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
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
        .admit_user_prompt(session, "now summarize ".repeat(900))
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
