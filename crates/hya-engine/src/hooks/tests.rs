use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{MessageId, ModelRef, SessionId};
use hya_runtime_abi::HostError;
use serde_json::{Value, json};

use super::*;

#[tokio::test]
async fn replaceable_activation_hooks_support_none_some_none() {
    let session = SessionId::new();
    scope_optional_activation_hooks(session, None, async {
        assert!(activation_hook_for(session).is_none());
        replace_activation_hooks(session, Some(Arc::new(HookChain::default())));
        assert!(activation_hook_for(session).is_some());
        replace_activation_hooks(session, None);
        assert!(activation_hook_for(session).is_none());
    })
    .await;
}

/// Prefixes rewritten text, retries with its own model name, and answers
/// `test/bundle:point` with its name.
struct PrefixHook {
    name: &'static str,
    declared: Vec<String>,
}

impl PrefixHook {
    fn named(name: &'static str) -> Arc<dyn NativeHook> {
        Arc::new(Self {
            name,
            declared: vec!["test/bundle:point".to_string()],
        })
    }
}

#[async_trait]
impl NativeHook for PrefixHook {
    fn id(&self) -> &str {
        self.name
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::MessageUserBefore, HookPoint::ModelFallback]
    }

    fn declared_points(&self) -> &[String] {
        &self.declared
    }

    async fn message_user_before(
        &self,
        input: NativeMessageUserBeforeInput,
    ) -> Result<String, HostError> {
        Ok(format!("{}{}", self.name, input.text))
    }

    async fn model_fallback(
        &self,
        _input: NativeModelFallbackInput,
    ) -> Result<Option<ModelRef>, HostError> {
        Ok(Some(ModelRef::new(self.name)))
    }

    async fn invoke_declared(
        &self,
        _point: &str,
        _input: Value,
    ) -> Result<Option<Value>, HostError> {
        Ok(Some(json!(self.name)))
    }
}

/// Implements every point and declares nothing, answering with defaults.
struct DeferHook;

#[async_trait]
impl NativeHook for DeferHook {
    fn id(&self) -> &str {
        "defer"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &HookPoint::ALL
    }
}

/// Fails every call.
struct FailingHook;

#[async_trait]
impl NativeHook for FailingHook {
    fn id(&self) -> &str {
        "failing"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::MessageUserBefore]
    }

    async fn message_user_before(
        &self,
        _input: NativeMessageUserBeforeInput,
    ) -> Result<String, HostError> {
        Err(HostError::Internal("boom".into()))
    }
}

fn fallback_input() -> NativeModelFallbackInput {
    NativeModelFallbackInput {
        session: SessionId::new(),
        root_session: SessionId::new(),
        agent: None,
        message: MessageId::new(),
        model: ModelRef::new("alpha"),
        error_class: ModelFailureClass::Retryable.as_str().to_string(),
        error_message: "reset".to_string(),
        attempt: 1,
        tried: vec![ModelRef::new("alpha")],
    }
}

/// `model.fallback`: give-ups pass the consult on, and the first named
/// model in dispatch order wins.
#[tokio::test]
async fn model_fallback_first_retry_wins() {
    let chain = HookChain::of_hooks([
        Arc::new(DeferHook) as Arc<dyn NativeHook>,
        PrefixHook::named("beta"),
        PrefixHook::named("gamma"),
    ]);
    assert_eq!(
        chain.model_fallback(fallback_input()).await,
        Some(ModelRef::new("beta"))
    );
    assert_eq!(
        HookChain::of_hooks([Arc::new(DeferHook) as Arc<dyn NativeHook>])
            .model_fallback(fallback_input())
            .await,
        None
    );
}

/// Rewrites feed each output into the next entry; a failing entry is
/// skipped, and an entry scoped away from the point is not called.
#[tokio::test]
async fn rewrites_run_in_registration_order_skipping_failures_and_scoped_entries() {
    let chain = HookChain::new(vec![
        HookEntry::new(PrefixHook::named("a")),
        HookEntry::new(Arc::new(FailingHook)),
        HookEntry::scoped(
            PrefixHook::named("x"),
            &["acme/hook/chat.params".to_string()],
        ),
        HookEntry::new(PrefixHook::named("b")),
    ]);
    let text = chain
        .message_user_before(NativeMessageUserBeforeInput {
            session: SessionId::new(),
            text: "body".to_string(),
        })
        .await;
    assert_eq!(text, "babody");
}

/// A hook implementing every point with defaults leaves every payload as is.
#[tokio::test]
async fn default_hook_methods_are_noops() {
    let session = SessionId::new();
    let chain = HookChain::of_hooks([Arc::new(DeferHook) as Arc<dyn NativeHook>]);
    assert_eq!(
        chain
            .compaction_before(NativeCompactionBeforeInput {
                session,
                trigger: NativeCompactionTrigger::Overflow,
                messages_token_estimate: 10,
            })
            .await,
        NativeCompactionDecision::Proceed
    );
    chain
        .compaction_after(NativeCompactionAfterInput {
            session,
            summary_tokens: 1,
        })
        .await;
    chain.session_start(session).await;
    chain.session_end(session).await;
    chain
        .agent_spawn(NativeAgentSpawnInput {
            parent: session,
            child: SessionId::new(),
        })
        .await;
    assert!(!chain.declares("test/bundle:point"));
    assert_eq!(
        chain
            .invoke_declared("test/bundle:point", Value::Null)
            .await,
        None
    );
}

/// Bundle-declared points are answered by the latest implementer, and only
/// entries that admit the point are asked.
#[tokio::test]
async fn declared_points_answer_from_the_latest_admitting_entry() {
    let chain = HookChain::new(vec![
        HookEntry::new(PrefixHook::named("early")),
        HookEntry::new(PrefixHook::named("late")),
        HookEntry::scoped(
            PrefixHook::named("hidden"),
            &["acme/hook/event".to_string()],
        ),
    ]);
    assert!(chain.declares("test/bundle:point"));
    assert!(!chain.declares("other/bundle:point"));
    assert_eq!(
        chain
            .invoke_declared("test/bundle:point", Value::Null)
            .await,
        Some(json!("late"))
    );
}
