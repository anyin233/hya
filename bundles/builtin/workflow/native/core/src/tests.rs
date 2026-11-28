#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_bundle::{
    ApiMethod, ApiScope, Bundle, BundleSource, PreparedAgent, SourceFile, prepare_package,
};
use hya_proto::{
    ActorClaim, AgentName, Envelope, Event, EventSeq, MemberId, ModelRef, OwnerRunId, Projection,
    SessionId, SessionKind,
};
use hya_runtime_abi::{
    BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
    BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionEventContext, ExtensionKind,
    HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
    HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, ManagedMemberActivateRequest,
    ManagedMemberActivationOutcome, ManagedMemberActivationStatus, ManagedMemberHandle,
    ManagedMemberSpawnRequest, ModelCallRequest, ModelCallResponse, ModelRouteEntry,
    ModelStreamChunk, ModelWindowInfo, NativeBundleApiHandler, NativeBundleApiRequest,
    NativeService, ProjectionReducer, ResolvedModelRoute, ResolvedModelRouteCandidate,
    ScopeCatalogSnapshot, SessionListFilter, SessionSummaryMeta, TurnOutcome,
    TurnRouteFailureClass, TurnRouteOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus,
};
use hya_tool::{AgentDef, SkillCatalogEntry};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::api::WorkflowBundleApiHandler;
use crate::catalog::{
    bundle_workflow_revision, discover_workflow_files_in_root, load_workflow_file,
    workflow_dirs_for_workdir, workflow_project_dir,
};
use crate::compile::{WorkflowSource, compile};
use crate::error::{
    WorkflowControlError, WorkflowError, map_workflow_error_with_detail,
    map_workflow_preflight_error,
};
use crate::events::WorkflowEvent;
use crate::model::{
    WorkflowCommand, WorkflowCommandResult, WorkflowDelivery, WorkflowIdentity, WorkflowMemberRole,
    WorkflowProjection, WorkflowRevision, WorkflowRouteFailureClass, WorkflowRunId,
    WorkflowRunStatus, WorkflowSourceId, WorkflowStageStatus,
};
use crate::reducer::{
    WorkflowProjectionReducer, decode_workflow_projection, extension_event_parts,
    workflow_event_run,
};
use crate::service::{
    WORKFLOW_BUNDLE_ID, WorkflowInvocation, WorkflowService, workflow_request_hash,
};

fn test_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "hya-workflow-bundle-{label}-{}-{nonce}",
        std::process::id()
    ))
}

#[derive(Default)]
struct MockState {
    owner: Option<OwnerRunId>,
    sessions: BTreeMap<SessionId, Vec<Envelope>>,
    workdir: PathBuf,
    bundles: Vec<Bundle>,
    activations: Vec<String>,
    route_outcomes: Vec<TurnRouteOutcome>,
    activation_replies: Mutex<Vec<String>>,
    deny_agent: Option<String>,
    project_roots: Vec<PathBuf>,
    diagnostics: Vec<String>,
}

struct MockHostRuntime {
    state: Arc<Mutex<MockState>>,
    bus: broadcast::Sender<Envelope>,
}

impl MockHostRuntime {
    fn new(workdir: PathBuf) -> Self {
        let (bus, _) = broadcast::channel(64);
        let mut st = MockState {
            owner: Some(OwnerRunId::new()),
            workdir,
            ..Default::default()
        };
        st.bundles = Vec::new();
        Self {
            state: Arc::new(Mutex::new(st)),
            bus,
        }
    }

    fn seed_session(&self, session: SessionId, agent: &str, workdir: &Path) {
        let mut st = self.state.lock().unwrap();
        let env = Envelope {
            seq: EventSeq(1),
            ts_millis: 1,
            event: Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new(agent),
                model: ModelRef::new("fake/model"),
                workdir: workdir.display().to_string(),
                project: None,
                kind: SessionKind::Project,
            },
        };
        st.sessions.insert(session, vec![env]);
    }

    fn push_reply(&self, text: &str) {
        let st = self.state.lock().unwrap();
        st.activation_replies.lock().unwrap().push(text.to_string());
    }
}

impl HostRuntime for MockHostRuntime {
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

#[async_trait]
impl HostSessionStore for MockHostRuntime {
    async fn session_exists(&self, session: SessionId) -> Result<bool, HostError> {
        Ok(self.state.lock().unwrap().sessions.contains_key(&session))
    }

    async fn read_events(
        &self,
        session: SessionId,
        _after_seq: Option<EventSeq>,
        _limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .sessions
            .get(&session)
            .cloned()
            .unwrap_or_default())
    }

