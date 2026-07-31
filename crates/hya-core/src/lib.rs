//! `hya-core` — SessionEngine, the agent turn loop, and the in-process EventBus.
//! Team orchestration and the completion (goal + loop) engines land in later phases.

pub mod bus;
pub mod category;
pub mod compaction;
pub mod completion;
pub mod engine;
pub mod error;
pub mod hooks;
pub mod loop_mode;
pub mod mailbox;
pub mod orchestrator;
pub mod prompt;
pub mod resident;
pub mod runtime_registry;
pub mod subagent;
pub mod title;
pub mod workspace;

pub use bus::EventBus;
pub use category::{
    CategoryEntry, CategoryRegistry, ResolvedCategory, build_member_agent, inject_skills,
};
pub use compaction::{
    CompactionConfig, ModelSummarizer, Summarizer, compact_with, estimate_tokens, needs_compaction,
};
pub use completion::{
    GoalEvaluator, IterationDriver, ModelGoalEvaluator, RunOutcome, SafetyCaps, Verdict, run_goal,
};
pub use engine::{AgentSpec, CreateSession, SessionEngine, SpawnAdmissionOutcome};
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
pub use prompt::{PromptEnv, build_system_prompt};
pub use resident::ResidentSupervisor;
pub use runtime_registry::{
    RuntimeCandidate, RuntimeEffectiveManifest, RuntimeRefreshError, RuntimeRegistry,
    RuntimeSource, RuntimeSourceExport, RuntimeSourceId, RuntimeSourceKind, RuntimeSourceManifest,
    RuntimeSourceOwner, TurnBinding,
};
pub use subagent::{
    MemberEvidence, MemberSpec, MemberStatus, TeamAdmissionError, TeamEvidenceEnvelope,
    pre_admit_team, project_envelope, run_pre_admitted_team, run_team,
};
pub use workspace::{TmuxPaneManager, WorktreeManager};
