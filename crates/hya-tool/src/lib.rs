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
//!   interaction/questions, subagent spawning, todos, skills, web search, LSP,
//!   mailbox, and formatters.
//! - **Native bundle loading** — all five builtin families execute from
//!   trusted, lockstep dynamic libraries. Their implementations live in bundles.
//!
//! Tool authors implement [`Tool`] and register with [`ToolRegistry`]. Security
//! reviewers should start with [`PermissionPlane`], [`Action`], and
//! [`Resource`]. Downstream crates (`hya-core`, `hya-app`) wire planes and run
//! tools; this crate stays free of the session engine.

mod agents;
mod base_tools;
mod formatter;
mod formatter_catalog;
mod formatter_command;
mod formatter_definition;
/// Internal resource URLs (`artifact://`, `skill://`, `local://`) for
/// agent-owned payloads. Ordinary filesystem paths are unaffected.
pub mod handle;
/// Human interaction channel for structured questions and free-text asks.
pub mod interaction;
/// Subagent lifecycle requests (`report`, `archive`, `wait`) to the supervisor.
pub mod lifecycle;
mod lsp_path;
mod lsp_plane;
/// Team mailbox requests and the mailbox plane used by send/channel tools.
pub mod mailbox;
/// Namespaced tool names (`namespace__local`) and namespaced registration.
pub mod namespace;
/// Lockstep native-library loading contract for first-party tool bundles.
pub mod native_bundle;
mod output_cap;
/// Allow/ask/deny permission plane: invocation policy, resource rules, and asks.
pub mod permission;
/// Project-root path boundary shared by the builtin file tools (ADR-0026).
pub mod project_scope;
mod skill;
mod skill_catalog;
/// Subagent spawn plane and request types used by the `task` tool.
pub mod spawn;
/// In-memory per-session todo list plane and the `todo__` tool group.
pub mod todo;
/// Tool trait, registry, and permission class metadata.
pub mod tool;
mod websearch;
/// User-authored workflow plane and the `workflow` tool.
pub mod workflow_plane;
pub use workflow_plane::{
    WorkflowHostError, WorkflowPlane, WorkflowRequest, WorkflowRequestSink, WorkflowSendError,
};

pub use agents::AgentDef;
pub use base_tools::{
    AliasVisibility, BaseToolAlias, BaseToolExposure, BaseToolScheme, BaseToolsPreset,
    base_tools_preset, tool_bundle_presets,
};
pub use formatter::{
    BuiltinFormatterProvider, FormatterConfig, FormatterEntry, FormatterError, FormatterPlane,
    FormatterProvider, FormatterStatus,
};
pub use interaction::{
    InteractionError, InteractionPlane, QuestionAnswer, QuestionInfo, QuestionKind, QuestionOption,
    QuestionPrompt, QuestionReply, QuestionRequest,
};
pub use lifecycle::{
    ArchiveReceipt, LifecyclePlane, LifecycleRequest, REPORT_ALREADY_ACCEPTED, ReportLatch,
    WAIT_DEFAULT_TIMEOUT_SECS, WAIT_MAX_TIMEOUT_SECS, WAIT_RESULT_BUDGET, WaitMail, WaitMember,
    WaitMemberState, WaitMode, WaitOutcome, WaitSpec, WaitWake, wait_tool_schema,
};
pub use lsp_plane::{LspError, LspOperation, LspPlane, LspProvider, LspRequest};
pub use mailbox::{
    ArchivedAgentRow, ChannelInfo, ChannelPolicySnapshot, ChannelRow, MailReceipt, MailboxError,
    MailboxPlane, MailboxRequest, MemberStatusRow,
};
pub use namespace::{
    InvalidNamespacedName, NAMESPACE_SEPARATOR, NamespacedRegisterError, namespace_of,
    namespaced_name,
};
pub use output_cap::{
    MAX_TOOL_OUTPUT_CHARS, cap_tool_output, cap_tool_output_spilling, cap_tool_output_with_policy,
};
pub use permission::{
    Action, AskRequest, Decision, ExactSubject, Invocation, InvocationDecision, InvocationPolicy,
    InvocationRule, Mode, PermissionError, PermissionInterceptor, PermissionModel, PermissionPlane,
    PermissionRules, PermissionTarget, RememberScope, Resource, Rule, glob_match,
};
pub use project_scope::ProjectScope;
pub use skill::{SkillError, SkillInfo, SkillPlane};
pub use skill_catalog::{
    ParsedSkill, SkillCatalogEntry, SkillCatalogOrigin, builtin_skills, core_skills_preset_bytes,
    discover_skills, discover_skills_from_dirs, discover_skills_with_builtins, is_embedded_skill,
    merge_skill_catalog, parse_skill, skill_dirs_for_workdir, skills_section,
};
pub use spawn::{
    HANDLE_PREFIX_MAX_LEN, InlineAgent, MemberOutcome, SpawnError, SpawnMember, SpawnRequest,
    SpawnRequestSendError, SpawnRequestSink, SpawnerPlane, sanitize_handle_prefix,
};
pub use todo::{SessionTodos, TodoItem, TodoPlane, TodoStatus};
pub use tool::{
    DuplicateName, NamedTool, ResolvedTool, Tool, ToolCtx, ToolError, ToolOperation,
    ToolPermission, ToolRegistry, ToolRegistrySnapshot, ToolResultPolicy,
};
pub use websearch::{WebSearchConfig, WebSearchPlane, WebSearchProvider};
