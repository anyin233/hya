//! Native implementation of the unified `hya/agent-channels` built-in bundle
//! exported via Lockstep Native Runtime ABI v2 (ADR-0001, ADR-0016, ADR-0039).
//!
//! Exports:
//! - Managed [`AgentChannelsService`] (`NativeService`)
//! - Model tools: `send`, `list_channel`, `report`, and mail-aware `wait`
//! - `channel://` [`ChannelUriSchemaHandler`] (`UriSchemaHandler`)
//! - `unread-mail-steer` [`UnreadMailSteerHook`] (`NativeHook`)
//! - Read-only [`AgentChannelsBundleApiHandler`] (`NativeBundleApiHandler`)
//! - [`AgentChannelsProjectionReducer`] (`ProjectionReducer`)
//!
//! All host interactions flow exclusively through `Arc<dyn HostRuntime>` with
//! zero dependencies on `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`,
//! or `tonic`.

use std::sync::Arc;

/// Native HTTP/gRPC `BundleApi` read endpoints for `hya/agent-channels`.
pub mod api;
/// Harness coordination tool constants and mail-only `read` wrapper.
pub mod coordination;
/// Native `tool.execute.after` unread-mail steer hook.
pub mod hook;
/// Mail address, channel id, preview, and history types.
pub mod mail;
/// Declarative AgentSet channel policy evaluation and topology helpers.
pub mod policy;
/// Durable `Event::Extension` projection reducer for `hya/agent-channels`.
pub mod reducer;
/// Managed [`hya_runtime_abi::NativeService`] owning mailbox & channel operations.
pub mod service;
/// Model tools: `send`, `list_channel`, `report`, and mail-aware `wait`.
pub mod tools;
/// Native `channel://` [`hya_runtime_abi::UriSchemaHandler`].
pub mod uri;

pub use api::AgentChannelsBundleApiHandler;
pub use coordination::{
    CHANNEL_MARKER_TOOL, COORDINATION_TOOLS, ChannelReadTool, SPAWN_TOOLS, UNDENIABLE_TOOLS,
};
pub use hook::{UNREAD_MAIL_STEER_HOOK_ID, UnreadMailSteerHook, append_steer_notice};
pub use mail::{
    CHANNEL_RANDOM_LEN, ChannelHistoryRead, ChannelKind, ChannelPolicySnapshot, ChannelRow,
    MAIL_PREVIEW_CHARS, MAIN_HANDLE, MailEndpoint, MailKind, MemberStatusRow, SteeredMail,
    dm_channel_between, is_minted_channel_id, mail_preview, mint_channel_id,
};
pub use policy::{
    AGENT_CHANNELS_PRESET_ID, BIT_FOLLOW_UP, BIT_REPORT, BIT_RESIDENT_MAIL, BIT_SEND, BIT_STEER,
    ChannelPolicy, allows_steer_message, channel_topology, mail_recipient_handles,
    snapshot_allows_bit, snapshot_allows_steer,
};
pub use reducer::{AgentChannelsProjectionReducer, decode_channels_projection};
pub use service::{
    AGENT_CHANNELS_BUNDLE_ID, AGENT_CHANNELS_SERVICE_ID, AgentChannelsService, InboxSummary,
    PreparedSteerNotice, channel_kind_template, format_steer_notice, host_error_to_tool_error,
};
pub use tools::{ListChannelTool, ReportTool, SendTool};
pub use uri::{ChannelUriSchemaHandler, parse_channel_uri};

/// Register all `hya/agent-channels` services, tools, URI handlers, hooks, APIs, and reducers.
pub fn register_bundle(out: &mut hya_runtime_abi::NativeRegistrations) {
    let service = Arc::new(AgentChannelsService::new());
    out.services
        .push(Arc::clone(&service) as Arc<dyn hya_runtime_abi::NativeService>);
    out.tools
        .push(Arc::new(SendTool::new(Arc::clone(&service))) as Arc<dyn hya_tool::Tool>);
    out.tools
        .push(Arc::new(ListChannelTool::new(Arc::clone(&service))) as Arc<dyn hya_tool::Tool>);
    out.tools
        .push(Arc::new(ReportTool) as Arc<dyn hya_tool::Tool>);
    out.tools
        .push(Arc::new(hya_tool::lifecycle::WaitTool::new(true)) as Arc<dyn hya_tool::Tool>);
    out.uri_schema_handlers
        .push(Arc::new(ChannelUriSchemaHandler::new(Arc::clone(&service)))
            as Arc<dyn hya_runtime_abi::UriSchemaHandler>);
    out.hooks
        .push(Arc::new(UnreadMailSteerHook::new(Arc::clone(&service)))
            as Arc<dyn hya_runtime_abi::NativeHook>);
    out.api_handlers
        .push(Arc::new(AgentChannelsBundleApiHandler::new(service))
            as Arc<dyn hya_runtime_abi::NativeBundleApiHandler>);
    out.projection_reducers.push(
        Arc::new(AgentChannelsProjectionReducer) as Arc<dyn hya_runtime_abi::ProjectionReducer>
    );
}

#[cfg(test)]
mod tests;
