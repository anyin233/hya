#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter, ReasoningEffort};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};

use crate::{AppState, ServerState};

use super::agent_catalog::AgentEntry;
use super::reference::apply_agent_entry;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-server-reference-test-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> ServerState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
    ServerState::new(AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "system".to_string(),
            workdir,
            reasoning: None,
        }),
    ))
}
fn agent() -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("12th-anth/claude-opus-4-8"),
        system_prompt: "system".to_string(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    }
}

fn entry(variant: Option<&str>, options: BTreeMap<String, Value>) -> AgentEntry {
    AgentEntry {
        name: "build".to_string(),
        description: None,
        mode: "primary".to_string(),
        hidden: false,
        native: true,
        model: Some("12th-anth/claude-opus-4-8".to_string()),
        variant: variant.map(str::to_string),
        temperature: None,
        top_p: None,
        color: None,
        steps: None,
        options,
        request_headers: BTreeMap::new(),
        request_body: BTreeMap::new(),
        permissions: Vec::new(),
        prompt: None,
    }
}

#[test]
fn apply_agent_entry_sets_reasoning_from_variant() {
    let mut agent = agent();
    let entry = entry(Some("max"), BTreeMap::new());
    let config = json!({
        "provider": {
            "12th-anth": {
                "models": {
                    "claude-opus-4-8": {
                        "variants": {
                            "max": { "thinking": { "budgetTokens": 31999 } }
                        }
                    }
                }
            }
        }
    });

    apply_agent_entry(
        &mut agent,
        &entry,
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &config,
    );

    assert_eq!(agent.reasoning, Some(ReasoningEffort::Max));
}

#[test]
fn apply_agent_entry_leaves_reasoning_unset_without_signal() {
    let mut agent = agent();
    let entry = entry(None, BTreeMap::new());

    apply_agent_entry(
        &mut agent,
        &entry,
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &json!({}),
    );

    assert_eq!(agent.reasoning, None);
}

#[tokio::test]
async fn session_agent_with_guidance_uses_session_workdir() {
    let server_dir = tempdir();
    let session_dir = tempdir();
    let st = state(server_dir.clone()).await;
    let session = st
        .engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: session_dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();

    let agent = super::reference::session_agent_with_guidance(&st, session).await;

    assert_eq!(agent.workdir, session_dir);
    assert_ne!(agent.workdir, server_dir);
}
