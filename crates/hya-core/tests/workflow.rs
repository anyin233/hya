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
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use futures::stream;
use hya_bundle::AgentRole;
use hya_core::{
    CreateSession, EventBus, FailurePolicy, SessionEngine, SubagentGovernor, SubagentLimits,
    WorkflowDef, WorkflowStatus, load_workflow_file, run_workflow,
};
use hya_proto::{AgentName, Event, FinishReason, Message, MessageId, ModelRef, Part, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, Tool, ToolRegistry};
use tokio_util::sync::CancellationToken;

/// Records every completion request (session + rendered user prompt text) and
/// answers with a fixed marker so handoff assertions can trace flow.
///
/// Verifier-gate judgments are scripted separately: the Nth verifier judgment
/// answers `{"met": bool}` taken from `verdicts[N-1]` (missing entries count as
/// met), which lets loop tests pin exactly when the stop decision happens.
struct RecordingProvider {
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
        _req: &CompletionRequest,
        session: SessionId,
        user_text: String,
        reply: String,
    ) -> Result<EventStream, ProviderError> {
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

const TWO_STAGE_YAML: &str = r#"
name: feature-flow
description: explore then build
inputs:
  target: what to explore
stages:
  - id: explore
    agent: explorer
    prompt: "EXPLORE {{inputs.target}}"
  - id: build
    agent: builder
    needs: [explore]
    prompt: "BUILD from\n{{explore}}"
"#;

fn write_workflow(workdir: &std::path::Path, file_name: &str, body: &str) -> PathBuf {
    let dir = workdir.join(".hya/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file_name);
    std::fs::write(&path, body).unwrap();
    path
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
    let path = write_workflow(workdir.path(), "feature.yaml", TWO_STAGE_YAML);
    let def: WorkflowDef = load_workflow_file(&path).unwrap();

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
    let yaml = r#"
name: fan-flow
description: plan fans out to two impls then reviews both
stages:
  - id: plan
    agent: planner
    prompt: "PLAN"
  - id: impl_a
    agent: builder
    needs: [plan]
    prompt: "IMPL A per\n{{plan}}"
  - id: impl_b
    agent: builder
    needs: [plan]
    prompt: "IMPL B per\n{{plan}}"
  - id: review
    agent: reviewer
    needs: [impl_a, impl_b]
    prompt: "REVIEW both:\n{{impl_a}}\n{{impl_b}}"
"#;
    let workdir = support::TestDir::new("workflow-fan");
    let path = write_workflow(workdir.path(), "fan.yaml", yaml);
    let def = load_workflow_file(&path).unwrap();

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
        .find(|(_, text)| text.starts_with("REVIEW both:"))
        .expect("review prompt appears once");
    assert!(
        review_prompt.contains("## upstream stage `impl_a`")
            && review_prompt.contains("## upstream stage `impl_b`"),
        "join stage receives both fanned-in sections: {review_prompt:?}"
    );
}

/// `on_member_failure: collect_all` keeps the DAG running: the failed stage is
/// reported FAILED in the joined directive instead of aborting the workflow.
#[tokio::test]
async fn collect_all_policy_reports_failures_into_the_join() {
    let yaml = r#"
name: tolerant-flow
description: one impl may fail; review still aggregates
on_member_failure: collect_all
stages:
  - id: good
    agent: builder
    prompt: "IMPL GOOD"
  - id: bad
    agent: builder
    prompt: "FAIL_ME IMPL BAD"
  - id: review
    agent: reviewer
    needs: [good, bad]
    prompt: "REVIEW all:\n{{good}}\n{{bad}}"
"#;
    let workdir = support::TestDir::new("workflow-collect-all");
    let path = write_workflow(workdir.path(), "tolerant.yaml", yaml);
    let def: WorkflowDef = load_workflow_file(&path).unwrap();
    assert_eq!(def.on_member_failure, FailurePolicy::CollectAll);

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
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.status, WorkflowStatus::Completed);
    let bad = report.stages.iter().find(|s| s.stage == "bad").unwrap();
    assert_eq!(bad.status.to_string(), "failed");

    let prompts = provider.prompts();
    let (_, review_prompt) = prompts
        .iter()
        .find(|(_, text)| text.starts_with("REVIEW all:"))
        .expect("review still ran");
    assert!(
        review_prompt.contains("`good`") && review_prompt.contains("WORKER_DONE"),
        "healthy upstream flows through"
    );
    assert!(
        review_prompt.contains("`bad`") && review_prompt.contains("FAILED"),
        "failed upstream is declared to the joining stage: {review_prompt:?}"
    );
}

/// A failure after a loop stage's first round is still a stage outcome. Under
/// `collect_all`, its downstream join runs and receives explicit FAILED
/// evidence instead of the executor returning early with a workflow error.
#[tokio::test]
async fn collect_all_applies_to_resumed_loop_worker_failures() {
    let yaml = r#"
name: tolerant-loop
description: a later loop round may fail; review still aggregates
on_member_failure: collect_all
stages:
  - id: build
    agent: builder
    prompt: "BUILD first round"
    mode: loop
    verify:
      agent: reviewer
      until: FAIL_LOOP_RESUME
      max_iterations: 3
  - id: review
    agent: reviewer
    needs: [build]
    prompt: "REVIEW loop:\n{{build}}"
"#;
    let workdir = support::TestDir::new("workflow-collect-loop-failure");
    let path = write_workflow(workdir.path(), "tolerant-loop.yaml", yaml);
    let def: WorkflowDef = load_workflow_file(&path).unwrap();
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
        },
        CancellationToken::new(),
    )
    .await
    .expect("collect_all must turn the loop error into failed evidence");

    assert_eq!(report.status, WorkflowStatus::Completed);
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
        .find(|(_, text)| text.starts_with("REVIEW loop:"))
        .expect("review receives loop evidence");
    assert!(review_prompt.contains("`build`") && review_prompt.contains("FAILED"));
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
    def: &WorkflowDef,
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
        },
        CancellationToken::new(),
    )
    .await
}

