//! Tool plane for the hya agent runtime.
//!
//! This crate owns everything the model calls as a **tool** and the **permission**
//! checks that gate those calls:
//!
//! - **[`Tool`] trait and registry** — name, JSON schema, and async `execute`;
//!   builtin registration, aliases, and an immutable snapshot for an admitted turn
//!   ([`tool`]).
//! - **Permission state machine** — invocation-level policies plus resource-level
//!   allow/ask/deny rules, remembered grants, optional interceptors, and user-ask
//!   channels ([`permission`]).
//! - **Runtime planes** — session-scoped services injected through [`ToolCtx`]:
//!   interaction/questions, subagent spawning, skills, web search, LSP,
//!   mailbox, and formatters.
//! - **Native bundle ABI** — shared by the host loader and the first-party
//!   tool families that ship lockstep dynamic libraries.
//!
//! Tool authors implement [`Tool`] and register with [`ToolRegistry`]. Security
//! reviewers should start with [`PermissionPlane`], [`Action`], and
//! [`Resource`]. Downstream crates (`hya-engine`, `hya-app`) wire planes and run
//! tools; this crate stays free of the session engine.

mod agents;
/// Wall-clock budget and staged notices for a headless run.
pub mod deadline;
/// Domain extension event codecs, projection structs, and reducers (v2 bridge).
pub mod extension;
mod formatter;
/// Internal resource URLs (`artifact://`, `skill://`, `local://`) for
/// agent-owned payloads. Ordinary filesystem paths are unaffected.
pub mod handle;
/// Human interaction channel for structured questions and free-text asks.
pub mod interaction;
/// Subagent lifecycle requests (`report`, `archive`, `wait`) to the supervisor.
pub mod lifecycle;
pub mod literal_span;
mod lsp_plane;
/// Team mailbox requests and the mailbox plane used by send/channel tools.
pub mod mailbox;
pub mod media;
/// Namespaced tool names (`namespace__local`) and namespaced registration.
pub mod namespace;
/// Lockstep native-library ABI for first-party bundles.
pub mod native_bundle;
/// Nested tool calls from inside a tool call.
pub mod nested;
mod output_cap;
pub mod path;
/// Allow/ask/deny permission plane: invocation policy, resource rules, and asks.
pub mod permission;
#[cfg(unix)]
pub mod process_group;
/// Read-only activity query scoped to the caller's Project.
pub mod project_activity;
/// Project-root path boundary shared by the builtin file tools (ADR-0026).
pub mod project_scope;
mod skill;
/// Subagent spawn plane and request types used by the `task` tool.
pub mod spawn;
/// Tool trait, registry, and permission class metadata.
pub mod tool;
/// User-authored workflow plane and the `workflow` tool.
pub mod workflow_plane;
pub use workflow_plane::{
    WorkflowCommand, WorkflowCommandResult, WorkflowDelivery, WorkflowHostError,
    WorkflowMemberProjection, WorkflowMemberRole, WorkflowPlane, WorkflowProjection,
    WorkflowRequest, WorkflowRequestSink, WorkflowRouteFailureClass, WorkflowRunProjection,
    WorkflowRunResult, WorkflowRunStatus, WorkflowSendError, WorkflowStagePlan,
    WorkflowStageProjection, WorkflowStageRouteOutcome, WorkflowStageStatus,
};

pub use agents::AgentDef;
pub use formatter::{
    FormatterConfig, FormatterEntry, FormatterError, FormatterPlane, FormatterProvider,
    FormatterStatus,
};
pub use interaction::{
    InteractionError, InteractionPlane, QuestionAnswer, QuestionInfo, QuestionKind, QuestionOption,
    QuestionPrompt, QuestionReply, QuestionRequest,
};
pub use lifecycle::{
    ArchiveReason, ArchiveReceipt, LifecyclePlane, LifecycleRequest, REPORT_ALREADY_ACCEPTED,
    ReportLatch, ReportOutcome, WAIT_DEFAULT_TIMEOUT_SECS, WAIT_MAX_TIMEOUT_SECS,
    WAIT_OUTPUT_BUDGET_CHARS, WaitAckSlot, WaitMail, WaitMailAck, WaitMember, WaitMemberState,
    WaitMode, WaitOutcome, WaitSpec, WaitWake, wait_tool_schema,
};
pub use lsp_plane::{LspError, LspOperation, LspPlane, LspProvider, LspRequest};
pub use mailbox::{
    ArchivedAgentRow, ChannelInfo, ChannelPolicySnapshot, ChannelRow, MailReceipt, MailboxError,
    MailboxPlane, MailboxRequest, MemberStatusRow,
};
pub use namespace::{InvalidNamespacedName, NAMESPACE_SEPARATOR, namespace_of, namespaced_name};
pub use nested::{NestedToolPlane, NestedTools};
pub use output_cap::{
    MAX_CODING_ATTACHMENT_BYTES, MAX_CODING_OUTPUT_PATH_BYTES, MAX_TOOL_OUTPUT_CHARS,
    cap_tool_output, cap_tool_output_spilling, cap_tool_output_with_policy, json_char_len,
    serialized_string_len,
};
pub use permission::{
    Action, AskRequest, Decision, ExactSubject, GrantScope, Invocation, InvocationDecision,
    InvocationPolicy, InvocationRule, Mode, PermissionError, PermissionInterceptor,
    PermissionModel, PermissionPlane, PermissionRules, PermissionTarget, RememberScope, Resource,
    Rule, external_directory_grant_matches, glob_match,
};
pub use project_activity::{
    ProjectActivityError, ProjectActivityFile, ProjectActivityPlane, ProjectActivityRequest,
    ProjectActivityRequestMsg, ProjectActivityResult, ProjectActivitySession,
};
pub use project_scope::ProjectScope;
pub use skill::{
    MAX_SKILL_DESCRIPTION_CHARS, MAX_SKILL_NAME_CHARS, ParsedSkill, SkillCatalogEntry,
    SkillCatalogOrigin, SkillDiagnostic, SkillDiagnosticKind, SkillDiscovery, SkillError,
    SkillInfo, SkillParseError, SkillPlane, is_embedded_skill, skill_name_problem, skills_section,
};
pub use spawn::{
    HANDLE_PREFIX_MAX_LEN, InlineAgent, MemberOutcome, SpawnError, SpawnMember, SpawnRequest,
    SpawnRequestSendError, SpawnRequestSink, SpawnerPlane, sanitize_handle_prefix,
};
pub use tool::{
    AliasVisibility, DuplicateName, NamedTool, RESERVED_KERNEL_TOOLS, ResolvedTool, Tool, ToolCtx,
    ToolError, ToolOperation, ToolPermission, ToolRegistry, ToolRegistrySnapshot, ToolResultPolicy,
    is_reserved_kernel_tool,
};
