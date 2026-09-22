//! Plugin hook dispatchers are retained by immutable runtime generations.

#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use hya_bundle::{BundleCatalog, BundleSource, SourceFile, prepare_package};
use hya_core::{
    AgentCatalog, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CreateSession, EventBus, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome, RuntimeRegistry, RuntimeSource, RuntimeSourceId, SessionEngine,
    SessionLifecycleInput, TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_proto::{AgentName, Envelope, ModelRef};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

struct RewriteHook {
    suffix: &'static str,
    events: Arc<AtomicUsize>,
    starts: Arc<AtomicUsize>,
    ends: Arc<AtomicUsize>,
}

#[async_trait]
impl HookDispatcher for RewriteHook {
    fn dispatch_event(&self, _envelope: &Envelope) {
        self.events.fetch_add(1, Ordering::SeqCst);
    }

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue {
            text: format!("{}{}", input.text, self.suffix),
        }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(
        &self,
        mut input: MessageUserBeforeInput,
    ) -> MessageUserBeforeOutcome {
        input.text.push_str(self.suffix);
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

    async fn session_start(&self, _input: SessionLifecycleInput) {
        self.starts.fetch_add(1, Ordering::SeqCst);
    }

    async fn session_end(&self, _input: SessionLifecycleInput) {
        self.ends.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn removed_plugin_hooks_leave_fresh_bindings_but_remain_live_on_old_bindings() {
    let prepared = prepare_package(BundleSource::new(
        "plugin",
        vec![SourceFile::new(
            "bundle.yaml",
            "kind: Plugin\nidentity: { id: acme/rewrite, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .expect("prepare Plugin");
    let bundles = BundleCatalog::from_verified_catalogs(&[&prepared]).expect("bundle catalog");
    let catalog = Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("agent catalog"));
    let registry = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
    let source_id = RuntimeSourceId::bundle("acme/rewrite");
    let starts = Arc::new(AtomicUsize::new(0));
    let ends = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(AtomicUsize::new(0));
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                RuntimeSource::new(source_id.clone(), [7; 32], Arc::new(()), Vec::new())
                    .with_hooks(Arc::new(RewriteHook {
                        suffix: "/old",
                        events: Arc::clone(&events),
                        starts: Arc::clone(&starts),
                        ends: Arc::clone(&ends),
                    })),
            ])
        })
        .expect("publish hook source");

    let workdir = std::env::temp_dir();
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.expect("store"),
        Arc::new(ProviderRouter::new()),
        Arc::clone(&registry),
        permission,
        EventBus::default(),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .expect("create session");
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let before_admission_events = events.load(Ordering::SeqCst);
    let message = engine
        .admit_command_prompt(session, "test".into(), String::new(), "body".into())
        .await
        .expect("bundle admission hooks");
    let projection = engine.read_projection(session).await.expect("projection");
    let admitted = projection
        .session
        .messages
        .iter()
        .find(|candidate| candidate.id == message)
        .expect("admitted message")
        .parts
        .iter()
        .filter_map(|part| match part {
            hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(admitted, "body/old/old");
    assert!(events.load(Ordering::SeqCst) > before_admission_events);
    let old = registry.bind_turn(&workdir).expect("old binding");
    let old_hooks = old.bundle_hooks_for_agent("build");
    assert_eq!(old_hooks.len(), 1);

    registry
        .refresh(|candidate| {
            candidate.remove_sources(&BTreeSet::from([source_id.clone()]));
            Ok(())
        })
        .expect("remove hook source");
    let fresh = registry.bind_turn(&workdir).expect("fresh binding");
    assert!(fresh.bundle_hooks_for_agent("build").is_empty());
    assert!(old.bundle_hooks_for_agent("acme-bundle-agent").is_empty());
    let fresh_message = engine
        .admit_user_prompt(session, "fresh".to_string())
        .await
        .expect("fresh admission");
    let projection = engine.read_projection(session).await.expect("projection");
    let fresh_text = projection
        .session
        .messages
        .iter()
        .find(|candidate| candidate.id == fresh_message)
        .expect("fresh message")
        .parts
        .iter()
        .filter_map(|part| match part {
            hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(fresh_text, "fresh", "admission must use its fresh binding");

    let outcome = old_hooks[0]
        .message_user_before(MessageUserBeforeInput {
            session: hya_proto::SessionId::new(),
            text: "body".to_string(),
        })
        .await;
    let MessageUserBeforeOutcome::Continue { text } = outcome;
    assert_eq!(text, "body/old");
    assert!(
        engine
            .delete_session(session)
            .await
            .expect("delete session")
    );
    assert_eq!(ends.load(Ordering::SeqCst), 1);
}
