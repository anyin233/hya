//! Agent runtime: session engine, turn loop, orchestration, and live event fan-out.
//!
//! This crate owns:
//! - **[`SessionEngine`]** — create/delete sessions, admit prompts, run turns,
//!   compact/summarize, and emit canonical events through the store and bus.
//! - **[`EventBus`]** — in-process broadcast of [`hya_proto::Envelope`] for SSE/TUI.
//! - **Extension traits** implemented by the app/plugin layers:
//!   [`HookDispatcher`], [`RuntimeCatalogRefresh`], [`Summarizer`], goal/loop
//!   gates and verifiers, and [`RuntimeSourceOwner`].
//! - **Team/subagent orchestration** — admission, governors, resident supervisors,
//!   and mailbox delivery over the same event-sourced log.
//!
//! Downstream crates wire planes (permission, tools, mailbox) and plugins; this
//! crate stays free of terminal UI and HTTP routing.

/// Hardcoded subagent recursion cap (ADR-0015): the interactive root is depth
/// 0 and may open exactly two subagent layers beneath it. Not configurable —
/// admission depth checks and the depth-2 tool advertisement filter both
/// read this constant.
pub const MAX_SUBAGENT_DEPTH: u32 = 2;

/// Built-ins plus installed bundles resolved as one agent namespace.
pub mod agent_catalog;
/// Compiled-in agent definitions (not AgentBundles).
pub mod builtin_agents;
/// Bundle-registered HTTP endpoints and the host reads behind their capability.
pub mod bundle_apis;
mod bundle_hooks;
/// Live envelope broadcast for observers (SSE, TUI, plugins).
pub mod bus;
/// Model category resolution and member-agent construction.
pub mod category;
/// Trusted defaults and restrictive bundle policy for runtime channels.
pub mod channel_policy;
/// Context compaction thresholds, token estimates, and summarizer trait.
pub mod compaction;
/// Goal-mode iteration driver, safety caps, and independent evaluators.
pub mod completion;
/// Session engine, agent specs, and turn admission.
pub mod engine;
/// Shared error type for the core runtime.
pub mod error;
/// Plugin/host hook dispatch contract and native payload types.
pub mod hooks;
/// Team mailbox service loop (event-sourced mail/channels).
pub mod lifecycle;
/// Loop-mode verifier/planner traits and drive helpers.
pub mod loop_mode;
pub mod mailbox;
/// Real per-family tokenizers backing the usage-ledger fallback estimate.
pub mod model_tokenizers;
/// Subagent concurrency governor and team budgets.
pub mod orchestrator;
/// System prompt construction and context file discovery.
pub mod prompt;
/// Long-lived resident actors and recovery.
pub mod resident;
/// Immutable runtime snapshots, sources, and turn bindings.
pub mod runtime_registry;
/// Bundle sidecar lifecycle for executable public packages.
pub mod sidecar;
/// Multi-member team admission and fan-out execution.
pub mod subagent;
/// Session title generation helpers.
pub mod title;
/// Token accounting: local estimation and provider-usage reliability.
pub mod tokens;
/// User-authored workflow DAGs over the governed team primitives.
pub mod workflow;
/// Git worktree and tmux helpers for isolated workers.
pub mod workspace;

#[cfg(test)]
mod test_support;

