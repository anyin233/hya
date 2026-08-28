//! Integration tests for `hya-core`: user-assembled workflow DAGs.
//!
//! Workflows compose existing team primitives into user-authored stage graphs:
//! fan-out via `needs` levels executed as one governed team batch per level,
//! fan-in via template placeholders carrying upstream bounded outputs into the
//! consuming stage's directive.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::stream;
use hya_bundle::{AgentRole, SpawnLifecycle};
use hya_core::{
    CompiledWorkflow, CreateSession, EventBus, FailurePolicy, SessionEngine, SubagentGovernor,
    SubagentLimits, WorkflowStatus, load_workflow_file, run_workflow,
};
use hya_proto::{AgentName, Event, FinishReason, Message, MessageId, ModelRef, Part, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, Tool, ToolRegistry};
use hya_workflow::{WorkflowSource, compile};
use tokio_util::sync::CancellationToken;

/// Records every completion request (session + rendered user prompt text) and
/// answers with a fixed marker so handoff assertions can trace flow.
///
/// Verifier-gate judgments are scripted separately: the Nth verifier judgment
/// answers `{"met": bool}` taken from `verdicts[N-1]` (missing entries count as
/// met), which lets loop tests pin exactly when the stop decision happens.
struct RecordingProvider {
    systems: Mutex<Vec<(SessionId, Option<String>)>>,
    requests: Mutex<Vec<(SessionId, String)>>,
    verdicts: Mutex<Vec<bool>>,
    hang_verifier: AtomicBool,
    verifier_started: tokio::sync::Notify,
}

impl RecordingProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            verdicts: Mutex::new(Vec::new()),
            hang_verifier: AtomicBool::new(false),
            systems: Mutex::new(Vec::new()),
            verifier_started: tokio::sync::Notify::new(),
        })
    }

    /// Script the Nth verifier judgment's `met` value.
    fn script_verdict(&self, met: bool) -> &Self {
        self.verdicts.lock().unwrap().push(met);
        self
    }

    /// Make verifier provider calls stay pending until their member token is
    /// cancelled, and expose a notification once the call starts.
    fn script_hanging_verifier(&self) {
        self.hang_verifier.store(true, Ordering::SeqCst);
    }

    async fn wait_for_verifier(&self) {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.verifier_started.notified(),
        )
        .await
        .expect("verifier request should start");
    }

    fn verdict_requests(&self) -> Vec<(SessionId, String)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, text)| text.contains("independent verifier"))
            .cloned()
            .collect()
    }

    fn prompts(&self) -> Vec<(SessionId, String)> {
        self.requests.lock().unwrap().clone()
    }

    /// Effective system prompt observed for each completed provider request.
    fn systems(&self) -> Vec<(SessionId, Option<String>)> {
        self.systems.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        _message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let mut user_text = String::new();
        for message in &req.messages {
            if let Message::User { parts, .. } = message {
                for part in parts {
                    if let Part::Text { text, .. } = part {
                        user_text.push_str(text);
                    }
                }
            }
        }
        let reply = if user_text.starts_with("EXPLORE") {
            "EXPLORER_REPORT"
        } else if user_text.contains("FAIL_ME") {
            return Err(ProviderError::Transport("member scripted failure".into()));
        } else if user_text.contains("independent verifier") {
            if self.hang_verifier.load(Ordering::SeqCst) {
                self.verifier_started.notify_one();
                futures::future::pending::<()>().await;
                unreachable!("pending verifier future only exits by cancellation");
            }
            let mut verdicts = self.verdicts.lock().unwrap();
            // Scripted judgments are consumed in order; unscripted ones grant
            // so a miscount fails loudly via missing worker resumes instead.
            let met = if verdicts.is_empty() {
                true
            } else {
                verdicts.remove(0)
            };
            return self.complete(
                &req,
                session,
                user_text,
                format!("{{\"met\": {met}, \"reason\": \"scripted\"}}"),
            );
        } else if user_text.contains("FAIL_LOOP_RESUME") {
            return Err(ProviderError::Transport(
                "resumed loop worker scripted failure".into(),
            ));
        } else {
            "WORKER_DONE"
        };
        self.complete(&req, session, user_text, reply.to_string())
    }
}

