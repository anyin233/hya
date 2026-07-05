//! `hya_sdk` — typed client + wire types + event reducer for the backend server.

/// HTTP header that scopes each request to a working directory. This is the single wire-protocol
/// string coupled to the current backend; change it here (or translate it in the native bridge)
/// when porting to a different backend.
pub const DIRECTORY_HEADER: &str = "x-opencode-directory";

pub mod error;
pub mod events;
pub mod native;
pub mod pending;
pub mod reducer;
pub mod server;
pub mod store;
pub mod team;
pub mod types;

mod client;
pub use client::{ApiClient, Client, HttpClient, Transport};
pub use error::SdkError;
pub use events::stream_global_events;
pub use native::{NativeBridge, NativeClient};
pub use pending::{PendingClient, PendingSlot};
pub use reducer::{Data, V2Event};
pub use server::ServerHandle;
pub use store::{MemberProjection, MessageStore, StoredPart};
pub use team::{ChannelProjection, MailEndpoint, MailMessage, RosterEntry, TeamProjection};
pub use types::{
    Agent, Config, EventPayload, GlobalEvent, Message, MessageTime, Part, Session, SessionMessage,
    ToolPart,
};
