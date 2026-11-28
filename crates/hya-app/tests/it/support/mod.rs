#![allow(dead_code, clippy::expect_used)]

pub mod lifecycle;

use std::ffi::{OsStr, OsString};
use std::sync::{Arc, OnceLock};

use hya_bundle::{AgentRole, BundleCatalog, BundleSource, SourceFile};
use hya_engine::RuntimeRegistry;
use hya_proto::{MessageId, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, Provider, ProviderError,
};
use hya_tool::ToolRegistry;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

static ENV_LOCK: OnceLock<RwLock<()>> = OnceLock::new();

fn shared_env_lock() -> &'static RwLock<()> {
    ENV_LOCK.get_or_init(|| RwLock::new(()))
}

/// Exclusive process-wide gate for integration tests that mutate `HOME`/`XDG_*`.
pub async fn env_lock() -> RwLockWriteGuard<'static, ()> {
    shared_env_lock().write().await
}

/// Blocking exclusive gate for synchronous `#[test]` cases that mutate `HOME`/`XDG_*`.
pub fn env_lock_blocking() -> RwLockWriteGuard<'static, ()> {
    shared_env_lock().blocking_write()
}

/// Shared read gate so concurrent tests reading env or spawning subprocesses never
/// race with [`env_lock`] mutations.
pub async fn env_read_lock() -> RwLockReadGuard<'static, ()> {
    shared_env_lock().read().await
}

/// Restores environment variables on drop; hold [`env_lock`] / [`env_lock_blocking`] while active.
pub struct EnvVarGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvVarGuard {
    pub fn new() -> Self {
        Self { saved: Vec::new() }
    }

    pub fn set(&mut self, key: &'static str, value: impl AsRef<OsStr>) {
        if !self.saved.iter().any(|(existing, _)| *existing == key) {
            self.saved.push((key, std::env::var_os(key)));
        }
        // SAFETY: callers hold the process-wide `env_lock` gate.
        unsafe { std::env::set_var(key, value) };
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (key, previous) in self.saved.iter().rev() {
            match previous {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

/// Wraps [`FakeProvider`] with a stable configured provider identity.
pub struct IdentityFakeProvider {
    inner: FakeProvider,
}

impl IdentityFakeProvider {
    pub fn new(inner: FakeProvider) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl Provider for IdentityFakeProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }

    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        Some(b"hya-test-nested-spawn-identity-v1".to_vec())
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.inner.stream(request, session, message).await
    }
}

fn assert_yaml_safe_id(id: &str) {
    assert!(!id.is_empty(), "test agent id must not be empty");
    assert!(
        !id.contains(','),
        "test agent id {id:?} contains ',', which splits the can_spawn flow sequence"
    );
    assert!(
        !id.starts_with('&'),
        "test agent id {id:?} starts with '&', which YAML parses as an anchor and yields an empty id"
    );
    assert_eq!(
        id.trim(),
        id,
        "test agent id {id:?} has surrounding whitespace, which YAML silently trims"
    );
}

pub fn test_runtime(
    tools: Arc<ToolRegistry>,
    agents: &[(&str, AgentRole, &[&str])],
) -> Arc<RuntimeRegistry> {
    let core_agents =
        hya_bundle::first_party_bundle("hya/core-agents").expect("first-party hya/core-agents");
    let mut prepared = Vec::new();
    for (stable_id, role, can_spawn) in agents {
        assert_yaml_safe_id(stable_id);
        for target in *can_spawn {
            assert_yaml_safe_id(target);
        }
        if core_agents.bundles()[0]
            .agents()
            .iter()
            .any(|agent| agent.id.as_str() == *stable_id)
        {
            continue;
        }
        let role = match role {
            AgentRole::Main => "main",
            AgentRole::Subagent => "subagent",
        };
        let mut manifest = format!(
            "kind: Bundle\nschema_version: 2\nidentity:\n  id: hya/app-tests-{stable_id}\n  version: 0.0.0\n  publisher: hya-tests\nagents:\n  - id: {stable_id}\n    role: {role}\n    prompt: prompts/{stable_id}.md\n"
        );
        if !can_spawn.is_empty() {
            manifest.push_str("    can_spawn: [");
            manifest.push_str(&can_spawn.join(", "));
            manifest.push_str("]\n");
        }
        let files = vec![
            SourceFile::new(
                format!("prompts/{stable_id}.md"),
                format!("{stable_id} prompt").into_bytes(),
            ),
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
        ];
        prepared.push(
            hya_bundle::prepare_package(BundleSource::new(
                format!("hya/app-tests-{stable_id}"),
                files,
            ))
            .expect("test bundle must prepare"),
        );
    }
    let mut refs = prepared.iter().collect::<Vec<_>>();
    let fixture_ids: std::collections::BTreeSet<&str> =
        agents.iter().map(|(id, _, _)| *id).collect();
    if let Ok(multi_agent) = hya_bundle::first_party_bundle("hya/multi-agent")
        && multi_agent.bundles().iter().all(|bundle| {
            bundle
                .agents()
                .iter()
                .all(|agent| !fixture_ids.contains(agent.id.as_str()))
        })
    {
        refs.push(multi_agent);
    }
    refs.push(core_agents);
    let catalog = BundleCatalog::from_verified_catalogs(&refs)
        .expect("test bundles must retain verified identity");
    let catalog = hya_engine::AgentCatalog::new(Arc::new(catalog)).expect("test agent catalog");
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ))
}