impl RecordingProvider {
    fn complete(
        &self,
        req: &CompletionRequest,
        session: SessionId,
        user_text: String,
        reply: String,
    ) -> Result<EventStream, ProviderError> {
        self.systems
            .lock()
            .unwrap()
            .push((session, req.system.clone()));
        self.requests.lock().unwrap().push((session, user_text));
        let events = FakeProvider::materialize(
            &[FakeStep::Text(reply), FakeStep::Finish(FinishReason::Stop)],
            session,
            MessageId::new(),
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

/// Provider barrier that can finish only when two same-level Stage streams are
/// polled concurrently.
struct BarrierProvider {
    barrier: tokio::sync::Barrier,
    entered: AtomicUsize,
}

impl BarrierProvider {
    /// Create a two-member overlap barrier.
    fn new() -> Arc<Self> {
        Arc::new(Self {
            barrier: tokio::sync::Barrier::new(2),
            entered: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl Provider for BarrierProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        _message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let parallel = request.messages.iter().any(|message| {
            matches!(message, Message::User { parts, .. } if parts.iter().any(|part| {
                matches!(part, Part::Text { text, .. } if text.starts_with("PARALLEL"))
            }))
        });
        if parallel {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.barrier.wait().await;
        }
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("OVERLAPPED".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            MessageId::new(),
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

/// Provider that exposes resident work admission, then remains pending until
/// the supervisor cancels the resident turn.
struct ResidentHangingProvider {
    started: tokio::sync::Notify,
}

impl ResidentHangingProvider {
    /// Create one hanging resident provider.
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: tokio::sync::Notify::new(),
        })
    }

    /// Wait until the resident provider call starts.
    async fn wait_started(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(2), self.started.notified())
            .await
            .expect("resident provider call starts");
    }
}

#[async_trait]
impl Provider for ResidentHangingProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
        _session: SessionId,
        _message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.started.notify_one();
        futures::future::pending::<Result<EventStream, ProviderError>>().await
    }
}

async fn engine(provider: Arc<dyn Provider>) -> Arc<SessionEngine> {
    engine_with_limits(provider, SubagentLimits::default()).await
}

async fn engine_with_limits(
    provider: Arc<dyn Provider>,
    limits: SubagentLimits,
) -> Arc<SessionEngine> {
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    // Install the fixture catalog so stage agents (explorer/builder/...) resolve
    // through the same AgentCatalog seam the production task path uses.
    let runtime = Arc::new(hya_core::RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        catalog(),
    ));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    Arc::new(
        SessionEngine::new(store, router, runtime, perm, EventBus::default())
            .with_governor(SubagentGovernor::new(limits)),
    )
}

const TWO_STAGE_WORKFLOW: &str = r#"---
kind: Workflow
name: feature-flow
description: Explore then build.
inputs:
  target: What to explore.
nodes:
  explore:
    agent: explorer
    directive: EXPLORE {{input.target}}
  build:
    agent: builder
    directive: BUILD from direct evidence.
---
flowchart TD
  explore --> build
"#;

fn write_workflow(workdir: &std::path::Path, file_name: &str, body: &str) -> PathBuf {
    let dir = workdir.join(".hya/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file_name);
    std::fs::write(&path, body).unwrap();
    path
}

/// Compile one in-memory Workflow fixture through the public authoring seam.
fn compile_workflow(name: &str, source: &str) -> CompiledWorkflow {
    compile(WorkflowSource::new(name, source)).unwrap()
}

fn catalog() -> Arc<hya_core::AgentCatalog> {
    support::test_catalog(&[
        ("explorer", AgentRole::Subagent, &[]),
        ("builder", AgentRole::Subagent, &[]),
        ("reviewer", AgentRole::Subagent, &[]),
        ("planner", AgentRole::Subagent, &[]),
    ])
}

async fn start_lead(engine: &Arc<SessionEngine>) -> SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap()
}

/// Smallest end-to-end claim: a two-stage user workflow parses from a fixture,
/// resolves both agents through the test `AgentCatalog`, runs stage 1 to
/// completion, and injects stage 1's bounded output into stage 2's directive.
#[tokio::test]
async fn two_stage_workflow_runs_in_order_and_hands_off_evidence() {
    let workdir = support::TestDir::new("workflow-two-stage");
    let path = write_workflow(workdir.path(), "feature.hya.md", TWO_STAGE_WORKFLOW);
    let def = load_workflow_file(&path).unwrap();

    let provider = RecordingProvider::new();
    let engine = engine(provider.clone()).await;
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();

    let report = run_workflow(
        engine.clone(),
        lead,
        &def,
        hya_core::WorkflowRunContext {
            binding,
            caller: "build".to_string(),
            base_agent: base,
            inputs: BTreeMap::from([("target".to_string(), "the retry paths".to_string())]),
            resident_supervisor: None,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.status, WorkflowStatus::Completed);
    assert_eq!(report.stages.len(), 2);
    assert_eq!(report.stages[0].stage, "explore");
    assert_eq!(report.stages[0].status.to_string(), "done");
    assert_eq!(report.stages[1].stage, "build");

    let prompts = provider.prompts();
    assert_eq!(prompts.len(), 2, "one turn per stage");
    assert!(
        prompts[0].1.starts_with("EXPLORE the retry paths"),
        "inputs substitute into the first directive: {:?}",
        prompts[0].1
    );
    assert!(
        prompts[1].1.contains("BUILD from") && prompts[1].1.contains("EXPLORER_REPORT"),
        "second directive must embed the explore stage output: {:?}",
        prompts[1].1
    );
    assert_ne!(
        prompts[0].0, prompts[1].0,
        "each stage runs in its own child session"
    );
    assert_ne!(prompts[0].0, lead, "members never reuse the lead session");
}

/// Fan-out/fan-in: two workers depend on one planner (parallel level), and a
/// reviewer joins BOTH worker outputs; review verdicts converge.
#[tokio::test]
async fn fan_out_then_fan_in_joins_all_upstream_outputs() {
    let source = r#"---
kind: Workflow
name: fan-flow
description: Plan fans out to two implementations, then reviews both.
nodes:
  plan:
    agent: planner
    directive: PLAN
  impl_a:
    agent: builder
    directive: IMPL A
  impl_b:
    agent: builder
    directive: IMPL B
  review:
    agent: reviewer
    directive: REVIEW both
---
flowchart TD
  plan --> impl_a & impl_b
  impl_a & impl_b --> review
"#;
    let workdir = support::TestDir::new("workflow-fan");
    let def = compile_workflow("fan.hya.md", source);

    let provider = RecordingProvider::new();
    let engine = engine(provider.clone()).await;
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();

    let report = run_workflow(
        engine,
        lead,
        &def,
        hya_core::WorkflowRunContext {
            binding,
            caller: "build".to_string(),
            base_agent: base,
            inputs: BTreeMap::new(),
            resident_supervisor: None,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.status, WorkflowStatus::Completed);
    assert_eq!(report.stages.len(), 4);

    // impl_a and impl_b form one level; the reviewer joins both.
    let prompts = provider.prompts();
    assert_eq!(
        prompts
            .iter()
            .filter(|(_, text)| text.contains("IMPL A"))
            .count(),
        1,
        "impl A runs exactly one member turn"
    );

    let (_, review_prompt) = prompts
        .iter()
        .find(|(_, text)| text.starts_with("REVIEW both"))
        .expect("review prompt appears once");
    assert!(
        review_prompt.contains("<stage id=\"impl_a\"")
            && review_prompt.contains("<stage id=\"impl_b\""),
        "join stage receives both fanned-in sections: {review_prompt:?}"
    );
}

/// Same-level transient Stages enter provider execution together through one
/// governed batch; a sequential scheduler deadlocks at this barrier.
#[tokio::test]
async fn same_level_transient_stages_overlap_provider_execution() {
    let source = r#"---
kind: Workflow
name: overlap-flow
description: Two independent Stages overlap.
nodes:
  left:
    agent: builder
    directive: PARALLEL left
  right:
    agent: builder
    directive: PARALLEL right
---
flowchart TD
  left
  right
"#;
    let workflow = compile_workflow("overlap.hya.md", source);
    let provider = BarrierProvider::new();
    let engine = engine(provider.clone()).await;
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(std::path::Path::new("/tmp")).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_workflow(
            engine,
            lead,
            &workflow,
            hya_core::WorkflowRunContext {
                binding,
                caller: "build".to_string(),
                base_agent: base,
                inputs: BTreeMap::new(),
                resident_supervisor: None,
            },
            CancellationToken::new(),
        ),
    )
    .await
    .expect("both provider calls must reach the barrier")
    .unwrap();
    assert_eq!(
        report.status,
        WorkflowStatus::Completed,
        "reports={:?}, entered={}",
        report.stages,
        provider.entered.load(Ordering::SeqCst)
    );
    assert_eq!(provider.entered.load(Ordering::SeqCst), 2);
}

/// `on_member_failure: collect_all` keeps the DAG running: the failed stage is
/// reported FAILED in the joined directive instead of aborting the workflow.
#[tokio::test]
async fn collect_all_policy_reports_failures_into_the_join() {
    let source = r#"---
kind: Workflow
name: tolerant-flow
description: One implementation may fail; review still aggregates.
on_failure: collect_all
nodes:
  good:
    agent: builder
    directive: IMPL GOOD
  bad:
    agent: builder
    directive: FAIL_ME IMPL BAD
  review:
    agent: reviewer
    directive: REVIEW all
---
flowchart TD
  good & bad --> review
"#;
    let workdir = support::TestDir::new("workflow-collect-all");
    let def = compile_workflow("tolerant.hya.md", source);
    assert_eq!(def.definition().on_failure(), FailurePolicy::CollectAll);

    let provider = RecordingProvider::new();
    let engine = engine(provider.clone()).await;
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();

    let report = run_workflow(
        engine,
        lead,
        &def,
        hya_core::WorkflowRunContext {
            binding,
            caller: "build".to_string(),
            base_agent: base,
            inputs: BTreeMap::new(),
            resident_supervisor: None,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.status, WorkflowStatus::Failed);
    let bad = report.stages.iter().find(|s| s.stage == "bad").unwrap();
    assert_eq!(bad.status.to_string(), "failed");

    let prompts = provider.prompts();
    let (_, review_prompt) = prompts
        .iter()
        .find(|(_, text)| text.starts_with("REVIEW all"))
        .expect("review still ran");
    assert!(
        review_prompt.contains("id=\"good\"") && review_prompt.contains("WORKER_DONE"),
        "healthy upstream flows through"
    );
    assert!(
        review_prompt.contains("id=\"bad\"") && review_prompt.contains("status=\"failed\""),
        "failed upstream is declared to the joining stage: {review_prompt:?}"
    );
}

/// A failure after a loop stage's first round is still a stage outcome. Under
/// `collect_all`, its downstream join runs and receives explicit FAILED
/// evidence instead of the executor returning early with a workflow error.
#[tokio::test]
async fn collect_all_applies_to_resumed_loop_worker_failures() {
    let source = r#"---
kind: Workflow
name: tolerant-loop
description: A later loop round may fail; review still aggregates.
on_failure: collect_all
nodes:
  build:
    agent: builder
    directive: BUILD first round
    mode: loop
    verify:
      agent: reviewer
      until: FAIL_LOOP_RESUME
      max_iterations: 3
  review:
    agent: reviewer
    directive: REVIEW loop
---
flowchart TD
  build --> review
"#;
    let workdir = support::TestDir::new("workflow-collect-loop-failure");
    let def = compile_workflow("tolerant-loop.hya.md", source);
    let provider = RecordingProvider::new();
    provider.script_verdict(false);
    let engine = engine(provider.clone()).await;
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();

    let report = run_workflow(
        engine,
        lead,
        &def,
        hya_core::WorkflowRunContext {
            binding,
            caller: "build".to_string(),
            base_agent: base,
            inputs: BTreeMap::new(),
            resident_supervisor: None,
        },
        CancellationToken::new(),
    )
    .await
    .expect("collect_all must turn the loop error into failed evidence");

    assert_eq!(report.status, WorkflowStatus::Failed);
    let build = report
        .stages
        .iter()
        .find(|stage| stage.stage == "build")
        .unwrap();
    assert_eq!(build.status.to_string(), "failed");
    let review = report
        .stages
        .iter()
        .find(|stage| stage.stage == "review")
        .expect("downstream review still runs");
    assert_eq!(review.status.to_string(), "done");
    let prompts = provider.prompts();
    let (_, review_prompt) = prompts
        .iter()
        .find(|(_, text)| text.starts_with("REVIEW loop"))
        .expect("review receives loop evidence");
    assert!(review_prompt.contains("id=\"build\"") && review_prompt.contains("status=\"failed\""));
}

fn base_spec() -> hya_core::AgentSpec {
    hya_core::AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    }
}

/// Shared run harness for the negative-path tests below.
struct Run {
    provider: Arc<RecordingProvider>,
    engine: Arc<SessionEngine>,
    lead: SessionId,
    binding: hya_core::TurnBinding,
}

async fn setup(workdir_label: &str) -> Run {
    let provider = RecordingProvider::new();
    let engine = engine(provider.clone()).await;
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(std::path::Path::new("/tmp")).unwrap();
    let _ = workdir_label;
    Run {
        provider,
        engine,
        lead,
        binding,
    }
}

async fn run_def(
    run: &Run,
    def: &CompiledWorkflow,
    inputs: BTreeMap<String, String>,
) -> Result<hya_core::WorkflowRunReport, hya_core::WorkflowError> {
    let base = run
        .engine
        .agent_spec_for_binding(&run.binding, &base_spec(), "build")
        .unwrap();
    run_workflow(
        run.engine.clone(),
        run.lead,
        def,
        hya_core::WorkflowRunContext {
            binding: run.binding.clone(),
            caller: "build".to_string(),
            base_agent: base,
            inputs,
            resident_supervisor: None,
        },
        CancellationToken::new(),
    )
    .await
}

#[test]
fn old_stage_list_sources_are_rejected() {
    let old_source = r#"
name: old-flow
description: The removed Stage-list format.
stages:
  - id: run
    agent: builder
    prompt: Run.
"#;
    let error = compile(WorkflowSource::new("old.yaml", old_source)).unwrap_err();
    assert!(
        error.message().contains("must start with YAML frontmatter"),
        "{error}"
    );
}

#[tokio::test]
async fn missing_declared_input_fails_before_any_spawn() {
    let source = r#"---
kind: Workflow
name: input-flow
description: Require one run input.
inputs:
  target: Required target.
nodes:
  run:
    agent: builder
    directive: Do {{input.target}}.
---
flowchart TD
  run
"#;
    let def = compile_workflow("input.hya.md", source);
    let run = setup("input").await;
    let error = run_def(&run, &def, BTreeMap::new())
        .await
        .expect_err("missing input must fail");
    assert!(
        error.to_string().contains("`target` was not provided"),
        "{error}"
    );
    assert!(run.provider.prompts().is_empty());
}

#[tokio::test]
async fn unknown_stage_agent_is_rejected_before_spawn() {
    let source = r#"---
kind: Workflow
name: ghost-flow
description: Reference an unavailable Agent.
nodes:
  run:
    agent: ghost-agent-not-installed
    directive: Run.
---
flowchart TD
  run
"#;
    let def = compile_workflow("ghost.hya.md", source);
    let run = setup("ghost").await;
    let error = run_def(&run, &def, BTreeMap::new())
        .await
        .expect_err("unknown agent must fail");
    assert!(
        error.to_string().contains("not spawnable by `build`")
            && error.to_string().contains("ghost-agent-not-installed"),
        "{error}"
    );
    assert!(
        run.provider.prompts().is_empty(),
        "authorization must fail before any member turns"
    );
}

#[tokio::test]
async fn per_run_budget_overflow_is_rejected_up_front() {
    let source = r#"---
kind: Workflow
name: wide-flow
description: Three Stages exceed a two-slot budget.
nodes:
  one:
    agent: builder
    directive: One.
  two:
    agent: builder
    directive: Two.
  three:
    agent: reviewer
    directive: Three.
---
flowchart TD
  one & two --> three
"#;
    let def = compile_workflow("wide.hya.md", source);
    let provider = RecordingProvider::new();
    let engine = engine_with_limits(
        provider.clone(),
        SubagentLimits {
            per_run_budget: 2,
            ..SubagentLimits::default()
        },
    )
    .await;
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(std::path::Path::new("/tmp")).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();
    let error = run_workflow(
        engine,
        lead,
        &def,
        hya_core::WorkflowRunContext {
            binding,
            caller: "build".to_string(),
            base_agent: base,
            inputs: BTreeMap::new(),
            resident_supervisor: None,
        },
        CancellationToken::new(),
    )
    .await
    .expect_err("3 stages must exceed per_run_budget=2");
    assert!(
        error.to_string().contains("exceed the per-run budget"),
        "{error}"
    );
    assert!(
        provider.prompts().is_empty(),
        "budget rejection happens before any spawn"
    );
}

/// Fail-fast: when a level member fails, downstream levels never start and the
/// report marks the run Failed while keeping the failed level's evidence.
#[tokio::test]
async fn fail_fast_aborts_downstream_levels() {
    let source = r#"---
kind: Workflow
name: abort-flow
description: Fail-fast skips downstream Stages.
on_failure: fail_fast
nodes:
  broken:
    agent: builder
    directive: FAIL_ME explode
  after:
    agent: reviewer
    directive: Should never run.
---
flowchart TD
  broken --> after
"#;
    let def = compile_workflow("abort.hya.md", source);
    let run = setup("fail-fast").await;
    let report = run_def(&run, &def, BTreeMap::new())
        .await
        .expect("workflow completes with failure semantics");
    assert_eq!(report.status, WorkflowStatus::Failed);
    assert_eq!(
        report.stages.len(),
        2,
        "terminal reports retain every Stage"
    );
    assert_eq!(report.stages[0].stage, "broken");
    assert_eq!(report.stages[0].status.to_string(), "failed");
    assert_eq!(report.stages[1].status, hya_core::StageStatus::Skipped);
    // The scripted failure errors during streaming, so no request ever
    // completes; downstream never renders.
    assert!(run.provider.prompts().is_empty());
}

/// Loop stages iterate through the shared driver and the INDEPENDENT verifier —
/// never the worker's own claim — owns the stop decision. Script: worker always
/// claims done; verifier denies once then grants. Exactly one extra worker round
/// runs in the SAME child session, both judgments come from fresh sessions, and
/// the report records the verified outcome.
#[tokio::test]
async fn loop_stage_verifier_owns_the_stop_decision() {
    let source = r#"---
kind: Workflow
name: loop-flow
description: Verify-gated build Stage.
nodes:
  build:
    agent: builder
    directive: LOOP_BUILD the thing
    mode: loop
    verify:
      agent: reviewer
      until: Tests pass.
      max_iterations: 4
---
flowchart TD
  build
"#;
    let def = compile_workflow("loop.hya.md", source);
    let provider = RecordingProvider::new();
    provider.script_verdict(false); // first judgment: not yet met
    provider.script_verdict(true); // second judgment: met -> stop
    let engine = engine(provider.clone()).await;
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(std::path::Path::new("/tmp")).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();
    let report = run_workflow(
        engine,
        lead,
        &def,
        hya_core::WorkflowRunContext {
            binding,
            caller: "build".to_string(),
            base_agent: base,
            inputs: BTreeMap::new(),
            resident_supervisor: None,
        },
        CancellationToken::new(),
    )
    .await
    .expect("loop workflow completes");
    assert_eq!(report.status, WorkflowStatus::Completed);
    assert_eq!(report.stages.len(), 1);
    assert_eq!(report.stages[0].status.to_string(), "done");
    assert!(
        report.stages[0].output.contains("WORKER_DONE")
            && report.stages[0].output.contains("[verified: scripted]"),
        "final output carries the worker text plus verified marker: {:?}",
        report.stages[0].output
    );

    let prompts = provider.prompts();
    let worker_rounds: Vec<_> = prompts
        .iter()
        .filter(|(_, t)| t.starts_with("LOOP_BUILD"))
        .collect();
    assert_eq!(
        worker_rounds.len(),
        2,
        "first batch round + exactly one resume; prompts={prompts:?}"
    );
    // The resume continues the SAME child session (iteration continuity).
    assert_eq!(worker_rounds[0].0, worker_rounds[1].0);
    // Continuation directive carries the verifier's denial reason.
    assert!(
        worker_rounds[1]
            .1
            .contains("The verifier reports the condition is not yet met")
            && worker_rounds[1].1.contains("scripted"),
        "resume directive embeds verifier feedback: {:?}",
        worker_rounds[1].1
    );

    let verdicts = provider.verdict_requests();
    assert_eq!(verdicts.len(), 2, "one judgment per iteration");
    for (session, _) in &verdicts {
        assert_ne!(
            session, &worker_rounds[0].0,
            "verifier runs in its own fresh session"
        );
    }
    assert_ne!(verdicts[0].0, verdicts[1].0, "fresh session per judgment");
}

/// Cancellation must reach the fresh verifier member. A provider call that
/// never returns cannot keep a cancelled Workflow or engine shutdown pending.
#[tokio::test]
async fn workflow_cancellation_stops_an_inflight_loop_verifier() {
    let source = r#"---
kind: Workflow
name: cancellable-loop
description: Verifier cancellation reaches its governed member.
nodes:
  build:
    agent: builder
    directive: BUILD before verifier
    mode: loop
    verify:
      agent: reviewer
      until: Tests pass.
      max_iterations: 3
---
flowchart TD
  build
"#;
    let workdir = support::TestDir::new("workflow-cancel-verifier");
    let def = compile_workflow("cancellable-loop.hya.md", source);
    let provider = RecordingProvider::new();
    provider.script_hanging_verifier();
    let engine = engine(provider.clone()).await;
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let run = tokio::spawn(async move {
        run_workflow(
            engine,
            lead,
            &def,
            hya_core::WorkflowRunContext {
                binding,
                caller: "build".to_string(),
                base_agent: base,
                inputs: BTreeMap::new(),
                resident_supervisor: None,
            },
            run_cancel,
        )
        .await
    });

    provider.wait_for_verifier().await;
    cancel.cancel();
    let report = tokio::time::timeout(std::time::Duration::from_secs(2), run)
        .await
        .expect("cancelled Workflow must not wait for the verifier forever")
        .expect("Workflow task must not panic")
        .expect("cancellation is a terminal report, not a core error");
    assert_eq!(report.status, WorkflowStatus::Cancelled);
}

/// Sequential Stages with one explicit actor key reuse one resident Session and
/// cross two durable mail boundaries; the first directive is mail, not an
/// implicit registration wake.
#[tokio::test]
async fn sequential_actor_stages_reuse_one_resident_session() {
    let source = r#"---
kind: Workflow
name: resident-flow
description: Reuse one resident planner across two sequential Stages.
nodes:
  draft:
    agent: resident-planner
    actor: planner
    directive: Draft the plan.
  refine:
    agent: resident-planner
    actor: planner
    directive: Refine the plan.
---
flowchart TD
  draft --> refine
"#;
    let workflow = compile_workflow("resident.hya.md", source);
    let provider = RecordingProvider::new();
    let catalog = support::test_catalog_with_lifecycles(&[(
        "resident-planner",
        AgentRole::Subagent,
        SpawnLifecycle::Resident,
        &[],
    )]);
    let (engine, _events) = engine_parts(provider.clone(), catalog, None).await;
    let supervisor = hya_core::ResidentSupervisor::start(engine.clone());
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(std::path::Path::new("/tmp")).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();

    let report = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_workflow(
            engine.clone(),
            lead,
            &workflow,
            hya_core::WorkflowRunContext {
                binding,
                caller: "build".to_string(),
                base_agent: base,
                inputs: BTreeMap::new(),
                resident_supervisor: Some(supervisor),
            },
            CancellationToken::new(),
        ),
    )
    .await
    .expect("resident Workflow settles")
    .unwrap();

    assert_eq!(report.status, WorkflowStatus::Completed);
    assert_eq!(report.stages[0].session, report.stages[1].session);
    let session = report.stages[0]
        .session
        .as_deref()
        .expect("resident session");
    assert_eq!(
        provider
            .prompts()
            .iter()
            .filter(|(candidate, _)| candidate.to_string() == session)
            .count(),
        2,
        "one provider activation per Stage in the same resident Session"
    );

    let systems = provider.systems();
    assert_eq!(systems.len(), 2);
    assert!(
        systems[0]
            .1
            .as_deref()
            .is_some_and(|prompt| prompt.contains("resident-planner prompt")
                && prompt.contains("stage: draft")),
        "first activation uses the target Agent prompt plus Workflow context"
    );
    assert!(
        systems[1]
            .1
            .as_deref()
            .is_some_and(|prompt| prompt.contains("resident-planner prompt")
                && prompt.contains("stage: refine")),
        "later actor activation replaces only its Workflow context layer"
    );

    let root = engine.read_projection(lead).await.unwrap();
    assert_eq!(root.team.inboxes.values().map(Vec::len).sum::<usize>(), 2);
    let resident = root
        .team
        .roster
        .values()
        .find(|entry| entry.session.to_string() == session)
        .expect("resident roster row");
    assert!(resident.resident_cursor >= 2);
    assert!(resident.resident_work.is_none());
    let mail_events = engine
        .replay(lead)
        .await
        .unwrap()
        .iter()
        .filter(|envelope| matches!(envelope.event, Event::MailSent { .. }))
        .count();
    assert_eq!(mail_events, 2, "both Stage directives are durable mail");
}

/// Workflow cancellation stops an in-flight resident turn, waits for its
/// durable boundary to settle, and never reports while resident work remains.
#[tokio::test]
async fn workflow_cancellation_settles_inflight_resident_stage() {
    let source = r#"---
kind: Workflow
name: cancel-resident
description: Cancel one active resident Stage.
nodes:
  work:
    agent: resident-worker
    actor: worker
    directive: Hang until cancelled.
---
flowchart TD
  work
"#;
    let workflow = compile_workflow("cancel-resident.hya.md", source);
    let provider = ResidentHangingProvider::new();
    let catalog = support::test_catalog_with_lifecycles(&[(
        "resident-worker",
        AgentRole::Subagent,
        SpawnLifecycle::Resident,
        &[],
    )]);
    let (engine, _events) = engine_parts(provider.clone(), catalog, None).await;
    let supervisor = hya_core::ResidentSupervisor::start(engine.clone());
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(std::path::Path::new("/tmp")).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let run = tokio::spawn({
        let engine = engine.clone();
        async move {
            run_workflow(
                engine,
                lead,
                &workflow,
                hya_core::WorkflowRunContext {
                    binding,
                    caller: "build".to_string(),
                    base_agent: base,
                    inputs: BTreeMap::new(),
                    resident_supervisor: Some(supervisor),
                },
                run_cancel,
            )
            .await
        }
    });

    provider.wait_started().await;
    cancel.cancel();
    let report = tokio::time::timeout(std::time::Duration::from_secs(2), run)
        .await
        .expect("resident cancellation must settle")
        .expect("Workflow task must not panic")
        .unwrap();
    assert_eq!(report.status, WorkflowStatus::Cancelled);
    assert_eq!(report.stages[0].status, hya_core::StageStatus::Cancelled);
    let projection = engine.read_projection(lead).await.unwrap();
    let resident = projection
        .team
        .roster
        .values()
        .find(|entry| entry.agent_type.as_str() == "resident-worker")
        .expect("resident roster row remains replay-visible");
    assert!(resident.resident_work.is_none());
}

/// Agent ids are definitions, not identities: transient reuse creates distinct
/// Sessions, while two explicit actor keys create two resident Sessions.
#[tokio::test]
async fn actor_keys_not_agent_ids_control_session_reuse() {
    let source = r#"---
kind: Workflow
name: identity-flow
description: Contrast transient and actor identity.
nodes:
  first:
    agent: builder
    directive: First transient.
  second:
    agent: builder
    directive: Second transient.
  actor_a:
    agent: resident-planner
    actor: alpha
    directive: Resident alpha.
  actor_b:
    agent: resident-planner
    actor: beta
    directive: Resident beta.
---
flowchart TD
  first --> second
  second --> actor_a & actor_b
"#;
    let workflow = compile_workflow("identity.hya.md", source);
    let provider = RecordingProvider::new();
    let catalog = support::test_catalog_with_lifecycles(&[
        (
            "builder",
            AgentRole::Subagent,
            SpawnLifecycle::Transient,
            &[],
        ),
        (
            "resident-planner",
            AgentRole::Subagent,
            SpawnLifecycle::Resident,
            &[],
        ),
    ]);
    let (engine, _events) = engine_parts(provider, catalog, None).await;
    let supervisor = hya_core::ResidentSupervisor::start(engine.clone());
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(std::path::Path::new("/tmp")).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();
    let report = run_workflow(
        engine.clone(),
        lead,
        &workflow,
        hya_core::WorkflowRunContext {
            binding,
            caller: "build".to_string(),
            base_agent: base,
            inputs: BTreeMap::new(),
            resident_supervisor: Some(supervisor),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.status, WorkflowStatus::Completed);
    assert_ne!(report.stages[0].session, report.stages[1].session);
    assert_ne!(report.stages[2].session, report.stages[3].session);
    assert_eq!(
        engine
            .read_projection(lead)
            .await
            .unwrap()
            .team
            .roster
            .values()
            .filter(|entry| entry.agent_type.as_str() == "resident-planner")
            .count(),
        2
    );
}

/// A provider failure in a resident activation becomes typed failed Stage
/// evidence and leaves no in-flight resident work behind.
#[tokio::test]
async fn resident_stage_failure_is_terminal_evidence() {
    let source = r#"---
kind: Workflow
name: failed-resident
description: Surface one resident provider failure.
nodes:
  fail:
    agent: resident-worker
    actor: worker
    directive: FAIL_ME resident turn.
---
flowchart TD
  fail
"#;
    let workflow = compile_workflow("failed-resident.hya.md", source);
    let provider = RecordingProvider::new();
    let catalog = support::test_catalog_with_lifecycles(&[(
        "resident-worker",
        AgentRole::Subagent,
        SpawnLifecycle::Resident,
        &[],
    )]);
    let (engine, _events) = engine_parts(provider, catalog, None).await;
    let supervisor = hya_core::ResidentSupervisor::start(engine.clone());
    let lead = start_lead(&engine).await;
    let binding = engine.bind_runtime(std::path::Path::new("/tmp")).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();
    let report = run_workflow(
        engine.clone(),
        lead,
        &workflow,
        hya_core::WorkflowRunContext {
            binding,
            caller: "build".to_string(),
            base_agent: base,
            inputs: BTreeMap::new(),
            resident_supervisor: Some(supervisor),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.status, WorkflowStatus::Failed);
    assert_eq!(report.stages[0].status, hya_core::StageStatus::Failed);
    let projection = engine.read_projection(lead).await.unwrap();
    let resident = projection
        .team
        .roster
        .values()
        .find(|entry| entry.agent_type.as_str() == "resident-worker")
        .expect("resident roster row");
    assert!(resident.resident_work.is_none());
}

// ---------------------------------------------------------------------------
// Per-member execution-context resolution (task-path parity)
//
// Regression coverage for the reviewer contract: a workflow worker/verifier
// must reuse the task tool's target-agent context resolution — the STAGE
// agent's own `can_spawn` roster, resource policy, and bound sidecar factory —
// never the caller's roster and never a `None` factory.
// ---------------------------------------------------------------------------

/// Catalog-driven engine variant with an optional bundle sidecar environment;
/// returns a live event-log subscriber for tool-outcome assertions.
async fn engine_parts(
    provider: Arc<dyn Provider>,
    catalog: Arc<hya_core::AgentCatalog>,
    sidecar: Option<Arc<dyn hya_core::sidecar::SidecarEnvironment>>,
) -> (
    Arc<SessionEngine>,
    tokio::sync::broadcast::Receiver<hya_proto::Envelope>,
) {
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = Arc::new(hya_core::RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        catalog,
    ));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let bus = EventBus::default();
    let events = bus.subscribe();
    let builder = SessionEngine::new(store, router, runtime, perm, bus)
        .with_governor(SubagentGovernor::new(SubagentLimits::default()));
    let builder = if let Some(environment) = sidecar {
        builder.with_sidecar_environment(environment)
    } else {
        builder
    };
    (Arc::new(builder), events)
}

/// Provider that makes each member session call one probe tool on its first
/// turn and finish on its second, while independent verifiers answer their
/// strict JSON verdict immediately (they must stay tool-free).
struct ProbeProvider {
    probe_tool: &'static str,
    turns: Mutex<BTreeMap<String, u32>>,
}

impl ProbeProvider {
    fn new(probe_tool: &'static str) -> Arc<Self> {
        Arc::new(Self {
            probe_tool,
            turns: Mutex::new(BTreeMap::new()),
        })
    }

    fn stream_steps(
        &self,
        steps: &[FakeStep],
        session: SessionId,
    ) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(stream::iter(
            FakeProvider::materialize(steps, session, MessageId::new())
                .into_iter()
                .map(Ok::<Event, ProviderError>),
        )))
    }
}

