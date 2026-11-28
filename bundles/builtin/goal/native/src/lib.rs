//! Native ABI v2 implementation of the `hya/goal` built-in bundle:
//! goal-mode iteration driver, independent model/hook evaluators, contract
//! validation, managed lifecycle service, and [`GoalDriver`] export.

use std::sync::Arc;

/// Goal-mode iteration driver, safety caps, contract validation, and [`GoalDriver`].
pub mod driver;
/// Independent goal evaluators ([`ModelGoalEvaluator`] and [`PluginGoalEvaluator`]).
pub mod evaluator;
/// Managed [`NativeService`] for `hya/goal`.
pub mod service;

pub use driver::{
    DEFAULT_MAX_ITERATIONS, GOAL_DRIVER_ID, GateOutcome, GoalGate, GoalRunRequest, IterationDriver,
    IterationExecutor, IterationGate, LeadTurnExecutor, NativeGoalDriver, SafetyCaps, prepare_goal,
    render_transcript, run_goal, validate_goal_condition, wrap_up_directive,
};
pub use evaluator::{
    DEFAULT_EVALUATOR_SYSTEM_PROMPT, EVALUATE_HOOK, EVALUATE_HOOK_POINT, EvaluateHookInput,
    EvaluateHookReply, GoalEvaluator, MALFORMED_VERDICT_REASON, ModelGoalEvaluator,
    PLUGIN_HOOK_ERROR_REASON, PluginGoalEvaluator, Verdict, resolve_evaluator_model,
    skill_prompt_body,
};
pub use service::{BUNDLE_ID, GOAL_SERVICE_ID, GoalDriverService};