#[tokio::test]
async fn duplicate_stage_ids_are_rejected() {
    let yaml = r#"
name: dup-flow
description: d
stages:
  - id: a
    agent: builder
    prompt: p
  - id: a
    agent: builder
    prompt: q
"#;
    let def = serde_norway::from_str::<WorkflowDef>(yaml).unwrap();
    let run = setup("dup").await;
    let error = run_def(&run, &def, BTreeMap::new())
        .await
        .expect_err("duplicate ids must fail");
    assert!(error.to_string().contains("duplicate stage id"), "{error}");
    assert!(
        run.provider.prompts().is_empty(),
        "validation must fail before any member turn"
    );
}

#[tokio::test]
async fn missing_declared_input_fails_before_any_spawn() {
    let yaml = r#"
name: input-flow
description: d
inputs:
  target: required
stages:
  - id: s
    agent: builder
    prompt: "do {{inputs.target}}"
"#;
    let path = write_workflow(
        std::path::Path::new("/tmp/hya-workflow-neg-input"),
        "input.yaml",
        yaml,
    );
    let def = load_workflow_file(&path).unwrap();
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
    let yaml = r#"
name: ghost-flow
description: d
stages:
  - id: s
    agent: ghost-agent-not-installed
    prompt: p
"#;
    let def = serde_norway::from_str::<WorkflowDef>(yaml).unwrap();
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
    let yaml = r#"
name: wide-flow
description: three stages exceed a two-slot budget
stages:
  - id: one
    agent: builder
    prompt: p
  - id: two
    agent: builder
    prompt: p
  - id: three
    agent: reviewer
    needs: [one, two]
    prompt: j
"#;
    let def = serde_norway::from_str::<WorkflowDef>(yaml).unwrap();
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
    let yaml = r#"
name: abort-flow
description: d
on_member_failure: fail_fast
stages:
  - id: broken
    agent: builder
    prompt: "FAIL_ME explode"
  - id: after
    agent: reviewer
    needs: [broken]
    prompt: "should never render {{broken}}"
"#;
    let def = serde_norway::from_str::<WorkflowDef>(yaml).unwrap();
    let run = setup("fail-fast").await;
    let report = run_def(&run, &def, BTreeMap::new())
        .await
        .expect("workflow completes with failure semantics");
    assert_eq!(report.status, WorkflowStatus::Failed);
    assert_eq!(report.stages.len(), 1, "downstream stage must not run");
    assert_eq!(report.stages[0].stage, "broken");
    assert_eq!(report.stages[0].status.to_string(), "failed");
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
    let yaml = r#"
name: loop-flow
description: verify-gated build stage
stages:
  - id: build
    agent: builder
    mode: loop
    verify:
      agent: reviewer
      until: tests pass
      max_iterations: 4
    prompt: "LOOP_BUILD the thing"
"#;
    let def = serde_norway::from_str::<WorkflowDef>(yaml).unwrap();
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
    let yaml = r#"
name: cancellable-loop
description: verifier cancellation reaches its governed member
stages:
  - id: build
    agent: builder
    prompt: "BUILD before verifier"
    mode: loop
    verify:
      agent: reviewer
      until: tests pass
      max_iterations: 3
"#;
    let workdir = support::TestDir::new("workflow-cancel-verifier");
    let path = write_workflow(workdir.path(), "cancellable-loop.yaml", yaml);
    let def: WorkflowDef = load_workflow_file(&path).unwrap();
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
    const ISOLATED_YAML: &str = r#"
name: isolated-flow
description: worker cannot delegate further
stages:
  - id: only
    agent: worker
    prompt: "ROSTER_PROBE_ISOLATED"
"#;
    const DELEGATING_YAML: &str = r#"
name: delegating-flow
description: delegate may hand off to helper only
stages:
  - id: spawn_step
    agent: delegate
    prompt: "ROSTER_PROBE_DELEGATE"
"#;

    let isolated: WorkflowDef = serde_norway::from_str(ISOLATED_YAML).unwrap();
    let delegating: WorkflowDef = serde_norway::from_str(DELEGATING_YAML).unwrap();

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
    const LOOP_SIDECAR_YAML: &str = r#"
name: sidecar-loop-flow
description: verified build stage carrying sidecar bindings
stages:
  - id: make
    agent: builder
    mode: loop
    verify:
      agent: reviewer
      until: marker reached
      max_iterations: 3
    prompt: "SIDECAR_PROBE"
"#;
    let def: WorkflowDef = serde_norway::from_str(LOOP_SIDECAR_YAML).unwrap();

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