#[async_trait]
impl Provider for ProbeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        _message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let mut user_text = String::new();
        for message in &req.messages {
            if let Message::User { parts, .. } = message {
                for part in parts {
                    if let Part::Text { text, .. } = part {
                        user_text.push_str(text);
                    }
                }
            }
        }
        if user_text.contains("independent verifier") {
            return self.stream_steps(
                &[
                    FakeStep::Text("{\"met\": true, \"reason\": \"scripted\"}".to_string()),
                    FakeStep::Finish(FinishReason::Stop),
                ],
                session,
            );
        }
        let turn = {
            let mut turns = self.turns.lock().unwrap();
            let count = turns.entry(session.to_string()).or_insert(0);
            *count += 1;
            *count
        };
        if turn == 1 {
            self.stream_steps(
                &[
                    FakeStep::ToolCall {
                        name: self.probe_tool.to_string(),
                        input: serde_json::json!({}),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ],
                session,
            )
        } else {
            self.stream_steps(
                &[
                    FakeStep::Text("PROBE_DONE".to_string()),
                    FakeStep::Finish(FinishReason::Stop),
                ],
                session,
            )
        }
    }
}

/// Terminal texts of one session's single probe call, drained from the shared
/// event log (the projection folds intermediate tool rounds away). Each probe
/// member issues exactly one tool call, so a session-level filter suffices.
fn tool_results_for(
    rx: &mut tokio::sync::broadcast::Receiver<hya_proto::Envelope>,
    session: SessionId,
    _tool: &str,
) -> Vec<Result<String, String>> {
    let mut out = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        match envelope.event {
            Event::ToolResult {
                session: s, output, ..
            } if s == session => {
                out.push(Ok(output.to_string()));
            }
            Event::ToolError {
                session: s,
                message_text,
                ..
            } if s == session => {
                out.push(Err(message_text));
            }
            _ => {}
        }
    }
    out
}

