#![allow(dead_code, clippy::expect_used)]

use std::sync::Arc;

use hya_bundle::{AgentRole, BundleCatalog, BundleSource, SourceFile};
use hya_core::RuntimeRegistry;
use hya_proto::{MessageId, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, Provider, ProviderError,
};
use hya_tool::ToolRegistry;

/// Wraps [`FakeProvider`] with a stable configured provider identity.
///
/// Durable spawn admission resolves a canonical provider identity and fails
/// closed when any router member leaves `configured_identity_v1` unset, so a
/// bare `FakeProvider` router cannot reach the admission path at all.
///
/// This wrapper exists *because* `FakeProvider` itself must keep returning
/// `None`: `crates/hya-app/src/runtime.rs` asserts that a bare-`FakeProvider`
/// router fails closed with `ProviderIdentityUnavailable`. Add the identity
/// here, never there.
pub struct IdentityFakeProvider {
    inner: FakeProvider,
}

impl IdentityFakeProvider {
    /// Wraps `inner` so the router it joins can satisfy durable admission.
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

/// Rejects agent ids that the `bundle.yaml` built below cannot carry verbatim.
///
/// The manifest is assembled by string concatenation with three *unquoted*
/// interpolation points per agent (`local_id`, `stable_id`, and the flow
/// sequence `can_spawn: [a, b]`), so some ids parse successfully into a
/// *different* value than the caller wrote. Those are the dangerous ones —
/// they produce a fixture that silently disagrees with its own arguments.
/// Measured against the real `hya_bundle::prepare_builtins` path:
///
/// - `,` splits the `can_spawn` flow sequence: `can_spawn: [alpha,beta]`
///   silently yields two spawn edges (`alpha`, `beta`).
/// - A leading `&` is a YAML anchor; the id becomes `""`.
/// - Surrounding whitespace (`" a"`, `"a "`) is silently trimmed away.
/// - `""` is an empty id.
///
/// Ids containing other YAML indicators (`-`, `:`, `]`, `[`, `{`, `*`, `!`,
/// `%`, `@`, `'`, `"`) already fail loudly with an `InvalidManifest` parse
/// error, so they need no guard here.
///
/// Deliberately **not** guarded: YAML 1.1 bareword booleans (`no`, `on`, `y`,
/// `off`, `yes`) and the other implicit-tag barewords (`null`, `~`, `123`,
/// `.inf`). They were tested against `prepare_builtins` and every one
/// round-trips exactly — `serde_norway` resolves only `true|True|TRUE|
/// false|False|FALSE` as booleans, and these fields deserialize as raw
/// `String`s anyway. A guard against that list would catch nothing while
/// being believed by whoever reads it next. Please do not "fix" it back.
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
    let mut manifest = String::from(
        "api_version: hya.agent-bundle/v1\nkind: AgentBundle\nidentity:\n  id: hya/app-tests\n  version: 0.0.0\n  publisher: hya-tests\nagents:\n",
    );
    let mut files = Vec::with_capacity(agents.len() + 1);
    for (stable_id, role, can_spawn) in agents {
        assert_yaml_safe_id(stable_id);
        for target in *can_spawn {
            assert_yaml_safe_id(target);
        }
        let role = match role {
            AgentRole::Main => "main",
            AgentRole::Subagent => "subagent",
        };
        manifest.push_str(&format!(
            "  - local_id: {stable_id}\n    stable_id: {stable_id}\n    role: {role}\n    prompt: prompts/{stable_id}.md\n    spawn_lifecycle: transient\n    harness_access: full\n"
        ));
        if !can_spawn.is_empty() {
            manifest.push_str("    can_spawn: [");
            manifest.push_str(&can_spawn.join(", "));
            manifest.push_str("]\n");
        }
        files.push(SourceFile::new(
            format!("prompts/{stable_id}.md"),
            format!("{stable_id} prompt").into_bytes(),
        ));
    }
    files.push(SourceFile::new("bundle.yaml", manifest.into_bytes()));
    let prepared = hya_bundle::prepare_builtins(vec![BundleSource::new("hya/app-tests", files)])
        .expect("test bundle must prepare");
    let catalog = BundleCatalog::from_verified_catalogs(&[&prepared])
        .expect("test bundle must retain verified identity");
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ))
}
