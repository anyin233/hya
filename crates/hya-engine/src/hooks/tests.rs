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

/// Records lifecycle calls under its name; refuses `session.close.before`
/// when `refuse` is set.
struct LifecycleHook {
    name: &'static str,
    refuse: bool,
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}

impl LifecycleHook {
    fn record(&self, point: &str) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("{}:{point}", self.name));
    }
}

#[async_trait]
impl NativeHook for LifecycleHook {
    fn id(&self) -> &str {
        self.name
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[
            HookPoint::ModelStreamPermit,
            HookPoint::TurnStop,
            HookPoint::SessionClose,
            HookPoint::HeadlessRun,
            HookPoint::SessionRestore,
            HookPoint::RuntimeRestored,
        ]
    }

    async fn model_stream_permit(
        &self,
        input: NativeStreamPermitInput,
    ) -> Result<Option<NativeStreamPermit>, HostError> {
        self.record("permit");
        let calls = Arc::clone(&self.calls);
        let name = self.name;
        Ok(Some(NativeStreamPermit(Box::new(DropRecorder {
            calls,
            line: format!("{name}:released:{}", input.depth),
        }))))
    }

    async fn turn_stop(&self, _input: NativeTurnStopInput) -> Result<(), HostError> {
        self.record("stop");
        Ok(())
    }

    async fn session_close(&self, _input: NativeSessionCloseInput) -> Result<(), HostError> {
        self.record("close");
        if self.refuse {
            return Err(HostError::InvalidInput(format!("{} refuses", self.name)));
        }
        Ok(())
    }

    async fn headless_run(
        &self,
        input: NativeHeadlessRunInput,
    ) -> Result<Option<String>, HostError> {
        Ok((input.phase == HeadlessRunPhase::End).then(|| format!("{} pending", self.name)))
    }

    async fn session_restore(
        &self,
        _input: NativeSessionRestoreInput,
    ) -> Result<Option<String>, HostError> {
        Ok(Some(format!("{} restored", self.name)))
    }

    async fn runtime_restored(&self) -> Result<(), HostError> {
        self.record("restored");
        Ok(())
    }
}

struct DropRecorder {
    calls: Arc<std::sync::Mutex<Vec<String>>>,
    line: String,
}

impl Drop for DropRecorder {
    fn drop(&mut self) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(std::mem::take(&mut self.line));
    }
}

#[tokio::test]
async fn lifecycle_points_fan_out_and_close_refusals_run_latest_first() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hook = |name, refuse| -> Arc<dyn NativeHook> {
        Arc::new(LifecycleHook {
            name,
            refuse,
            calls: Arc::clone(&calls),
        })
    };
    let chain = HookChain::of_hooks([hook("first", true), hook("second", false)]);
    let session = SessionId::new();
    let taken = || std::mem::take(&mut *calls.lock().unwrap_or_else(|e| e.into_inner()));

    let permits = chain
        .model_stream_permit(NativeStreamPermitInput {
            session,
            root: session,
            depth: 1,
        })
        .await;
    assert_eq!(permits.len(), 2);
    drop(permits);
    assert_eq!(
        taken(),
        [
            "first:permit",
            "second:permit",
            "first:released:1",
            "second:released:1"
        ]
    );

    let refused = chain
        .session_close(NativeSessionCloseInput {
            session,
            kind: SessionCloseKind::Delete,
        })
        .await;
    assert_eq!(
        refused,
        Err(HostError::InvalidInput("first refuses".into()))
    );
    assert_eq!(taken(), ["second:close", "first:close"]);

    chain
        .turn_stop(NativeTurnStopInput {
            session,
            cause: hya_proto::FinishCause::UserCancel,
        })
        .await;
    chain.runtime_restored().await;
    assert_eq!(
        taken(),
        [
            "first:stop",
            "second:stop",
            "first:restored",
            "second:restored"
        ]
    );

    let input = |phase| NativeHeadlessRunInput { session, phase };
    assert!(
        chain
            .headless_run(input(HeadlessRunPhase::Start))
            .await
            .is_empty()
    );
    assert_eq!(
        chain.headless_run(input(HeadlessRunPhase::End)).await,
        ["first pending", "second pending"]
    );
    assert_eq!(
        chain.session_restore(session).await,
        ["first restored", "second restored"]
    );
}
