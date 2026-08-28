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

/// Built-ins plus installed bundles resolved as one agent namespace.
pub mod agent_catalog;
/// Compiled-in agent definitions (not AgentBundles).
pub mod builtin_agents;
/// Live envelope broadcast for observers (SSE, TUI, plugins).
pub mod bus;
/// Model category resolution and member-agent construction.
pub mod category;
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
/// Loop-mode verifier/planner traits and drive helpers.
pub mod loop_mode;
/// Team mailbox service loop (event-sourced mail/channels).
pub mod mailbox;
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
/// User-authored workflow DAGs over the governed team primitives.
pub mod workflow;
/// Git worktree and tmux helpers for isolated workers.
pub mod workspace;

#[cfg(test)]
mod test_support;

pub use agent_catalog::{AgentCatalog, AgentDefinition, AgentOrigin};
pub use builtin_agents::{BUILTIN_AGENTS, BuiltinAgent, SpawnScope, builtin_agent, is_builtin_id};
pub use bus::EventBus;
pub use category::{
    CategoryEntry, CategoryRegistry, ResolvedCategory, build_member_agent, inject_skills,
};
pub use compaction::{
    CompactionConfig, CompactionPlan, MIN_RESOLVED_THRESHOLD, ModelSummarizer, SummarizeOptions,
    Summarizer, compact_with, estimate_tokens, measured_tokens, needs_compaction,
    needs_compaction_at, plan_compaction, plan_compaction_at, resolved_threshold, tokens_in_use,
};
pub use completion::{
    GoalEvaluator, IterationDriver, ModelGoalEvaluator, RunOutcome, SafetyCaps, Verdict, run_goal,
};
pub use engine::{
    AdmissionMemberIdentity, AgentSpec, BoundSpawnRequest, BoundSpawnSender, BoundWorkflowRequest,
    BoundWorkflowSender, CreateSession, RuntimeCatalogRefresh, SessionEngine,
    SpawnAdmissionOutcome,
};
pub use error::CoreError;
pub use hooks::{
    ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome,
    HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome, NoopHookHost,
    TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome,
    ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, ToolOutcomeNative,
};
pub use loop_mode::{
    EvidenceQuality, LoopConfig, LoopPlanner, LoopVerifier, PlannerOutput, VerifierVerdict,
    cost_preflight, drive_loop, run_loop,
};
pub use mailbox::run_mailbox_service;
pub use orchestrator::{OperationReservation, SubagentGovernor, SubagentLimits, TeamBudget};
pub use prompt::{
    PromptEnv, build_system_prompt, context_file_reads, discover_context_files,
    render_environment_and_context, today,
};
pub use resident::{ResidentRecovery, ResidentRecoveryReport, ResidentSupervisor};
pub use runtime_registry::{
    AgentResourcePolicy, RuntimeCandidate, RuntimeEffectiveManifest, RuntimeRefreshError,
    RuntimeRegistry, RuntimeSource, RuntimeSourceExport, RuntimeSourceId, RuntimeSourceKind,
    RuntimeSourceManifest, RuntimeSourceOwner, TurnBinding,
};
pub use sidecar::{
    BoundSidecarFactory, SidecarEnvironment, SidecarHandle, SidecarLifecycle, SidecarStart,
};
pub use subagent::{
    MemberEvidence, MemberSpec, MemberStatus, TeamAdmissionError, TeamEvidenceEnvelope,
    pre_admit_team, project_envelope, project_envelope_for_actor, run_pre_admitted_member,
    run_pre_admitted_team, run_pre_admitted_team_for_actor, run_team,
};
pub use workflow::{
    CompiledWorkflow, DurableWorkflowAdmission, DurableWorkflowSelection, FailurePolicy,
    PreparedWorkflowRun, StageMode, StageReport, StageStatus, VerifySpec, WorkflowDefinition,
    WorkflowError, WorkflowPlan, WorkflowRevision, WorkflowRunContext, WorkflowRunReport,
    WorkflowStage, WorkflowStatus, discover_workflow_files_in_root, load_workflow_file,
    prepare_workflow_run, prepare_workflow_run_for_actor, run_workflow, workflow_dirs_for_workdir,
};
pub use workspace::{TmuxPaneManager, WorktreeManager};
