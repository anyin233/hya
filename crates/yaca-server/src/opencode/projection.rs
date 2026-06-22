use serde::Serialize;
use serde_json::{Value, json};
use yaca_proto::{
    AgentName, Envelope, Event, FinishReason, MessageId, ModelRef, PartProjection, Projection,
    Role, SessionId,
};

#[derive(Clone, Debug, Serialize)]
pub(super) struct OpenCodeSessionInfo {
    id: String,
    slug: String,
    #[serde(rename = "projectID")]
    project_id: String,
    directory: String,
    #[serde(rename = "parentID", skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    title: String,
    agent: String,
    model: OpenCodeModel,
    version: String,
    time: OpenCodeSessionTime,
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeModel {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct OpenCodeSessionTime {
    created: u64,
    updated: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct OpenCodeMessage {
    info: OpenCodeMessageInfo,
    parts: Vec<Value>,
}

impl OpenCodeMessage {
    pub(super) fn id(&self) -> &str {
        &self.info.id
    }
}

impl OpenCodeSessionInfo {
    pub(super) fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
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

#[derive(Clone, Debug)]
struct SessionCreatedMeta {
    parent: Option<SessionId>,
    agent: AgentName,
    model: ModelRef,
    workdir: String,
}

pub(super) struct OpenCodeSessionSnapshot {
    pub(super) info: OpenCodeSessionInfo,
    pub(super) messages: Vec<OpenCodeMessage>,
}

pub(super) fn snapshot(
    session: SessionId,
    envs: &[Envelope],
    projection: &Projection,
    started_hint: Option<i64>,
) -> Option<OpenCodeSessionSnapshot> {
    let meta = created_meta(envs)?;
    let created = started_hint
        .filter(|ts| *ts > 0)
        .or_else(|| envs.first().map(|env| env.ts_millis))
        .unwrap_or(0);
    let updated = envs.last().map(|env| env.ts_millis).unwrap_or(created);
    let info = session_info(session, projection, &meta, created, updated);
    let messages = projection
        .session
        .messages
        .iter()
        .map(|message| opencode_message(session, message))
        .collect();
    Some(OpenCodeSessionSnapshot { info, messages })
}

fn created_meta(envs: &[Envelope]) -> Option<SessionCreatedMeta> {
    envs.iter().find_map(|env| {
        if let Event::SessionCreated {
            parent,
            agent,
            model,
            workdir,
            ..
        } = &env.event
        {
            Some(SessionCreatedMeta {
                parent: *parent,
                agent: agent.clone(),
                model: model.clone(),
                workdir: workdir.clone(),
            })
        } else {
            None
        }
    })
}

fn session_info(
    session: SessionId,
    projection: &Projection,
    meta: &SessionCreatedMeta,
    created: i64,
    updated: i64,
) -> OpenCodeSessionInfo {
    let id = session.to_string();
    OpenCodeSessionInfo {
        id: id.clone(),
        slug: id,
        project_id: "local".to_string(),
        directory: meta.workdir.clone(),
        parent_id: meta.parent.map(|parent| parent.to_string()),
        title: projection
            .session
            .title
            .clone()
            .unwrap_or_else(|| "Untitled".to_string()),
        agent: meta.agent.to_string(),
        model: model_info(&meta.model),
        version: env!("CARGO_PKG_VERSION").to_string(),
        time: OpenCodeSessionTime {
            created: millis(created),
            updated: millis(updated),
        },
    }
}

fn model_info(model: &ModelRef) -> OpenCodeModel {
    if let Some((provider, model_id)) = model.as_str().split_once('/') {
        return OpenCodeModel {
            id: model_id.to_string(),
            provider_id: provider.to_string(),
        };
    }
    OpenCodeModel {
        id: model.to_string(),
        provider_id: "yaca".to_string(),
    }
}

fn opencode_message(
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

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}
