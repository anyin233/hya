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
//! - **Concrete builtins** — read/write/edit/patch, shell, glob/grep/find, task,
//!   webfetch/websearch, and team mailbox tools.
//!
//! Tool authors implement [`Tool`] and register with [`ToolRegistry`]. Security
//! reviewers should start with [`PermissionPlane`], [`Action`], and
//! [`Resource`]. Downstream crates (`hya-core`, `hya-app`) wire planes and run
//! tools; this crate stays free of the session engine.

mod agents;
mod apply_patch;
mod edit;
mod edit_replace;
mod file_diff;
mod formatter;
mod formatter_catalog;
mod formatter_command;
mod formatter_definition;
/// Human interaction channel for structured questions and free-text asks.
pub mod interaction;
mod invalid;
mod lsp;
mod lsp_path;
mod lsp_plane;
mod lsp_post_edit;
/// Team mailbox requests and the mailbox plane used by send/roster/channel tools.
pub mod mailbox;
mod output_cap;
/// Allow/ask/deny permission plane: invocation policy, resource rules, and asks.
pub mod permission;
mod plan;
mod question;
mod read;
mod read_media;
mod read_text;
mod shell;
mod skill;
mod skill_catalog;
/// Subagent spawn plane and request types used by the `task` tool.
pub mod spawn;
mod task;
/// In-memory per-session todo list plane and the `todowrite` tool types.
pub mod todo;
/// Tool trait, registry, permission class metadata, and local search builtins.
pub mod tool;
mod utf8_bom;
mod webfetch;
mod websearch;
mod websearch_response;
/// User-authored workflow plane and the `workflow` tool.
pub mod workflow_plane;
mod write;
pub use workflow_plane::{
    WorkflowAction, WorkflowOutcome, WorkflowPlane, WorkflowReply, WorkflowReplyPayload,
    WorkflowRequest, WorkflowRequestSink, WorkflowSendError, WorkflowStageOutcome, WorkflowSummary,
    WorkflowTool,
};

pub use agents::AgentDef;
pub use formatter::{
    BuiltinFormatterProvider, FormatterConfig, FormatterEntry, FormatterError, FormatterPlane,
    FormatterProvider, FormatterStatus,
};
pub use interaction::{
    InteractionError, InteractionPlane, QuestionAnswer, QuestionInfo, QuestionKind, QuestionOption,
    QuestionPrompt, QuestionReply, QuestionRequest,
};
pub use lsp_plane::{LspError, LspOperation, LspPlane, LspProvider, LspRequest};
pub use mailbox::{ChannelInfo, MailReceipt, MailboxError, MailboxPlane, MailboxRequest};
pub use output_cap::{MAX_TOOL_OUTPUT_CHARS, cap_tool_output};
pub use permission::{
    Action, AskRequest, Decision, ExactSubject, Invocation, InvocationDecision, InvocationPolicy,
    InvocationRule, Mode, PermissionError, PermissionInterceptor, PermissionModel, PermissionPlane,
    PermissionRules, PermissionTarget, RememberScope, Resource, Rule, glob_match,
};
pub use skill::SkillPlane;
pub use skill_catalog::{
    ParsedSkill, SkillCatalogEntry, discover_skills, discover_skills_from_dirs, parse_skill,
    skill_dirs_for_workdir, skills_section,
};
pub use spawn::{
    InlineAgent, MemberOutcome, SpawnError, SpawnMember, SpawnRequest, SpawnRequestSendError,
    SpawnRequestSink, SpawnerPlane,
};
pub use todo::{TodoItem, TodoPlane, TodoPriority, TodoStatus};
pub use tool::{
    DuplicateName, ResolvedTool, Tool, ToolCtx, ToolError, ToolOperation, ToolPermission,
    ToolRegistry, ToolRegistrySnapshot,
};
pub use websearch::{WebSearchConfig, WebSearchPlane, WebSearchProvider};