/// Parent agents can authorize a stage target while the stage itself stays
/// restricted: the MEMBER's reachable roster is derived from the stage agent's
/// own `can_spawn`, not inherited from the caller's broader roster.
#[tokio::test]
async fn stage_members_resolve_their_own_can_spawn_roster() {
    const ISOLATED_WORKFLOW: &str = r#"---
kind: Workflow
name: isolated-flow
description: Worker cannot delegate further.
nodes:
  only:
    agent: worker
    directive: ROSTER_PROBE_ISOLATED
---
flowchart TD
  only
"#;
    const DELEGATING_WORKFLOW: &str = r#"---
kind: Workflow
name: delegating-flow
description: Delegate may hand off to helper only.
nodes:
  spawn_step:
    agent: delegate
    directive: ROSTER_PROBE_DELEGATE
---
flowchart TD
  spawn_step
"#;

    let isolated = compile_workflow("isolated.hya.md", ISOLATED_WORKFLOW);
    let delegating = compile_workflow("delegating.hya.md", DELEGATING_WORKFLOW);

    let provider = ProbeProvider::new("list_agents");
    let catalog = support::test_catalog(&[
        ("helper", AgentRole::Subagent, &[]),
        ("worker", AgentRole::Subagent, &[]),
        ("delegate", AgentRole::Subagent, &["helper"]),
    ]);
    let (engine, mut event_rx) = engine_parts(provider.clone(), catalog, None).await;
    let lead = start_lead(&engine).await;
    let workdir = support::TestDir::new("workflow-roster");
    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();
    let make_ctx = || hya_core::WorkflowRunContext {
        binding: binding.clone(),
        caller: "build".to_string(),
        base_agent: base.clone(),
        inputs: BTreeMap::new(),
        resident_supervisor: None,
    };

    // Parent authority control: the caller's OWN roster spans every target, so
    // declaring either agent as a workflow stage is admittable.
    let caller_roster = engine.agent_roster_for_binding(&binding, "build").unwrap();
    assert!(
        caller_roster.iter().any(|agent| agent.name == "worker")
            && caller_roster.iter().any(|agent| agent.name == "helper")
            && caller_roster.iter().any(|agent| agent.name == "delegate"),
        "caller roster must authorize all targets: {:?}",
        caller_roster
    );
    binding.resolve_spawn("build", "worker").unwrap();
    binding.resolve_spawn("build", "delegate").unwrap();

    // Stage with empty can_spawn: the member sees NO spawnable agents even
    // though its caller could spawn several — deeper derivation is closed.
    let report = run_workflow(
        engine.clone(),
        lead,
        &isolated,
        make_ctx(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.status, WorkflowStatus::Completed);
    assert_eq!(report.stages[0].status.to_string(), "done");
    let isolated_child = report.stages[0]
        .session
        .as_deref()
        .unwrap()
        .parse::<SessionId>()
        .unwrap();
    let results = tool_results_for(&mut event_rx, isolated_child, "list_agents");
    assert_eq!(results.len(), 1, "member probed its roster once");
    let listing = results[0].as_ref().expect("list_agents must complete");
    assert!(
        listing.contains("No agents available"),
        "empty can_spawn yields no reachable roster: {listing}"
    );

    // Stage whose OWN can_spawn names helper: exactly that delegation target is
    // visible — and nothing from the caller's broader roster leaks through.
    let report = run_workflow(
        engine.clone(),
        lead,
        &delegating,
        make_ctx(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.status, WorkflowStatus::Completed);
    let delegate_child = report.stages[0]
        .session
        .as_deref()
        .unwrap()
        .parse::<SessionId>()
        .unwrap();
    assert_ne!(delegate_child, isolated_child);
    let results = tool_results_for(&mut event_rx, delegate_child, "list_agents");
    assert_eq!(results.len(), 1);
    let listing = results[0].as_ref().expect("list_agents must complete");
    assert!(
        listing.contains("- helper"),
        "own can_spawn target must be listed: {listing}"
    );
    assert!(
        !listing.contains("worker") && !listing.contains("- build"),
        "caller-roster entries must not leak into the member listing: {listing}"
    );
}

/// Marker tool contributed by the test sidecar; completes with a fixed text so
/// the projection proves the sidecar tools reached the member turn.
#[derive(Debug)]
struct SidecarPingTool;

#[async_trait]
impl Tool for SidecarPingTool {
    fn name(&self) -> &str {
        "sidecar_ping"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        hya_proto::ToolSchema {
            name: hya_proto::ToolName::new("sidecar_ping"),
            description: "probe sidecar presence".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            output_schema: None,
        }
    }

    async fn execute(
        &self,
        _ctx: &hya_tool::ToolCtx,
        _input: serde_json::Value,
    ) -> Result<serde_json::Value, hya_tool::ToolError> {
        Ok(serde_json::json!({
            "title": "ping",
            "output": "PONG_FROM_SIDECAR"
        }))
    }
}

/// `builder` declares ONE bundle-local tool resource whose canonical id equals
/// the runtime tool name (`sidecar_ping`) so the bound sidecar implementation
/// resolves against it; `reviewer` stays plain.
fn sidecar_probe_catalog() -> Arc<hya_core::AgentCatalog> {
    let bundles = [("builder", true), ("reviewer", false)]
        .iter()
        .map(|(stable_id, declare_tool)| {
            let mut tools = Vec::new();
            if *declare_tool {
                tools.push(hya_bundle::PreparedResource {
                    local_id: "sidecar_ping".to_string(),
                    stable_id: "sidecar_ping".to_string(),
                    source_path: "tools/sidecar_ping.ts".to_string(),
                    digest: "test-only-sidecar-ping".to_string(),
                    content: "// provided by the bound sidecar at run time".to_string(),
                    aliases: Vec::new(),
                });
            }
            hya_bundle::PreparedBundle {
                format_version: 1,
                identity: hya_bundle::BundleIdentity {
                    id: format!("hya/test-{stable_id}"),
                    version: "0.0.0".to_string(),
                    publisher: "hya-tests".to_string(),
                },
                digest: format!("test-only-{stable_id}"),
                agent: hya_bundle::PreparedAgent {
                    id: AgentName::new(*stable_id),
                    description: None,
                    role: AgentRole::Subagent,
                    color: None,
                    prompt: Some(format!("{stable_id} prompt")),
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: hya_bundle::ModelPolicy::default(),
                    workdir: None,
                    spawn_lifecycle: hya_bundle::SpawnLifecycle::Transient,
                    resource_view: hya_bundle::ResourceView::default(),
                    can_spawn: Vec::new(),
                    hook_refs: Vec::new(),
                },
                tools,
                skills: Vec::new(),
                mcp: Vec::new(),
                hooks: Vec::new(),
                extensions: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    let bundles =
        hya_bundle::BundleCatalog::from_prepared(&bundles).expect("valid sidecar probe bundles");
    Arc::new(hya_core::AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog"))
}

/// Sidecar environment that resolves a bound factory ONLY for `builder`,
/// recording every resolution call (worker + verifier ordering included).
struct OnlyBuilderSidecarEnvironment {
    resolver_calls: Mutex<Vec<String>>,
}

impl OnlyBuilderSidecarEnvironment {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            resolver_calls: Mutex::new(Vec::new()),
        })
    }
}

impl hya_core::sidecar::SidecarEnvironment for OnlyBuilderSidecarEnvironment {
    fn factory_for(
        &self,
        _binding: &hya_core::TurnBinding,
        stable_id: &str,
    ) -> Result<Option<Arc<dyn hya_core::sidecar::BoundSidecarFactory>>, hya_core::CoreError> {
        self.resolver_calls
            .lock()
            .unwrap()
            .push(stable_id.to_string());
        if stable_id == "builder" {
            Ok(Some(Arc::new(MarkerSidecarFactory)))
        } else {
            Ok(None)
        }
    }
}

struct MarkerSidecarFactory;

#[async_trait]
impl hya_core::sidecar::BoundSidecarFactory for MarkerSidecarFactory {
    async fn start(
        &self,
        _start: hya_core::sidecar::SidecarStart,
    ) -> Result<Box<dyn hya_core::sidecar::SidecarHandle>, hya_core::CoreError> {
        Ok(Box::new(MarkerSidecarHandle))
    }
}

struct MarkerSidecarHandle;

#[async_trait]
impl hya_core::sidecar::SidecarHandle for MarkerSidecarHandle {
    async fn ready(&mut self) -> Result<(), hya_core::CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), hya_core::CoreError> {
        Ok(())
    }

    fn tool_bindings(&self) -> Arc<[hya_tool::ResolvedTool]> {
        Arc::from([hya_tool::ResolvedTool {
            tool: Arc::new(SidecarPingTool),
            permission: hya_tool::ToolPermission::ReadOnly,
        }])
    }
}

/// Loop stage whose worker carries its resolved sidecar factory into every
/// MemberSpec and whose independent verifier resolves against ITS OWN target
/// context — resolver order pins builder (worker) then reviewer (verifier).
#[tokio::test]
async fn loop_worker_and_verifier_reuse_target_execution_contexts() {
    const LOOP_SIDECAR_WORKFLOW: &str = r#"---
kind: Workflow
name: sidecar-loop-flow
description: Verified build Stage carrying sidecar bindings.
nodes:
  make:
    agent: builder
    directive: SIDECAR_PROBE
    mode: loop
    verify:
      agent: reviewer
      until: Marker reached.
      max_iterations: 3
---
flowchart TD
  make
"#;
    let def = compile_workflow("sidecar-loop.hya.md", LOOP_SIDECAR_WORKFLOW);

    let provider = ProbeProvider::new("sidecar_ping");
    let environment = OnlyBuilderSidecarEnvironment::new();
    let catalog = sidecar_probe_catalog();
    let (engine, mut loop_event_rx) = engine_parts(
        provider,
        catalog,
        Some(environment.clone() as Arc<dyn hya_core::sidecar::SidecarEnvironment>),
    )
    .await;
    let lead = start_lead(&engine).await;
    let workdir = support::TestDir::new("workflow-sidecar");
    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let base = engine
        .agent_spec_for_binding(&binding, &base_spec(), "build")
        .unwrap();

    let report = run_workflow(
        engine.clone(),
        lead,
        &def,
        hya_core::WorkflowRunContext {
            binding,
            caller: "build".to_string(),
            base_agent: base,
            inputs: BTreeMap::new(),
            resident_supervisor: None,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.status, WorkflowStatus::Completed);
    let stage = &report.stages[0];
    assert_eq!(
        (stage.stage.as_str(), stage.status.to_string()),
        ("make", "done".to_string())
    );
    assert!(
        stage.output.contains("PROBE_DONE") && stage.output.contains("[verified: scripted]"),
        "verified loop output must carry worker text plus verdict reason: {:?}",
        stage.output
    );

    // Both target contexts resolve through the task-path accessor — the worker
    // first, then its verifier — matching the up-front resolution order.
    assert_eq!(
        *environment.resolver_calls.lock().unwrap(),
        vec!["builder".to_string(), "reviewer".to_string()],
        "worker then verifier contexts must resolve via factory_for"
    );

    // The builder member's sidecar ACTIVATED and its marker tool executed
    // inside the child session; the verifier (no factory) never needs one.
    let child = stage
        .session
        .as_deref()
        .unwrap()
        .parse::<SessionId>()
        .unwrap();
    let pings = tool_results_for(&mut loop_event_rx, child, "sidecar_ping");
    assert_eq!(pings.len(), 1, "exactly one marker-tool probe");
    assert!(
        pings[0]
            .as_ref()
            .is_ok_and(|output| output.contains("PONG_FROM_SIDECAR")),
        "sidecar tool must execute inside the workflow member: {:?}",
        pings
    );
}