    async fn append_events(
        &self,
        session: SessionId,
        _claim: Option<ActorClaim>,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        let mut st = self.state.lock().unwrap();
        let log = st.sessions.entry(session).or_default();
        let mut out = Vec::new();
        for event in events {
            let seq = EventSeq((log.len() + 1) as u64);
            let env = Envelope {
                seq,
                ts_millis: 100,
                event,
            };
            log.push(env.clone());
            let _ = self.bus.send(env.clone());
            out.push(env);
        }
        Ok(out)
    }

    async fn append_extension_event(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
        claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError> {
        let mut out = self
            .append_events(
                session,
                claim,
                vec![Event::Extension {
                    session,
                    bundle: bundle.to_string(),
                    kind: kind.to_string(),
                    payload,
                }],
            )
            .await?;
        Ok(out.remove(0))
    }

    async fn read_projection(&self, session: SessionId) -> Result<Arc<Projection>, HostError> {
        let events = self.read_events(session, None, None).await?;
        let mut proj = Projection::from_events(&events);
        let reducer = WorkflowProjectionReducer;
        let mut ext_state = reducer.initial_state();
        for env in &events {
            if let Event::Extension {
                session: ev_sess,
                bundle,
                kind,
                payload,
            } = &env.event
                && bundle == WORKFLOW_BUNDLE_ID
            {
                reducer.apply(
                    &mut ext_state,
                    &ExtensionEventContext {
                        session: *ev_sess,
                        seq: env.seq,
                        ts_millis: env.ts_millis,
                        bundle: bundle.clone(),
                        kind: kind.clone(),
                        payload: payload.clone(),
                    },
                )?;
            }
        }
        if !ext_state.is_null() {
            proj.extensions
                .insert(WORKFLOW_BUNDLE_ID.to_string(), ext_state);
        }
        Ok(Arc::new(proj))
    }

    async fn read_extension_state(
        &self,
        session: SessionId,
        bundle: &str,
    ) -> Result<Option<Value>, HostError> {
        let proj = self.read_projection(session).await?;
        Ok(proj.extensions.get(bundle).cloned())
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

    fn runtime_owner_id(&self) -> Option<OwnerRunId> {
        self.state.lock().unwrap().owner
    }

    async fn find_sessions_with_extension(
        &self,
        bundle: &str,
        kind: &str,
    ) -> Result<Vec<SessionId>, HostError> {
        let st = self.state.lock().unwrap();
        let mut out = Vec::new();
        for (sess, log) in &st.sessions {
            if log.iter().any(|env| {
                matches!(
                    &env.event,
                    Event::Extension {
                        bundle: b,
                        kind: k,
                        ..
                    } if b == bundle && k == kind
                )
            }) {
                out.push(*sess);
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl HostBundleStorage for MockHostRuntime {
    async fn get_state(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<Option<BundleStateEntry>, HostError> {
        Ok(None)
    }
    async fn put_state(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Value,
        _: Option<u64>,
    ) -> Result<BundleStateEntry, HostError> {
        Err(HostError::Unavailable("unused".into()))
    }
    async fn delete_state(&self, _: &str, _: &str, _: &str) -> Result<bool, HostError> {
        Ok(false)
    }
    async fn list_state(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, HostError> {
        Ok(Vec::new())
    }
    async fn acquire_claim(&self, _: BundleClaimRequest) -> Result<BundleClaimLease, HostError> {
        Err(HostError::Unavailable("unused".into()))
    }
    async fn renew_claim(
        &self,
        _: &BundleClaimLease,
        _: u64,
    ) -> Result<BundleClaimLease, HostError> {
        Err(HostError::Unavailable("unused".into()))
    }
    async fn release_claim(&self, _: &BundleClaimLease) -> Result<(), HostError> {
        Ok(())
    }
    async fn read_claim(&self, _: &str, _: &str) -> Result<Option<BundleClaimLease>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl HostTurnRunner for MockHostRuntime {
    async fn spawn_session(&self, _: TurnSpawnRequest) -> Result<SessionId, HostError> {
        Ok(SessionId::new())
    }
    async fn run_turn(
        &self,
        req: TurnStartRequest,
        _: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Ok(TurnOutcome {
            session: req.session,
            message_id: None,
            finish_reason: None,
            output_text: "ok".into(),
            error: None,
        })
    }
    async fn spawn_session_and_run(
        &self,
        _: TurnSpawnRequest,
        _: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Ok(TurnOutcome {
            session: SessionId::new(),
            message_id: None,
            finish_reason: None,
            output_text: "ok".into(),
            error: None,
        })
    }
    async fn steer_turn(&self, _: SessionId, _: String) -> Result<(), HostError> {
        Ok(())
    }
    async fn cancel_turn(&self, _: SessionId, _: Option<String>) -> Result<bool, HostError> {
        Ok(false)
    }
    async fn turn_status(&self, _: SessionId) -> Result<TurnStatus, HostError> {
        Ok(TurnStatus::Idle)
    }
    async fn spawn_managed_member(
        &self,
        request: ManagedMemberSpawnRequest,
    ) -> Result<ManagedMemberHandle, HostError> {
        let session = SessionId::new();
        Ok(ManagedMemberHandle {
            session,
            handle: format!("member-{}", request.agent),
            member: MemberId::new(),
        })
    }
    async fn activate_managed_member(
        &self,
        request: ManagedMemberActivateRequest,
        _cancel: CancellationToken,
    ) -> Result<ManagedMemberActivationOutcome, HostError> {
        let (reply, route_outcomes) = {
            let mut st = self.state.lock().unwrap();
            st.activations.push(request.agent.to_string());
            let mut replies = st.activation_replies.lock().unwrap();
            let reply = if replies.is_empty() {
                format!("done:{}", request.agent)
            } else {
                replies.remove(0)
            };
            (reply, st.route_outcomes.clone())
        };
        Ok(ManagedMemberActivationOutcome {
            status: ManagedMemberActivationStatus::Done,
            output_text: reply,
            route_outcomes,
        })
    }
    async fn end_managed_member(&self, _parent: SessionId, _handle: &str) -> Result<(), HostError> {
        Ok(())
    }
}

#[async_trait]
impl HostModelCaller for MockHostRuntime {
    async fn complete(
        &self,
        _: ModelCallRequest,
        _: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        Err(HostError::Unavailable("unused".into()))
    }
    async fn stream(
        &self,
        _: ModelCallRequest,
        _: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
        Err(HostError::Unavailable("unused".into()))
    }
    async fn compact_window(
        &self,
        _: &ModelRef,
        _: &[hya_proto::Message],
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }
    async fn model_window(&self, _: &ModelRef) -> Result<Option<ModelWindowInfo>, HostError> {
        Ok(None)
    }
    async fn resolve_model_route(
        &self,
        entries: &[ModelRouteEntry],
    ) -> Result<ResolvedModelRoute, HostError> {
        let candidates = entries
            .iter()
            .map(|e| ResolvedModelRouteCandidate {
                model: e.model.clone(),
                reasoning: e.reasoning.clone().unwrap_or_else(|| "off".into()),
            })
            .collect();
        Ok(ResolvedModelRoute {
            candidates,
            selected_index: 0,
        })
    }
}

#[async_trait]
impl HostEventBus for MockHostRuntime {
    fn publish(&self, envelope: Envelope) -> Result<(), HostError> {
        let _ = self.bus.send(envelope);
        Ok(())
    }
    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
        Ok(self.bus.subscribe())
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
impl HostCatalogReader for MockHostRuntime {
    async fn list_agents(&self, _: Option<SessionId>) -> Result<Vec<AgentDef>, HostError> {
        Ok(Vec::new())
    }
    async fn get_agent(
        &self,
        _: &AgentName,
        _: Option<SessionId>,
    ) -> Result<Option<PreparedAgent>, HostError> {
        Ok(None)
    }
    async fn list_skills(&self, _: Option<SessionId>) -> Result<Vec<SkillCatalogEntry>, HostError> {
        Ok(Vec::new())
    }
    async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError> {
        Ok(Vec::new())
    }
    async fn get_bundle(&self, _: &str) -> Result<Option<Bundle>, HostError> {
        Ok(None)
    }
    async fn bundle_config(&self, _: &str) -> Result<Value, HostError> {
        Ok(Value::Null)
    }
    async fn global_config(&self) -> Result<Value, HostError> {
        Ok(Value::Null)
    }
    async fn resolve_scope_catalog(
        &self,
        _session: Option<SessionId>,
        directory: Option<&Path>,
    ) -> Result<ScopeCatalogSnapshot, HostError> {
        let st = self.state.lock().unwrap();
        Ok(ScopeCatalogSnapshot {
            workdir: directory
                .map(Path::to_path_buf)
                .unwrap_or_else(|| st.workdir.clone()),
            project_roots: st.project_roots.clone(),
            bundles: st.bundles.clone(),
            runtime_fingerprint: "test-fingerprint-v1".to_string(),
            diagnostics: st.diagnostics.clone(),
        })
    }
    async fn authorize_agent_run(
        &self,
        _session: SessionId,
        _caller: &AgentName,
        agent: &AgentName,
    ) -> Result<(), HostError> {
        let st = self.state.lock().unwrap();
        if st.deny_agent.as_deref() == Some(agent.as_str()) {
            return Err(HostError::PermissionDenied(format!(
                "agent `{agent}` not allowed"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl HostExtensionRegistry for MockHostRuntime {
    async fn list_extensions(
        &self,
        _: Option<ExtensionKind>,
    ) -> Result<Vec<ExtensionEntry>, HostError> {
        Ok(Vec::new())
    }
    async fn has_bundle(&self, _: &str) -> Result<bool, HostError> {
        Ok(false)
    }
    async fn invoke_bundle_api(
        &self,
        _: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError> {
        Err(HostError::Unavailable("unused".into()))
    }
    async fn invoke_tool(
        &self,
        _: Option<SessionId>,
        _: &str,
        _: Value,
        _: CancellationToken,
    ) -> Result<Value, HostError> {
        Err(HostError::Unavailable("unused".into()))
    }
}

const DEMO_WORKFLOW: &str = r#"---
kind: Workflow
name: demo
description: Two-stage demo workflow.
inputs:
  topic: Topic to process.
nodes:
  plan:
    agent: planner
    directive: "Plan {{input.topic}}"
  execute:
    agent: worker
    directive: "Execute {{input.topic}}"
---
flowchart TD
  plan --> execute
"#;

#[test]
fn markdown_workflow_compiles_and_discovers_from_disk() {
    let root = test_root("discovery");
    let wf_dir = workflow_project_dir(&root);
    std::fs::create_dir_all(&wf_dir).unwrap();
    for name in ["z.hya.md", "a.hya.md", "legacy.yaml", "notes.md"] {
        std::fs::write(wf_dir.join(name), DEMO_WORKFLOW).unwrap();
    }
    let files = discover_workflow_files_in_root(&wf_dir);
    let names: Vec<_> = files
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    assert_eq!(names, ["a.hya.md", "z.hya.md"]);

    let compiled = load_workflow_file(&wf_dir.join("a.hya.md")).unwrap();
    assert_eq!(compiled.definition().name(), "demo");
    assert_eq!(workflow_dirs_for_workdir(&root)[0], wf_dir);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn process_command_changes_packaged_workflow_revision() {
    let workflow_source = "---\nkind: Workflow\nname: demo\ndescription: Digest revision.\nnodes:\n  execute:\n    agent: worker\n    directive: Execute.\n---\nflowchart TD\n  execute\n";
    let prepare = |command: &str| {
        let manifest = format!(
            "kind: Bundle\nschema_version: 2\nidentity: {{id: hya/demo, version: 1.0.0, publisher: hya}}\nagents: [{{id: worker, role: subagent}}]\nworkflow: {{id: demo, path: workflows/demo.hya.md}}\nruntime: {{kind: js, mode: process, command: [{command}]}}\n"
        );
        prepare_package(BundleSource::new(
            "revision",
            vec![
                SourceFile::new("bundle.yaml", manifest.into_bytes()),
                SourceFile::new("workflows/demo.hya.md", workflow_source.as_bytes()),
            ],
        ))
        .unwrap()
    };
    let before = prepare("provider-before");
    let after = prepare("provider-after");
    let compiled = compile(WorkflowSource::new(
        "workflows/demo.hya.md",
        workflow_source,
    ))
    .unwrap();
    assert_ne!(
        bundle_workflow_revision(&compiled, before.bundles()[0].digest()),
        bundle_workflow_revision(&compiled, after.bundles()[0].digest()),
    );
}

#[test]
fn preflight_and_capacity_error_mapping() {
    let invalid = map_workflow_preflight_error(WorkflowError::Invalid {
        workflow: "demo".into(),
        detail: "bad topology".into(),
    });
    assert_eq!(invalid.code(), "WORKFLOW_INVALID_SOURCE");

    let cap_pre = map_workflow_preflight_error(WorkflowError::CapacityExceeded("full".into()));
    let cap_run = map_workflow_error_with_detail(
        WorkflowError::CapacityExceeded("full".into()),
        "full".into(),
    );
    assert_eq!(cap_pre.code(), "WORKFLOW_OVERLOADED");
    assert_eq!(cap_run.code(), "WORKFLOW_OVERLOADED");

    let run = WorkflowRunId::new();
    let (session, bundle, kind, payload) =
        extension_event_parts(WorkflowEvent::WorkflowStageRouteOutcome {
            session: SessionId::new(),
            run,
            stage: "execute".into(),
            member: MemberId::new(),
            role: WorkflowMemberRole::Worker,
            iteration: 0,
            step: 0,
            candidate_index: 0,
            model: ModelRef::new("fake/model"),
            reasoning: "off".into(),
            failure_class: WorkflowRouteFailureClass::None,
        })
        .unwrap();
    let event = Event::Extension {
        session,
        bundle,
        kind,
        payload,
    };
    assert_eq!(workflow_event_run(&event), Some(run));
}

#[test]
fn request_hash_is_deterministic_and_sensitive_to_fingerprint() {
    let wf = WorkflowIdentity {
        source: WorkflowSourceId::new("project:demo.hya.md"),
        name: "demo".into(),
        revision: WorkflowRevision::from_bytes([7; 32]),
    };
    let inputs = BTreeMap::from([("topic".to_string(), "rust".to_string())]);
    let h1 = workflow_request_hash(&wf, "hya-main", &inputs, "fp-a");
    let h2 = workflow_request_hash(&wf, "hya-main", &inputs, "fp-b");
    assert_ne!(h1, h2);
    assert_eq!(h1, workflow_request_hash(&wf, "hya-main", &inputs, "fp-a"));
}

#[tokio::test]
async fn service_executes_dag_persists_route_outcomes_and_replays_idempotently() {
    let root = test_root("dag-exec");
    let wf_dir = workflow_project_dir(&root);
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(wf_dir.join("demo.hya.md"), DEMO_WORKFLOW).unwrap();

    let runtime = Arc::new(MockHostRuntime::new(root.clone()));
    runtime
        .state
        .lock()
        .unwrap()
        .route_outcomes
        .push(TurnRouteOutcome {
            step: 0,
            candidate_index: 1,
            model: ModelRef::new("fake/fallback"),
            reasoning: "medium".into(),
            failure_class: TurnRouteFailureClass::Server,
        });
    let session = SessionId::new();
    runtime.seed_session(session, "hya-main", &root);

    let service = Arc::new(WorkflowService::new());
    service.start(runtime.clone()).await.unwrap();

    let run_id = WorkflowRunId::new();
    let result = service
        .execute(
            session,
            WorkflowInvocation {
                caller: None,
                operation: None,
                delivery: WorkflowDelivery::Finished,
            },
            WorkflowCommand::Run {
                name: Some("demo".into()),
                expected_revision: None,
                inputs: BTreeMap::from([("topic".into(), "extract".into())]),
                run: Some(run_id),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let WorkflowCommandResult::Run { result } = result else {
        panic!("expected Run result");
    };
    assert!(!result.replayed);
    assert_eq!(result.run.status, WorkflowRunStatus::Completed);
    assert_eq!(result.run.stages.len(), 2);
    assert_eq!(result.run.stages[0].status, WorkflowStageStatus::Completed);
    assert_eq!(result.run.stages[1].status, WorkflowStageStatus::Completed);
    assert_eq!(result.run.stages[0].route_outcomes.len(), 1);
    assert_eq!(
        result.run.stages[0].route_outcomes[0].failure_class,
        WorkflowRouteFailureClass::Server
    );
    assert_eq!(
        runtime.state.lock().unwrap().activations,
        vec!["planner", "worker"]
    );

    // Identical retry replays without re-running.
    let replay = service
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Run {
                name: Some("demo".into()),
                expected_revision: None,
                inputs: BTreeMap::from([("topic".into(), "extract".into())]),
                run: Some(run_id),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let WorkflowCommandResult::Run { result: replay } = replay else {
        panic!("expected Run replay");
    };
    assert!(replay.replayed);
    assert_eq!(replay.run.id, run_id);

    // Conflicting retry with different inputs is rejected.
    let err = service
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Run {
                name: Some("demo".into()),
                expected_revision: None,
                inputs: BTreeMap::from([("topic".into(), "different".into())]),
                run: Some(run_id),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        WorkflowControlError::OperationConflict { .. }
    ));

    service.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn service_recovers_stale_running_workflow_on_restart() {
    let root = test_root("restart-recovery");
    std::fs::create_dir_all(&root).unwrap();
    let runtime = Arc::new(MockHostRuntime::new(root.clone()));
    let session = SessionId::new();
    runtime.seed_session(session, "hya-main", &root);

    let prior_owner = OwnerRunId::new();
    let stale_run = WorkflowRunId::new();
    let (sess, bundle, kind, payload) = extension_event_parts(WorkflowEvent::WorkflowRunStarted {
        session,
        run: stale_run,
        workflow: WorkflowIdentity {
            source: WorkflowSourceId::new("project:demo.hya.md"),
            name: "demo".into(),
            revision: WorkflowRevision::from_bytes([1; 32]),
        },
        request_hash: "abc".into(),
        owner: prior_owner,
        stages: Vec::new(),
    })
    .unwrap();
    runtime
        .session_store()
        .append_extension_event(sess, &bundle, &kind, payload, None)
        .await
        .unwrap();

    let service = Arc::new(WorkflowService::new());
    service.start(runtime.clone()).await.unwrap();

    let state = decode_workflow_projection(
        runtime
            .session_store()
            .read_extension_state(session, WORKFLOW_BUNDLE_ID)
            .await
            .unwrap(),
    );
    assert_eq!(
        state.run.as_ref().map(|r| r.status),
        Some(WorkflowRunStatus::Interrupted)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn bundle_api_handler_serves_workflows_state_and_commands() {
    let root = test_root("bundle-api");
    let wf_dir = workflow_project_dir(&root);
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(wf_dir.join("demo.hya.md"), DEMO_WORKFLOW).unwrap();

    let runtime = Arc::new(MockHostRuntime::new(root.clone()));
    let session = SessionId::new();
    runtime.seed_session(session, "hya-main", &root);

    let service = Arc::new(WorkflowService::new());
    service.start(runtime.clone()).await.unwrap();
    let api = WorkflowBundleApiHandler::new(Arc::clone(&service));

    // Reject relative directory.
    let bad = api
        .handle(NativeBundleApiRequest {
            api: "list_workflows".into(),
            method: ApiMethod::Get,
            scope: ApiScope::Global,
            path: "/workflows".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::from([("directory".into(), "relative/path".into())]),
            body: Value::Null,
            session: None,
        })
        .await
        .unwrap();
    assert_eq!(bad.status, 400);
    assert_eq!(bad.body["error"]["code"], "invalid_argument");

    // List workflows in absolute directory.
    let listed = api
        .handle(NativeBundleApiRequest {
            api: "list_workflows".into(),
            method: ApiMethod::Get,
            scope: ApiScope::Global,
            path: "/workflows".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::from([("directory".into(), root.display().to_string())]),
            body: Value::Null,
            session: None,
        })
        .await
        .unwrap();
    assert_eq!(listed.status, 200);
    assert_eq!(listed.body["workflows"][0]["name"], "demo");

    // Select workflow via /command.
    let selected = api
        .handle(NativeBundleApiRequest {
            api: "submit_workflow_command".into(),
            method: ApiMethod::Post,
            scope: ApiScope::Session,
            path: "/command".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: json!({ "select": { "name": "demo" } }),
            session: Some(session),
        })
        .await
        .unwrap();
    assert_eq!(selected.status, 200);
    assert_eq!(selected.body["selected"]["selection"]["name"], "demo");

    // Read state via /state.
    let state = api
        .handle(NativeBundleApiRequest {
            api: "get_workflow_state".into(),
            method: ApiMethod::Get,
            scope: ApiScope::Session,
            path: "/state".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: Value::Null,
            session: Some(session),
        })
        .await
        .unwrap();
    assert_eq!(state.status, 200);
    let parsed: WorkflowProjection = serde_json::from_value(state.body).unwrap();
    assert_eq!(
        parsed.selection.as_ref().map(|s| s.name.as_str()),
        Some("demo")
    );

    service.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn loop_stage_iterates_until_verifier_accepts() {
    let root = test_root("loop-stage");
    let wf_dir = workflow_project_dir(&root);
    std::fs::create_dir_all(&wf_dir).unwrap();
    let loop_wf = r#"---
kind: Workflow
name: looped
description: Loop stage with independent verifier.
nodes:
  refine:
    agent: worker
    mode: loop
    directive: Produce refined answer.
    verify:
      agent: checker
      until: Answer says READY.
      max_iterations: 3
---
flowchart TD
  refine
"#;
    std::fs::write(wf_dir.join("looped.hya.md"), loop_wf).unwrap();

    let runtime = Arc::new(MockHostRuntime::new(root.clone()));
    let session = SessionId::new();
    runtime.seed_session(session, "hya-main", &root);
    runtime.push_reply("draft 1");
    runtime.push_reply(r#"{"met": false, "reason": "missing READY"}"#);
    runtime.push_reply("READY now");
    runtime.push_reply(r#"{"met": true, "reason": "contains READY"}"#);

    let service = Arc::new(WorkflowService::new());
    service.start(runtime.clone()).await.unwrap();

    let result = service
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Run {
                name: Some("looped".into()),
                expected_revision: None,
                inputs: BTreeMap::new(),
                run: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let WorkflowCommandResult::Run { result } = result else {
        panic!("expected Run");
    };
    assert_eq!(result.run.status, WorkflowRunStatus::Completed);
    assert_eq!(
        runtime.state.lock().unwrap().activations,
        vec!["worker", "checker", "worker", "checker"]
    );
    service.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn prepared_compiler_revision_matches_the_bundle_compiler() {
    let bundles = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    for dir in ["builtin/workflow", "examples/argus-example"] {
        let catalog =
            prepare_package(BundleSource::read_directory(bundles.join(dir)).unwrap()).unwrap();
        let prepared = catalog.bundles()[0].workflow().unwrap();
        let compiled =
            compile(WorkflowSource::new(&prepared.source_path, &prepared.source)).unwrap();
        assert_eq!(
            prepared.compiler_revision,
            compiled.revision().to_string(),
            "{dir}"
        );
    }
}

/// An event log written with the pre-bundle `hya_tool` event types, and the
/// projection `hya_tool`'s reducer folded from it.
const RECORDED_EVENTS: &str = include_str!("../tests/fixtures/workflow_events.json");

#[test]
fn recorded_event_log_folds_to_the_recorded_projection() {
    let recorded: Value = serde_json::from_str(RECORDED_EVENTS).unwrap();
    let session: SessionId = serde_json::from_value(recorded["session"].clone()).unwrap();
    let events = recorded["events"].as_array().unwrap();
    let kinds: std::collections::BTreeSet<_> = events
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.len(), 7, "{kinds:?}");

    let mut state = WorkflowProjectionReducer.initial_state();
    for event in events {
        let kind = event["kind"].as_str().unwrap();
        let decoded = WorkflowEvent::from_parts(session, kind, &event["payload"]).unwrap();
        let (_, bundle, encoded_kind, payload) = extension_event_parts(decoded).unwrap();
        assert_eq!(
            (bundle.as_str(), encoded_kind.as_str(), &payload),
            (event["bundle"].as_str().unwrap(), kind, &event["payload"]),
            "re-encoded {kind}"
        );
        WorkflowProjectionReducer
            .apply(
                &mut state,
                &ExtensionEventContext {
                    session,
                    seq: EventSeq(0),
                    ts_millis: 0,
                    bundle,
                    kind: encoded_kind,
                    payload,
                },
            )
            .unwrap();
    }
    assert_eq!(state, recorded["projection"]);
}

#[test]
fn run_id_from_operation_keeps_its_namespace() {
    let tool_call = hya_proto::ToolCallId::from_uuid(
        uuid::Uuid::parse_str("018f032a-3d2f-7a21-a05c-2e61fc57dced").unwrap(),
    );
    let run = WorkflowRunId::from_operation(hya_proto::OperationId::from_tool_call(tool_call));
    assert_eq!(run.to_string(), "wfrun_aca1b2cc13e959b7a9132634e71a804b");
    assert_eq!(run.to_string().parse::<WorkflowRunId>().unwrap(), run);
}

#[tokio::test]
async fn tool_without_a_started_service_reports_no_session() {
    let tool = crate::tool::WorkflowTool::new(Arc::new(WorkflowService::new()));
    let (permission, _) = hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
    let ctx = hya_tool::ToolCtx::new(test_root("tool"), permission, CancellationToken::new());
    let error = hya_tool::Tool::execute(&tool, &ctx, json!({ "action": "list" }))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, hya_tool::ToolError::Other(message) if message == "workflow tool requires a session"),
        "{error:?}"
    );
}

async fn cli_call(
    api: &WorkflowBundleApiHandler,
    endpoint: &str,
    session: Option<SessionId>,
    args: Value,
    cwd: &Path,
) -> hya_runtime_abi::DriverReport {
    let reply = api
        .handle(NativeBundleApiRequest {
            api: endpoint.into(),
            method: ApiMethod::Post,
            scope: if session.is_some() {
                ApiScope::Session
            } else {
                ApiScope::Global
            },
            path: format!("/cli/{}", endpoint.trim_start_matches("cli_")),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: json!({ "args": args, "cwd": cwd.display().to_string() }),
            session,
        })
        .await
        .unwrap();
    assert_eq!(reply.status, 200, "{}", reply.body);
    serde_json::from_value(reply.body).unwrap()
}

#[tokio::test]
async fn cli_endpoints_print_what_hya_workflow_printed() {
    let root = test_root("cli");
    let wf_dir = workflow_project_dir(&root);
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(wf_dir.join("demo.hya.md"), DEMO_WORKFLOW).unwrap();
    let runtime = Arc::new(MockHostRuntime::new(root.clone()));
    let session = SessionId::new();
    runtime.seed_session(session, "hya-main", &root);
    runtime.push_reply("planned");
    runtime.push_reply("executed");
    let service = Arc::new(WorkflowService::new());
    service.start(runtime.clone()).await.unwrap();
    let api = WorkflowBundleApiHandler::new(Arc::clone(&service));

    let list = cli_call(&api, "cli_list", None, json!({}), &root).await;
    assert_eq!(list.exit_code, 0, "{list:?}");
    assert!(list.stdout.starts_with("demo\n"), "{}", list.stdout);
    assert!(
        list.stdout.contains("(Stages: plan, execute)"),
        "{}",
        list.stdout
    );
    let json_list = cli_call(&api, "cli_list", None, json!({ "json": true }), &root).await;
    let parsed: WorkflowCommandResult = serde_json::from_str(&json_list.stdout).unwrap();
    assert!(
        matches!(parsed, WorkflowCommandResult::List { workflows } if workflows[0].name == "demo")
    );
    assert!(json_list.stdout.ends_with("}\n"));

    let info = cli_call(&api, "cli_info", None, json!({ "name": "demo" }), &root).await;
    assert!(
        info.stdout
            .contains("execute (after plan) <- Agent `worker` [once]"),
        "{}",
        info.stdout
    );
    let missing = cli_call(&api, "cli_info", None, json!({ "name": "nope" }), &root).await;
    assert_eq!(
        (
            missing.stdout.as_str(),
            missing.stderr.as_str(),
            missing.exit_code
        ),
        (
            "",
            "Error: WORKFLOW_NOT_FOUND: Workflow `nope` was not found\n",
            1
        )
    );

    let bad_revision = cli_call(
        &api,
        "cli_use",
        Some(session),
        json!({ "name": "demo", "revision": "zz", "session": session.to_string() }),
        &root,
    )
    .await;
    assert_eq!(
        bad_revision.stderr,
        "Error: invalid Workflow revision `zz`: Workflow revision must contain exactly 64 hexadecimal characters\n"
    );
    let selected = cli_call(
        &api,
        "cli_use",
        Some(session),
        json!({ "name": "demo", "session": session.to_string() }),
        &root,
    )
    .await;
    assert_eq!(selected.exit_code, 0, "{selected:?}");
    assert!(
        selected
            .stdout
            .starts_with("selected workflow: demo (revision "),
        "{}",
        selected.stdout
    );

    let nameless = cli_call(
        &api,
        "cli_run",
        Some(session),
        json!({ "input": ["topic=x"] }),
        &root,
    )
    .await;
    assert_eq!(
        nameless.stderr,
        "Error: workflow run requires NAME unless --session identifies an owning Session\n"
    );
    let run = cli_call(
        &api,
        "cli_run",
        Some(session),
        json!({ "input": ["topic=rust"], "session": session.to_string() }),
        &root,
    )
    .await;
    assert!(
        run.stdout.starts_with("workflow demo: completed\n"),
        "{run:?}"
    );
    assert_eq!((run.stderr.as_str(), run.exit_code), ("", 0));

    let state = cli_call(
        &api,
        "cli_state",
        Some(session),
        json!({ "session": session.to_string() }),
        &root,
    )
    .await;
    assert!(
        state.stdout.contains("latest run: demo ("),
        "{}",
        state.stdout
    );
    let unknown = cli_call(&api, "cli_state", Some(SessionId::new()), json!({}), &root).await;
    assert!(
        unknown
            .stderr
            .starts_with("Error: session_not_found: Session `"),
        "{unknown:?}"
    );

    service.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn cli_list_and_info_read_project_roots_from_a_subdirectory_and_print_diagnostics() {
    let root = test_root("cli-scope");
    let (first, second) = (root.join("first"), root.join("second"));
    let nested = first.join("src/nested");
    std::fs::create_dir_all(&nested).unwrap();
    let second_workflows = workflow_project_dir(&second);
    std::fs::create_dir_all(&second_workflows).unwrap();
    std::fs::write(second_workflows.join("demo.hya.md"), DEMO_WORKFLOW).unwrap();
    let runtime = Arc::new(MockHostRuntime::new(root.clone()));
    {
        let mut st = runtime.state.lock().unwrap();
        st.project_roots = vec![first.clone(), second.clone()];
        st.diagnostics = vec!["project bundle /p/broken: invalid".into()];
    }
    let service = Arc::new(WorkflowService::new());
    service.start(runtime.clone()).await.unwrap();
    let api = WorkflowBundleApiHandler::new(Arc::clone(&service));

    let list = cli_call(&api, "cli_list", None, json!({}), &nested).await;
    assert_eq!(list.exit_code, 0, "{list:?}");
    assert!(list.stdout.starts_with("demo\n"), "{}", list.stdout);
    assert_eq!(list.stderr, "warning: project bundle /p/broken: invalid\n");
    let missing = cli_call(&api, "cli_info", None, json!({ "name": "nope" }), &nested).await;
    assert_eq!(
        (missing.stderr.as_str(), missing.exit_code),
        (
            "warning: project bundle /p/broken: invalid\nError: WORKFLOW_NOT_FOUND: Workflow `nope` was not found\n",
            1
        )
    );
    service.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
