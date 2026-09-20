//! Integration tests for `hya-core`: the usage ledger records every finished
//! assistant message — provider-reported usage when present, a family
//! tokenizer estimate otherwise.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef, TokenUsage};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use tokio_util::sync::CancellationToken;

fn tempdir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-usage-ledger-{nanos}-{seq}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture_tokenizer_source() -> Arc<hya_core::model_tokenizers::ModelTokenizerSource> {
    // WordLevel: "a b c" -> 3 tokens; unknown words -> [UNK].
    let fixture = r#"{
        "version": "1.0",
        "truncation": null,
        "padding": null,
        "added_tokens": [],
        "normalizer": null,
        "pre_tokenizer": {"type": "Whitespace"},
        "post_processor": null,
        "decoder": null,
        "model": {"type": "WordLevel", "vocab": {"a": 0, "b": 1, "c": 2, "[UNK]": 3}, "unk_token": "[UNK]"}
    }"#;
    let bytes: Arc<Vec<u8>> = Arc::new(fixture.as_bytes().to_vec());
    Arc::new(
        hya_core::model_tokenizers::ModelTokenizerSource::with_loader(Arc::new(move |_| {
            Some(bytes.clone())
        })),
    )
}

async fn engine_with(
    script: Vec<FakeStep>,
    tokenizers: Option<Arc<hya_core::model_tokenizers::ModelTokenizerSource>>,
) -> (SessionEngine, PathBuf) {
    let dir = tempdir();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(script))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let mut engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    if let Some(tokenizers) = tokenizers {
        engine = engine.with_usage_tokenizers(tokenizers);
    }
    (engine, dir)
}

async fn run_turn(
    engine: &SessionEngine,
    dir: &std::path::Path,
    model: &str,
) -> hya_proto::SessionId {
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new(model),
        system_prompt: "x".to_string(),
        workdir: dir.to_path_buf(),
        reasoning: None,
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "hello please answer".to_string())
        .await
        .unwrap();
    let finish = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    session
}

#[tokio::test]
async fn ledger_records_provider_reported_usage() {
    let (engine, dir) = engine_with(
        vec![
            FakeStep::Text("measured answer".to_string()),
            FakeStep::Usage(TokenUsage {
                input: 120,
                output: 34,
                reasoning: 0,
                cache_read: 10,
                cache_write: 0,
            }),
            FakeStep::Finish(FinishReason::Stop),
        ],
        None,
    )
    .await;
    let session = run_turn(&engine, &dir, "fake").await;
    let usage = engine.store().read_usage(session).await.unwrap();
    assert_eq!(usage.len(), 1, "one row per finished assistant message");
    let row = &usage[0];
    assert_eq!(row.confidence, "provider");
    assert_eq!(row.prompt_tokens, 130, "input + cache_read");
    assert_eq!(row.completion_tokens, 34);
    assert_eq!(row.role, "build");
}

#[tokio::test]
async fn ledger_estimates_with_the_family_tokenizer_when_usage_is_missing() {
    let (engine, dir) = engine_with(
        vec![
            FakeStep::Text("a b c".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        Some(fixture_tokenizer_source()),
    )
    .await;
    let session = run_turn(&engine, &dir, "qwen/family-test").await;
    let usage = engine.store().read_usage(session).await.unwrap();
    assert_eq!(usage.len(), 1);
    let row = &usage[0];
    assert_eq!(row.confidence, "hf:Qwen/Qwen3-8B");
    assert_eq!(
        row.completion_tokens, 3,
        "real tokenizer counts the fixture vocab"
    );
    assert!(row.prompt_tokens > 0, "the prompt side is estimated too");
    assert_eq!(row.provider.as_deref(), Some("qwen"));
    assert_eq!(row.model.as_deref(), Some("qwen/family-test"));
}

#[tokio::test]
async fn ledger_estimates_calibrated_for_unmatched_models() {
    let (engine, dir) = engine_with(
        vec![
            FakeStep::Text("plain answer".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        Some(fixture_tokenizer_source()),
    )
    .await;
    let session = run_turn(&engine, &dir, "somecustom/mystery-9b").await;
    let usage = engine.store().read_usage(session).await.unwrap();
    assert_eq!(usage.len(), 1);
    let row = &usage[0];
    assert_eq!(row.confidence, "estimated");
    assert!(row.prompt_tokens > 0 && row.completion_tokens > 0);
}
