use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, SessionId};
use crate::message::FinishReason;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub agent: String,
    pub model: String,
    pub workdir: String,
    #[serde(default)]
    pub parent: Option<SessionId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    pub session: SessionId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptRequest {
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: String,
    pub arguments: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellRequest {
    pub command: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptResponse {
    pub message: MessageId,
    pub finish: FinishReason,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub since_seq: Option<u64>,
}