hya_runtime_abi::export_native_bundle_v2!(|out| {
    out.services.push(Arc::new(GoalDriverService::new()));
    out.hook_points.push(EVALUATE_HOOK_POINT.to_string());
    out.drivers.push(Arc::new(NativeGoalDriver));
});

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use async_trait::async_trait;

    async fn run_goal_request(
        runtime: Arc<dyn HostRuntime>,
        request: GoalRunRequest,
        cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        prepare_goal(runtime, request, cancel).await?.await
    }
    use hya_proto::{
        ActorClaim, AgentName, Envelope, Event, EventSeq, FinishReason, Message, MessageId,
        ModelRef, PartId, Projection, Role, SessionId,
    };
    use hya_runtime_abi::{
        BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
        BundleClaimRequest, BundleStateEntry, DriverRunOutcome, ExtensionEntry, ExtensionKind,
        HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
        HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, ModelCallRequest,
        ModelCallResponse, ModelStreamChunk, ModelWindowInfo, NativeService, SessionListFilter,
        SessionSummaryMeta, TurnOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus,
        abi_digest_v2,
    };
    use serde_json::{Value, json};
    use tokio::sync::{broadcast, mpsc};
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct MockHost {
        projection: Mutex<Projection>,
        turn_replies: Mutex<VecDeque<Result<String, HostError>>>,
        turn_prompts: Mutex<Vec<String>>,
        model_replies: Mutex<VecDeque<Result<String, HostError>>>,
        model_requests: Mutex<Vec<ModelCallRequest>>,
        has_hook: AtomicBool,
        hook_replies: Mutex<VecDeque<Result<Option<Value>, HostError>>>,
        hook_transcripts: Mutex<Vec<String>>,
        bundle_config: Mutex<Value>,
        bundle: Mutex<Option<hya_bundle::Bundle>>,
    }

    impl MockHost {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                projection: Mutex::new(Projection::default()),
                turn_replies: Mutex::new(VecDeque::new()),
                turn_prompts: Mutex::new(Vec::new()),
                model_replies: Mutex::new(VecDeque::new()),
                model_requests: Mutex::new(Vec::new()),
                has_hook: AtomicBool::new(false),
                hook_replies: Mutex::new(VecDeque::new()),
                hook_transcripts: Mutex::new(Vec::new()),
                bundle_config: Mutex::new(Value::Null),
                bundle: Mutex::new(None),
            })
        }

        fn with_turns(self: Arc<Self>, replies: Vec<Result<String, HostError>>) -> Arc<Self> {
            *self.turn_replies.lock().unwrap() = replies.into();
            self
        }

        fn with_model_replies(
            self: Arc<Self>,
            replies: Vec<Result<String, HostError>>,
        ) -> Arc<Self> {
            *self.model_replies.lock().unwrap() = replies.into();
            self
        }

        fn with_hook_replies(
            self: Arc<Self>,
            replies: Vec<Result<Option<Value>, HostError>>,
        ) -> Arc<Self> {
            self.has_hook.store(true, Ordering::Release);
            *self.hook_replies.lock().unwrap() = replies.into();
            self
        }

        fn with_bundle_config(self: Arc<Self>, cfg: Value) -> Arc<Self> {
            *self.bundle_config.lock().unwrap() = cfg;
            self
        }

        fn with_goal_bundle(self: Arc<Self>, prompt_body: &str) -> Arc<Self> {
            let prepared = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
                "goal-test",
                vec![
                    hya_bundle::SourceFile::new(
                        "bundle.yaml",
                        "kind: Bundle\nschema_version: 2\nidentity: { id: hya/goal, version: 1.0.0, publisher: hya }\nresources:\n  skills:\n    - id: evaluator-prompt\n      path: skills/evaluator-prompt/SKILL.md\n",
                    ),
                    hya_bundle::SourceFile::new(
                        "skills/evaluator-prompt/SKILL.md",
                        format!("---\nname: evaluator-prompt\ndescription: test\n---\n\n{prompt_body}\n"),
                    ),
                ],
            ))
            .unwrap();
            *self.bundle.lock().unwrap() = Some(prepared.bundles()[0].clone());
            self
        }
    }

    #[async_trait]
    impl HostSessionStore for MockHost {
        async fn session_exists(&self, _session: SessionId) -> Result<bool, HostError> {
            Ok(true)
        }

        async fn read_events(
            &self,
            _session: SessionId,
            _after_seq: Option<EventSeq>,
            _limit: Option<usize>,
        ) -> Result<Vec<Envelope>, HostError> {
            Ok(Vec::new())
        }

        async fn append_events(
            &self,
            _session: SessionId,
            _claim: Option<ActorClaim>,
            _events: Vec<Event>,
        ) -> Result<Vec<Envelope>, HostError> {
            Ok(Vec::new())
        }

        async fn append_extension_event(
            &self,
            _session: SessionId,
            _bundle: &str,
            _kind: &str,
            _payload: Value,
            _claim: Option<ActorClaim>,
        ) -> Result<Envelope, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }

        async fn read_projection(&self, _session: SessionId) -> Result<Arc<Projection>, HostError> {
            Ok(Arc::new(self.projection.lock().unwrap().clone()))
        }

        async fn read_extension_state(
            &self,
            _session: SessionId,
            _bundle: &str,
        ) -> Result<Option<Value>, HostError> {
            Ok(None)
        }

        async fn session_metadata(
            &self,
            _session: SessionId,
        ) -> Result<Option<SessionSummaryMeta>, HostError> {
            Ok(None)
        }

        async fn list_sessions(
            &self,
            _filter: SessionListFilter,
        ) -> Result<Vec<SessionSummaryMeta>, HostError> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl HostBundleStorage for MockHost {
        async fn get_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _key: &str,
        ) -> Result<Option<BundleStateEntry>, HostError> {
            Ok(None)
        }

        async fn put_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _key: &str,
            _value: Value,
            _expected_version: Option<u64>,
        ) -> Result<BundleStateEntry, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }

        async fn delete_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _key: &str,
        ) -> Result<bool, HostError> {
            Ok(false)
        }

        async fn list_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _prefix: Option<&str>,
        ) -> Result<Vec<BundleStateEntry>, HostError> {
            Ok(Vec::new())
        }

        async fn acquire_claim(
            &self,
            _request: BundleClaimRequest,
        ) -> Result<BundleClaimLease, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }

        async fn renew_claim(
            &self,
            _lease: &BundleClaimLease,
            _ttl_millis: u64,
        ) -> Result<BundleClaimLease, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }

        async fn release_claim(&self, _lease: &BundleClaimLease) -> Result<(), HostError> {
            Ok(())
        }

        async fn read_claim(
            &self,
            _bundle: &str,
            _resource_key: &str,
        ) -> Result<Option<BundleClaimLease>, HostError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl HostTurnRunner for MockHost {
        async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
            Ok(SessionId::new())
        }

        async fn run_turn(
            &self,
            request: TurnStartRequest,
            cancel: CancellationToken,
        ) -> Result<TurnOutcome, HostError> {
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled);
            }
            self.turn_prompts
                .lock()
                .unwrap()
                .push(request.prompt.clone().unwrap_or_default());
            let reply = self
                .turn_replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok("working".to_string()))?;
            let mut proj = self.projection.lock().unwrap();
            let next_seq = |p: &Projection| p.last_seq + 1;
            let user_msg = MessageId::new();
            let user_part = PartId::new();
            let asst_msg = MessageId::new();
            let asst_part = PartId::new();
            for event in [
                Event::MessageStarted {
                    session: request.session,
                    message: user_msg,
                    role: Role::User,
                    agent: None,
                    model: None,
                },
                Event::TextStart {
                    session: request.session,
                    message: user_msg,
                    part: user_part,
                },
                Event::TextDelta {
                    session: request.session,
                    message: user_msg,
                    part: user_part,
                    delta: request.prompt.unwrap_or_default(),
                },
                Event::MessageFinished {
                    session: request.session,
                    message: user_msg,
                    role: Role::User,
                    finish: FinishReason::Stop,
                    tokens: None,
                    cause: None,
                },
                Event::MessageStarted {
                    session: request.session,
                    message: asst_msg,
                    role: Role::Assistant,
                    agent: request.agent,
                    model: request.model,
                },
                Event::TextStart {
                    session: request.session,
                    message: asst_msg,
                    part: asst_part,
                },
                Event::TextDelta {
                    session: request.session,
                    message: asst_msg,
                    part: asst_part,
                    delta: reply.clone(),
                },
                Event::MessageFinished {
                    session: request.session,
                    message: asst_msg,
                    role: Role::Assistant,
                    finish: FinishReason::Stop,
                    tokens: None,
                    cause: None,
                },
            ] {
                let seq = next_seq(&proj);
                proj.apply(&Envelope {
                    seq: EventSeq(seq),
                    ts_millis: 0,
                    event,
                });
            }
            Ok(TurnOutcome {
                session: request.session,
                message_id: Some(asst_msg),
                finish_reason: Some(FinishReason::Stop),
                output_text: reply,
                error: None,
            })
        }

        async fn spawn_session_and_run(
            &self,
            _request: TurnSpawnRequest,
            _cancel: CancellationToken,
        ) -> Result<TurnOutcome, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }

        async fn steer_turn(&self, _session: SessionId, _message: String) -> Result<(), HostError> {
            Ok(())
        }

        async fn cancel_turn(
            &self,
            _session: SessionId,
            _reason: Option<String>,
        ) -> Result<bool, HostError> {
            Ok(false)
        }

        async fn turn_status(&self, _session: SessionId) -> Result<TurnStatus, HostError> {
            Ok(TurnStatus::Idle)
        }
    }

    #[async_trait]
    impl HostModelCaller for MockHost {
        async fn complete(
            &self,
            request: ModelCallRequest,
            cancel: CancellationToken,
        ) -> Result<ModelCallResponse, HostError> {
            if cancel.is_cancelled() {
                return Err(HostError::Cancelled);
            }
            self.model_requests.lock().unwrap().push(request.clone());
            let text = self
                .model_replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(r#"{"met":false,"reason":"default"}"#.to_string()))?;
            Ok(ModelCallResponse {
                model: request
                    .model
                    .unwrap_or_else(|| ModelRef::new("fake/default")),
                text,
                reasoning: None,
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Stop,
                usage: None,
            })
        }

        async fn stream(
            &self,
            _request: ModelCallRequest,
            _cancel: CancellationToken,
        ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }

        async fn compact_window(
            &self,
            _model: &ModelRef,
            _messages: &[Message],
        ) -> Result<Option<String>, HostError> {
            Ok(None)
        }

        async fn model_window(
            &self,
            _model: &ModelRef,
        ) -> Result<Option<ModelWindowInfo>, HostError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl HostEventBus for MockHost {
        fn publish(&self, _envelope: Envelope) -> Result<(), HostError> {
            Ok(())
        }

        fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
            let (tx, rx) = broadcast::channel(8);
            drop(tx);
            Ok(rx)
        }

        async fn publish_extension(
            &self,
            _session: SessionId,
            _bundle: &str,
            _kind: &str,
            _payload: Value,
        ) -> Result<(), HostError> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostCatalogReader for MockHost {
        async fn list_agents(
            &self,
            _session: Option<SessionId>,
        ) -> Result<Vec<hya_tool::AgentDef>, HostError> {
            Ok(Vec::new())
        }

        async fn get_agent(
            &self,
            _name: &AgentName,
            _session: Option<SessionId>,
        ) -> Result<Option<hya_bundle::PreparedAgent>, HostError> {
            Ok(None)
        }

        async fn list_skills(
            &self,
            _session: Option<SessionId>,
        ) -> Result<Vec<hya_tool::SkillCatalogEntry>, HostError> {
            Ok(Vec::new())
        }

        async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError> {
            Ok(Vec::new())
        }

        async fn get_bundle(&self, id: &str) -> Result<Option<hya_bundle::Bundle>, HostError> {
            if id == BUNDLE_ID {
                Ok(self.bundle.lock().unwrap().clone())
            } else {
                Ok(None)
            }
        }

        async fn bundle_config(&self, bundle_id: &str) -> Result<Value, HostError> {
            if bundle_id == BUNDLE_ID {
                Ok(self.bundle_config.lock().unwrap().clone())
            } else {
                Ok(Value::Null)
            }
        }

        async fn global_config(&self) -> Result<Value, HostError> {
            Ok(Value::Null)
        }
    }

    #[async_trait]
    impl HostExtensionRegistry for MockHost {
        async fn list_extensions(
            &self,
            _kind: Option<ExtensionKind>,
        ) -> Result<Vec<ExtensionEntry>, HostError> {
            Ok(Vec::new())
        }

        async fn has_bundle(&self, _bundle_id: &str) -> Result<bool, HostError> {
            Ok(false)
        }

        async fn invoke_bundle_api(
            &self,
            _call: BundleApiInvocation,
        ) -> Result<BundleApiResult, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }

        async fn invoke_tool(
            &self,
            _session: Option<SessionId>,
            _tool_name: &str,
            _input: Value,
            _cancel: CancellationToken,
        ) -> Result<Value, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }

        async fn has_hook(
            &self,
            _session: Option<SessionId>,
            point: &str,
        ) -> Result<bool, HostError> {
            assert_eq!(point, EVALUATE_HOOK);
            Ok(self.has_hook.load(Ordering::Acquire))
        }

        async fn invoke_hook(
            &self,
            _session: Option<SessionId>,
            point: &str,
            input: Value,
        ) -> Result<Option<Value>, HostError> {
            assert_eq!(point, EVALUATE_HOOK);
            self.hook_transcripts
                .lock()
                .unwrap()
                .push(input["transcript"].as_str().unwrap().to_string());
            self.hook_replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(None))
        }
    }

    impl HostRuntime for MockHost {
        fn session_store(&self) -> &dyn HostSessionStore {
            self
        }

        fn bundle_storage(&self) -> &dyn HostBundleStorage {
            self
        }

        fn turn_runner(&self) -> &dyn HostTurnRunner {
            self
        }

        fn model_caller(&self) -> &dyn HostModelCaller {
            self
        }

        fn event_bus(&self) -> &dyn HostEventBus {
            self
        }

        fn catalog_reader(&self) -> &dyn HostCatalogReader {
            self
        }

        fn extension_registry(&self) -> &dyn HostExtensionRegistry {
            self
        }
    }

    struct ScriptedEval {
        verdicts: Mutex<VecDeque<Verdict>>,
    }

    impl ScriptedEval {
        fn arc(verdicts: Vec<(bool, &str)>) -> Arc<dyn GoalEvaluator> {
            Arc::new(Self {
                verdicts: Mutex::new(
                    verdicts
                        .into_iter()
                        .map(|(met, reason)| Verdict {
                            met,
                            reason: reason.to_string(),
                        })
                        .collect(),
                ),
            })
        }
    }

    #[async_trait]
    impl GoalEvaluator for ScriptedEval {
        async fn evaluate(
            &self,
            _condition: &str,
            _transcript: &str,
            _cancel: &CancellationToken,
        ) -> Result<Verdict, HostError> {
            Ok(self
                .verdicts
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Verdict {
                    met: true,
                    reason: "done".into(),
                }))
        }
    }

    struct WrapFailureExecutor {
        calls: AtomicUsize,
        cancel_on_wrap: bool,
    }

    #[async_trait]
    impl IterationExecutor for WrapFailureExecutor {
        async fn run_iteration(
            &self,
            _directive: &str,
            _cancel: &CancellationToken,
        ) -> Result<String, HostError> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed);
            if n == 0 {
                Ok("first round".to_string())
            } else if self.cancel_on_wrap {
                Err(HostError::Cancelled)
            } else {
                Err(HostError::Unavailable("capacity exceeded".into()))
            }
        }
    }

    struct AlwaysContinueGate;

    #[async_trait]
    impl IterationGate for AlwaysContinueGate {
        async fn judge(
            &self,
            _transcript: &str,
            _cancel: &CancellationToken,
        ) -> Result<GateOutcome, HostError> {
            Ok(GateOutcome::Continue {
                directive: "keep going".into(),
            })
        }
    }

    #[test]
    fn abi_v2_symbol_reports_current_lockstep_digest() {
        let mut out = [0_u8; 32];
        // SAFETY: `out` is a writable 32-byte array.
        unsafe { hya_native_abi_v2(out.as_mut_ptr()) };
        assert_eq!(out, abi_digest_v2());
    }

    #[tokio::test]
    async fn goal_service_tracks_start_stop_and_health() {
        let service = GoalDriverService::new();
        assert_eq!(service.id(), GOAL_SERVICE_ID);
        assert!(!service.health().await.healthy);

        let host: Arc<dyn HostRuntime> = MockHost::new();
        service.start(host).await.unwrap();
        assert!(service.health().await.healthy);
        assert_eq!(service.health().await.status, "ready");

        service.stop().await.unwrap();
        assert!(!service.health().await.healthy);
        assert_eq!(service.health().await.status, "stopped");
    }

    #[test]
    fn goal_contract_validation_accepts_freeform_and_validates_structured_sections() {
        assert!(validate_goal_condition("make the tests pass").is_ok());
        assert!(
            validate_goal_condition("## Objective\nShip it.\n## Boundaries\nrepo only.\n").is_err()
        );
        assert!(
            validate_goal_condition(
                "## Objective\nShip it.\n## Success criteria\nall green.\n## Verification\nrun the suite somehow.\n## Boundaries\nrepo only.\n## Stop conditions\ntwo failures.\n"
            )
            .is_err()
        );
        assert!(
            validate_goal_condition(
                "## Objective\nShip it.\n## Success criteria\nall green.\n## Verification\n`cargo test`\n## Boundaries\nrepo only.\n## Stop conditions\ntwo failures.\n"
            )
            .is_ok()
        );
    }

    #[tokio::test]
    async fn goal_driver_achieves_with_model_evaluator_and_bundle_skill_prompt() {
        let host = MockHost::new()
            .with_turns(vec![Ok("step 1".into()), Ok("step 2".into())])
            .with_model_replies(vec![
                Ok(r#"{"met":false,"reason":"need tests"}"#.into()),
                Ok(r#"{"met":true,"reason":"all tests green"}"#.into()),
            ])
            .with_bundle_config(json!({"goal": {"evaluator_model": "deep/judge"}}))
            .with_goal_bundle("CUSTOM_EVALUATOR_PROMPT_BODY");

        let driver = NativeGoalDriver;
        assert_eq!(hya_runtime_abi::NativeDriver::name(&driver), GOAL_DRIVER_ID);
        let outcome = run_goal_request(
            host.clone(),
            GoalRunRequest {
                session: SessionId::new(),
                goal: "make tests pass".into(),
                max_iterations: 5,
                max_wall_clock_secs: 60,
                agent: Some(AgentName::new("hya-main")),
                model: Some(ModelRef::new("worker/model")),
                config: Value::Null,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            DriverRunOutcome::Achieved {
                iterations: 2,
                reason: "all tests green".into(),
            }
        );

        let reqs = host.model_requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].model, Some(ModelRef::new("deep/judge")));
        assert_eq!(
            reqs[0].system.as_deref(),
            Some("CUSTOM_EVALUATOR_PROMPT_BODY")
        );
        assert!(reqs[0].tools.is_empty());
    }

    #[tokio::test]
    async fn cli_evaluator_model_override_outranks_bundle_config() {
        let host = MockHost::new()
            .with_turns(vec![Ok("done".into())])
            .with_model_replies(vec![Ok(r#"{"met":true,"reason":"ok"}"#.into())])
            .with_bundle_config(json!({"goal": {"evaluator_model": "bundle/judge"}}));

        let outcome = run_goal_request(
            host.clone(),
            GoalRunRequest {
                session: SessionId::new(),
                goal: "ship it".into(),
                max_iterations: 3,
                max_wall_clock_secs: 60,
                agent: None,
                model: Some(ModelRef::new("worker/model")),
                config: json!({"evaluator_model": "cli/judge"}),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            DriverRunOutcome::Achieved { iterations: 1, .. }
        ));
        let reqs = host.model_requests.lock().unwrap();
        assert_eq!(reqs[0].model, Some(ModelRef::new("cli/judge")));
    }

    #[tokio::test]
    async fn malformed_model_output_caps_with_budget_wrap_up() {
        let host = MockHost::new()
            .with_turns(vec![
                Ok("first".into()),
                Ok("second".into()),
                Ok("handoff".into()),
            ])
            .with_model_replies(vec![Ok("not json".into()), Ok("still not json".into())]);

        let outcome = run_goal_request(
            host.clone(),
            GoalRunRequest {
                session: SessionId::new(),
                goal: "do the thing".into(),
                max_iterations: 2,
                max_wall_clock_secs: 60,
                agent: None,
                model: Some(ModelRef::new("fake/model")),
                config: Value::Null,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            DriverRunOutcome::BudgetLimited {
                iterations: 2,
                which: "max_iterations".into(),
            }
        );
        let prompts = host.turn_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 3);
        assert!(
            prompts[2].contains("BUDGET LIMIT REACHED"),
            "expected wrap-up directive, got: {}",
            prompts[2]
        );
    }

    #[tokio::test]
    async fn plugin_evaluator_hook_outranks_model_and_falls_back_on_malformed_or_error() {
        let host = MockHost::new()
            .with_turns(vec![Ok("step 1".into()), Ok("step 2".into())])
            .with_hook_replies(vec![
                Ok(Some(
                    serde_json::json!({"met": false, "reason": "add meow"}),
                )),
                Ok(Some(
                    serde_json::json!({"met": true, "reason": "hook approved"}),
                )),
            ]);

        let outcome = run_goal_request(
            host.clone(),
            GoalRunRequest {
                session: SessionId::new(),
                goal: "write readme".into(),
                max_iterations: 4,
                max_wall_clock_secs: 60,
                agent: None,
                model: None,
                config: Value::Null,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            DriverRunOutcome::Achieved {
                iterations: 2,
                reason: "hook approved".into(),
            }
        );
        assert!(host.model_requests.lock().unwrap().is_empty());
        {
            let prompts = host.turn_prompts.lock().unwrap();
            assert!(prompts[1].contains("add meow"), "{}", prompts[1]);
        }

        // Malformed hook falls back to ModelGoalEvaluator.
        let fb_host = MockHost::new()
            .with_hook_replies(vec![Ok(Some(serde_json::json!({"verdict": "garbled"})))])
            .with_model_replies(vec![Ok(r#"{"met":true,"reason":"fallback ok"}"#.into())]);
        let fallback = Arc::new(ModelGoalEvaluator::new(fb_host.clone(), None));
        let eval = PluginGoalEvaluator::new(fb_host.clone(), None).with_fallback(fallback);
        assert_eq!(
            eval.evaluate("goal", "transcript", &CancellationToken::new())
                .await
                .unwrap(),
            Verdict {
                met: true,
                reason: "fallback ok".into(),
            }
        );

        // Standalone poisoned hook without fallback degrades cleanly to not-met.
        let err_host =
            MockHost::new().with_hook_replies(vec![Err(HostError::Internal("poisoned".into()))]);
        let eval_no_fb = PluginGoalEvaluator::new(err_host, None);
        assert_eq!(
            eval_no_fb
                .evaluate("goal", "transcript", &CancellationToken::new())
                .await
                .unwrap(),
            Verdict {
                met: false,
                reason: PLUGIN_HOOK_ERROR_REASON.into(),
            }
        );
    }

    #[tokio::test]
    async fn stall_guard_pre_cancel_and_zero_iteration_cap() {
        let host = MockHost::new().with_turns(vec![
            Ok("working".into()),
            Ok("working".into()),
            Ok("working".into()),
        ]);
        let stalled = run_goal(
            host.clone(),
            SessionId::new(),
            None,
            None,
            "do the thing".into(),
            ScriptedEval::arc(vec![
                (false, "not yet"),
                (false, "not yet"),
                (false, "not yet"),
            ]),
            SafetyCaps::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(stalled, DriverRunOutcome::Stalled { iterations: 3 });

        let cancel = CancellationToken::new();
        cancel.cancel();
        let cancelled = run_goal(
            MockHost::new(),
            SessionId::new(),
            None,
            None,
            "x".into(),
            ScriptedEval::arc(vec![(true, "ok")]),
            SafetyCaps::default(),
            cancel,
        )
        .await
        .unwrap();
        assert_eq!(cancelled, DriverRunOutcome::Cancelled);

        let capped = run_goal(
            MockHost::new(),
            SessionId::new(),
            None,
            None,
            "x".into(),
            ScriptedEval::arc(vec![(true, "ok")]),
            SafetyCaps {
                max_iterations: 0,
                ..SafetyCaps::default()
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            capped,
            DriverRunOutcome::Capped {
                iterations: 0,
                which: "max_iterations".into(),
            }
        );
    }

    #[tokio::test]
    async fn wrap_up_failure_propagates_error_and_preserves_cancellation() {
        let caps = SafetyCaps {
            max_iterations: 1,
            ..SafetyCaps::default()
        };
        let err_res = IterationDriver::new(caps)
            .run(
                &WrapFailureExecutor {
                    calls: AtomicUsize::new(0),
                    cancel_on_wrap: false,
                },
                &AlwaysContinueGate,
                "work".into(),
                CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(err_res, Err(HostError::Unavailable(_))),
            "{err_res:?}"
        );

        let cancel_res = IterationDriver::new(caps)
            .run(
                &WrapFailureExecutor {
                    calls: AtomicUsize::new(0),
                    cancel_on_wrap: true,
                },
                &AlwaysContinueGate,
                "work".into(),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(cancel_res.unwrap(), DriverRunOutcome::Cancelled);
    }
}
