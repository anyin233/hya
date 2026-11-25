use std::sync::Arc;

use hya_proto::{MessageId, ModelRef};

use super::*;

#[tokio::test]
async fn replaceable_activation_hooks_support_none_some_none() {
    let session = SessionId::new();
    scope_optional_activation_hooks(session, None, async {
        assert!(activation_hook_for(session).is_none());
        replace_activation_hooks(session, Some(Arc::new(NoopHookHost)));
        assert!(activation_hook_for(session).is_some());
        replace_activation_hooks(session, None);
        assert!(activation_hook_for(session).is_none());
    })
    .await;
}

fn last_verdict() -> VerifierVerdict {
    VerifierVerdict {
        score: 50,
        satisfied: false,
        evidence_quality: crate::loop_mode::EvidenceQuality::ClaimOnly,
        critical_gaps: Vec::new(),
        iteration_summary: String::new(),
        reason: String::new(),
    }
}

struct PrefixHook(&'static str);

#[async_trait]
impl HookDispatcher for PrefixHook {
    fn dispatch_event(&self, _envelope: &Envelope) {}

    async fn command_execute_before(
        &self,
        mut input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        input.text.insert_str(0, self.0);
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, mut input: TextCompleteInput) -> TextCompleteOutcome {
        input.text.insert_str(0, self.0);
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(
        &self,
        mut input: MessageUserBeforeInput,
    ) -> MessageUserBeforeOutcome {
        input.text.insert_str(0, self.0);
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

    async fn model_fallback(&self, _input: ModelFallbackInput) -> ModelFallbackOutcome {
        ModelFallbackOutcome::Retry {
            model: ModelRef::new(self.0),
        }
    }
}

/// `model.fallback` chains like `permission.ask`: give-ups pass the
/// consult on, and the first `retry` in dispatch order wins.
#[tokio::test]
async fn hook_chain_model_fallback_first_retry_wins() {
    let input = ModelFallbackInput {
        session: SessionId::new(),
        root_session: SessionId::new(),
        agent: None,
        message: MessageId::new(),
        model: ModelRef::new("alpha"),
        error_class: ModelFailureClass::Retryable,
        error_message: "reset".to_string(),
        attempt: 1,
        tried: vec![ModelRef::new("alpha")],
    };
    let chain = HookChain::new(vec![
        Arc::new(NoopHookHost),
        Arc::new(PrefixHook("beta")),
        Arc::new(PrefixHook("gamma")),
    ]);
    assert_eq!(
        chain.model_fallback(input.clone()).await,
        ModelFallbackOutcome::Retry {
            model: ModelRef::new("beta")
        }
    );
    assert_eq!(
        HookChain::new(vec![Arc::new(NoopHookHost)])
            .model_fallback(input)
            .await,
        ModelFallbackOutcome::GiveUp
    );
}

#[tokio::test]
async fn hook_chain_applies_rewrites_in_registration_order() {
    let chain = HookChain::new(vec![Arc::new(PrefixHook("a")), Arc::new(PrefixHook("b"))]);
    let outcome = chain
        .message_user_before(MessageUserBeforeInput {
            session: SessionId::new(),
            text: "body".to_string(),
        })
        .await;
    let MessageUserBeforeOutcome::Continue { text } = outcome;
    assert_eq!(text, "babody");
}

/// Default trait impls must keep existing implementors compiling and
/// behave as no-ops: a `NoopHookHost` answers the new injection points
/// without overriding them.
#[tokio::test]
async fn default_hook_impls_are_noops() {
    let session = SessionId::new();
    let host = NoopHookHost;
    assert_eq!(
        host.compaction_before(CompactionBeforeInput {
            session,
            trigger: CompactionTrigger::Overflow,
            messages_token_estimate: 10,
        })
        .await,
        CompactionDecision::Proceed
    );
    host.compaction_after(CompactionAfterInput {
        session,
        summary_tokens: 1,
    })
    .await;
    host.session_start(SessionLifecycleInput { session }).await;
    host.session_end(SessionLifecycleInput { session }).await;
    host.agent_spawn(AgentSpawnInput {
        parent: session,
        child: SessionId::new(),
    })
    .await;
    // Goal/loop evaluator hooks default to unregistered and unprobed, so
    // existing implementors stay fail-open to the built-in evaluators.
    assert!(
        !host.has_goal_evaluate(),
        "default capability probe must be false"
    );
    assert!(
        host.goal_evaluate("condition", "transcript").await.is_err(),
        "default goal_evaluate hook must be unregistered"
    );
    assert!(
        host.loop_verify("target", "transcript").await.is_err(),
        "default loop_verify hook must be unregistered"
    );
    assert!(
        host.loop_plan("target", &[], &last_verdict(), "")
            .await
            .is_err(),
        "default loop_plan hook must be unregistered"
    );
    assert_eq!(
        host.loop_should_stop("target", "transcript").await,
        None,
        "default loop_should_stop consult must be a no-op"
    );
}

/// A `Skip` is only ever honored for proactive compaction. On an
/// overflow-forced trigger it is demoted to `Proceed` (plus a warning),
/// because skipping a compaction the request depends on risks context
/// overflow.
#[test]
fn skip_is_honored_proactively_but_ignored_on_overflow() {
    let skip = || CompactionDecision::Skip {
        reason: "still fits".to_string(),
    };
    assert_eq!(
        resolve_compaction_decision(skip(), CompactionTrigger::Proactive),
        CompactionResolution::Skip {
            reason: "still fits".to_string()
        }
    );
    assert_eq!(
        resolve_compaction_decision(skip(), CompactionTrigger::Overflow),
        CompactionResolution::Proceed,
        "overflow-forced compaction must never be skipped"
    );
}

/// `Proceed` and `Replace` pass through unchanged for both triggers: a
/// replacement of the summarizer instructions is not a safety decision and
/// needs no trigger gating.
#[test]
fn proceed_and_replace_pass_through_both_triggers() {
    for trigger in [CompactionTrigger::Overflow, CompactionTrigger::Proactive] {
        assert_eq!(
            resolve_compaction_decision(CompactionDecision::Proceed, trigger),
            CompactionResolution::Proceed
        );
        assert_eq!(
            resolve_compaction_decision(
                CompactionDecision::Replace {
                    instructions: "summarize as bullets".to_string(),
                },
                trigger
            ),
            CompactionResolution::Replace {
                instructions: "summarize as bullets".to_string(),
            }
        );
    }
}
