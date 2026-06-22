use serde::Serialize;
use serde_json::{Value, json};
use yaca_proto::{FinishReason, MessageId, PartProjection, Role, SessionId};

#[derive(Clone, Debug, Serialize)]
pub(super) struct OpenCodeMessage {
    info: OpenCodeMessageInfo,
    parts: Vec<Value>,
}

impl OpenCodeMessage {
    pub(super) fn id(&self) -> &str {
        &self.info.id
    }

    pub(super) fn has_part(&self, part: &str) -> bool {
        self.parts
            .iter()
            .any(|item| item["id"].as_str() == Some(part))
    }

    pub(super) fn part(&self, part: &str) -> Option<Value> {
        self.parts
            .iter()
            .find(|item| item["id"].as_str() == Some(part))
            .cloned()
    }
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeMessageInfo {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish: Option<FinishReason>,
}

pub(super) fn opencode_message(
    session: SessionId,
    message: &yaca_proto::MessageProjection,
) -> OpenCodeMessage {
    let message_id = message.id.to_string();
    OpenCodeMessage {
        info: OpenCodeMessageInfo {
            id: message_id,
            session_id: session.to_string(),
            role: role_name(message.role),
            finish: message.finish,
        },
        parts: message
            .parts
            .iter()
            .map(|part| opencode_part(session, message.id, part))
            .collect(),
    }
}

fn opencode_part(session: SessionId, message: MessageId, part: &PartProjection) -> Value {
    match part {
        PartProjection::Text { id, text } => json!({
            "id": id.to_string(),
            "sessionID": session.to_string(),
            "messageID": message.to_string(),
            "type": "text",
            "text": text,
        }),
        PartProjection::Reasoning { id, text } => json!({
            "id": id.to_string(),
            "sessionID": session.to_string(),
            "messageID": message.to_string(),
            "type": "reasoning",
            "text": text,
        }),
        PartProjection::Tool {
            id,
            call,
            name,
            state,
        } => json!({
            "id": id.to_string(),
            "sessionID": session.to_string(),
            "messageID": message.to_string(),
            "type": "tool",
            "callID": call.to_string(),
            "tool": name.as_str(),
            "state": state,
        }),
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}