pub use agent_catalog::{AgentCatalog, AgentDefinition, AgentOrigin};
pub use builtin_agents::{
    BuiltinAgent, CORE_AGENTS_PRESET_ID, CoreAgentsPreset, SpawnScope, builtin_agent,
    builtin_agents, core_agents_preset, is_builtin_id,
};
pub use bundle_apis::{
    ApiMethod, ApiPathTemplate, ApiScope, BundleApiCall, BundleApiError, BundleApiOutcome,
    BundleApiProvider, BundleApiReply, BundleApiRequest, HostSessionReads,
    MAX_BUNDLE_API_BODY_BYTES, PublishedBundleApis, SessionUsageReport, SourceApi, SourceApis,
    StoreSessionReads, UsageScope,
};
pub use bus::EventBus;
pub use category::{
    CategoryEntry, CategoryRegistry, ResolvedCategory, apply_agent_model_preference,
    apply_spawn_model_policy, build_member_agent, eligible_agent_model_preference, inject_skills,
    resolve_configured_agent_model, resolve_dispatch_model,
};
pub use channel_policy::{AGENT_CHANNELS_PRESET_ID, ChannelPolicy};
pub use compaction::{
    CompactionConfig, CompactionPlan, CompactionRung, MIN_RESOLVED_THRESHOLD, ModelSummarizer,
    SummarizeOptions, Summarizer, UsageCollector, compact_with, estimate_tokens,
    handoff_request_messages, measured_tokens, needs_compaction, needs_compaction_at,
    parse_method_order, plan_compaction, plan_compaction_at, plan_handoff, resolved_threshold,
    snapcompact_archive, tokens_in_use,
};
pub use completion::{GateOutcome, IterationGate, validate_goal_condition};
pub use completion::{
    GoalEvaluator, IterationDriver, ModelGoalEvaluator, RunOutcome, SafetyCaps, Verdict, run_goal,
};
pub use engine::{
    AdmissionMemberIdentity, AgentSpec, BoundSpawnRequest, BoundSpawnSender, BoundWorkflowRequest,
    BoundWorkflowSender, CreateSession, DRAIN_DEADLINE, RuntimeCatalogRefresh, SessionEngine,
    SpawnAdmissionOutcome, TurnBoundaryObserver, TurnDrainReport, TurnLease, advertise_tool,
};
pub use error::CoreError;
pub use hooks::{
    AgentSpawnInput, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CompactionAfterInput, CompactionBeforeInput, CompactionDecision,
    CompactionResolution, CompactionTrigger, HookChain, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome, ModelFailureClass, ModelFallbackInput, ModelFallbackOutcome,
    NoopHookHost, SessionLifecycleInput, TextCompleteInput, TextCompleteOutcome,
    ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome, ToolOutcomeNative, resolve_compaction_decision,
};
pub use lifecycle::run_lifecycle_service;
pub use loop_mode::{
    EvidenceQuality, HookLoopPlanner, HookLoopVerifier, LoopConfig, LoopGate, LoopPlanner,
    LoopPredicate, LoopPredicateOutcome, LoopVerifier, PlannerOutput, PredicateMode,
    VerifierVerdict, cost_preflight, drive_loop, run_loop,
};
pub use mailbox::run_mailbox_service;
pub use orchestrator::{OperationReservation, SubagentGovernor, SubagentLimits, TeamBudget};
pub use prompt::{
    PromptEnv, build_system_prompt, context_file_reads, discover_context_files,
    render_environment_and_context, today,
};
pub use resident::{ResidentRecovery, ResidentRecoveryReport, ResidentSupervisor};
pub use runtime_registry::{
    AgentModelConfiguration, AgentResourcePolicy, RuntimeCandidate, RuntimeEffectiveManifest,
    RuntimeRefreshError, RuntimeRegistry, RuntimeSource, RuntimeSourceExport, RuntimeSourceId,
    RuntimeSourceKind, RuntimeSourceManifest, RuntimeSourceOwner, SourceSchema, TurnBinding,
};
pub use sidecar::{
    BoundSidecarFactory, SidecarEnvironment, SidecarHandle, SidecarLifecycle, SidecarStart,
};
pub use subagent::{
    MemberEvidence, MemberSpec, MemberStatus, TeamAdmissionError, TeamEvidenceEnvelope,
    pre_admit_team, project_envelope, project_envelope_for_actor, run_pre_admitted_member,
    run_pre_admitted_team, run_pre_admitted_team_for_actor, run_team,
};
pub use tokens::{
    CalibratedTokenizer, TokenAccounting, TokenAccountingMode, TokenCount, TokenSource, Tokenizer,
};
pub use workflow::{
    CompiledWorkflow, DurableWorkflowAdmission, DurableWorkflowSelection, FailurePolicy,
    PreparedWorkflowRun, StageMode, StageReport, StageStatus, VerifySpec, WorkflowDefinition,
    WorkflowError, WorkflowModelAssignment, WorkflowModelCandidate, WorkflowModelRoute,
    WorkflowModelRouteCandidate, WorkflowPlan, WorkflowRevision, WorkflowRoutingContext,
    WorkflowRunContext, WorkflowRunReport, WorkflowStage, WorkflowStatus,
    discover_workflow_files_in_root, load_workflow_file, prepare_workflow_run,
    prepare_workflow_run_for_actor, run_workflow, workflow_dirs_for_workdir,
};
pub use workspace::{TmuxPaneManager, WorktreeManager};
