use std::collections::BTreeMap;

use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MessageId, ModelRef, Role, SessionId, TokenUsage,
};
use serde::Serialize;
use serde_json::Value;

use super::message_context_parts::prompt_attachment_parts;
use super::message_parts::{OpenCodePartContext, opencode_parts};

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

    pub(super) fn info(&self) -> Value {
        serde_json::to_value(&self.info).unwrap_or(Value::Null)
    }
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeMessageInfo {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    role: &'static str,
    time: OpenCodeMessageTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<OpenCodeUserModel>,
    #[serde(rename = "parentID", skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    #[serde(rename = "modelID", skip_serializing_if = "Option::is_none")]
    model_id: Option<String>,
    #[serde(rename = "providerID", skip_serializing_if = "Option::is_none")]
    provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<OpenCodeMessagePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens: Option<OpenCodeMessageTokens>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish: Option<FinishReason>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct OpenCodeMessageTime {
    created: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeUserModel {
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "modelID")]
    model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    variant: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeMessagePath {
    cwd: String,
    root: String,
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeMessageTokens {
    input: u64,
    output: u64,
    reasoning: u64,
    cache: OpenCodeMessageTokenCache,
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeMessageTokenCache {
    read: u64,
    write: u64,
}

pub(super) struct OpenCodeMessageContext {
    agent: String,
    provider_id: String,
    model_id: String,
    variant: Option<String>,
    workdir: String,
    times: BTreeMap<MessageId, OpenCodeMessageTime>,
    parts: OpenCodePartContext,
}

impl OpenCodeMessageContext {
    pub(super) fn new(
        agent: &AgentName,
        model: &ModelRef,
        workdir: &str,
        envs: &[Envelope],
    ) -> Self {
        let model = super::model_ref::model_ref_parts(model);
        Self {
            agent: agent.to_string(),
            provider_id: model.provider_id,
            model_id: model.model_id,
            variant: model.variant,
            workdir: workdir.to_string(),
            times: message_times(envs),
            parts: OpenCodePartContext::new(envs),
        }
    }

    fn time(&self, message: MessageId) -> OpenCodeMessageTime {
        self.times.get(&message).copied().unwrap_or_default()
    }

    fn parts(&self) -> &OpenCodePartContext {
        &self.parts
    }
}

pub(super) fn opencode_message(
    session: SessionId,
    message: &hya_proto::MessageProjection,
    context: &OpenCodeMessageContext,
    parent: Option<MessageId>,
) -> OpenCodeMessage {
    let info = match message.role {
        Role::Assistant => assistant_info(session, message, context, parent),
        Role::User | Role::System => user_or_system_info(session, message, context),
    };
    let mut parts = opencode_parts(session, message.id, &message.parts, context.parts());
    parts.extend(prompt_attachment_parts(session, message));
    OpenCodeMessage { info, parts }
}

fn user_or_system_info(
    session: SessionId,
    message: &hya_proto::MessageProjection,
    context: &OpenCodeMessageContext,
) -> OpenCodeMessageInfo {
    OpenCodeMessageInfo {
        id: message.id.to_string(),
        session_id: session.to_string(),
        role: role_name(message.role),
        time: context.time(message.id),
        agent: Some(context.agent.clone()),
        model: Some(OpenCodeUserModel {
            provider_id: context.provider_id.clone(),
            model_id: context.model_id.clone(),
            variant: context.variant.clone(),
        }),
        parent_id: None,
        model_id: None,
        provider_id: None,
        mode: None,
        path: None,
        cost: None,
        tokens: None,
        finish: message.finish,
    }
}

fn assistant_info(
    session: SessionId,
    message: &hya_proto::MessageProjection,
    context: &OpenCodeMessageContext,
    parent: Option<MessageId>,
) -> OpenCodeMessageInfo {
    OpenCodeMessageInfo {
        id: message.id.to_string(),
        session_id: session.to_string(),
        role: "assistant",
        time: context.time(message.id),
        agent: Some(context.agent.clone()),
        model: None,
        parent_id: Some(parent.unwrap_or(message.id).to_string()),
        model_id: Some(context.model_id.clone()),
        provider_id: Some(context.provider_id.clone()),
        mode: Some("build".to_string()),
        path: Some(OpenCodeMessagePath {
            cwd: context.workdir.clone(),
            root: context.workdir.clone(),
        }),
        cost: Some(0),
        tokens: Some(message_tokens(message.tokens)),
        finish: message.finish,
    }
}

fn message_tokens(tokens: Option<TokenUsage>) -> OpenCodeMessageTokens {
    let tokens = tokens.unwrap_or_default();
    OpenCodeMessageTokens {
        input: tokens.input,
        output: tokens.output,
        reasoning: tokens.reasoning,
        cache: OpenCodeMessageTokenCache {
            read: tokens.cache_read,
            write: tokens.cache_write,
        },
    }
}

fn message_times(envs: &[Envelope]) -> BTreeMap<MessageId, OpenCodeMessageTime> {
    let mut out = BTreeMap::new();
    for env in envs {
        match &env.event {
            Event::MessageStarted { message, .. } => {
                out.entry(*message).or_insert(OpenCodeMessageTime {
                    created: millis(env.ts_millis),
                    completed: None,
                });
            }
            Event::MessageFinished { message, .. } => {
                out.entry(*message).or_default().completed = Some(millis(env.ts_millis));
            }
            _ => {}
        }
    }
    out
}

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}
