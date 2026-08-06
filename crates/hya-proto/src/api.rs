//! HTTP/JSON request and response DTOs for the native session API.
//!
//! These types are the bodies (and query params) shared by `hya-server` and
//! `hya-client`. They are intentionally small and independent of `Event`.

use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, SessionId};
use crate::message::FinishReason;

/// Body for `POST` create-session: who runs, where, and optional parent link.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    /// Agent name/catalog id to bind as the session's default agent.
    pub agent: String,
    /// Model reference the session starts on (provider/model form as configured).
    pub model: String,
    /// Absolute workdir for tools and relative paths in this session.
    pub workdir: String,
    /// When set, marks this session as a child of `parent` (subagent / team tree).
    #[serde(default)]
    pub parent: Option<SessionId>,
}

/// Response after a session is created: use `session` for all further calls.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    /// Newly minted (or resumed) session id.
    pub session: SessionId,
}

/// Body for admitting a plain user prompt into a session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptRequest {
    /// User text to record as the next user message and run a turn on.
    pub text: String,
}

/// Body for admitting a slash-command as a user message (compat/native command path).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandRequest {
    /// Command name without the leading `/` (for example `compact`).
    pub command: String,
    /// Raw argument string after the command name.
    pub arguments: String,
    /// Optional full text to store if the client already composed the message body.
    #[serde(default)]
    pub text: Option<String>,
}

/// Body for a direct shell turn (no model round; synthetic tool call).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellRequest {
    /// Shell command line to run via the builtin `shell` tool.
    pub command: String,
}

/// Result of a completed prompt or command turn (native API).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptResponse {
    /// Assistant message id that finished (or was force-finished).
    pub message: MessageId,
    /// Terminal finish reason for that assistant message.
    pub finish: FinishReason,
}

/// Query parameters for replaying or streaming events after a sequence watermark.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EventsQuery {
    /// When set, return only envelopes with `seq` strictly greater than this value.
    #[serde(default)]
    pub since_seq: Option<u64>,
}
