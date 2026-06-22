use serde::Serialize;
use serde_json::{Number, Value};
use yaca_proto::{AgentName, Envelope, Event, ModelRef, Projection, SessionId};

pub(super) use super::message_projection::OpenCodeMessage;
use super::message_projection::opencode_message;
pub(super) use super::model_ref::{OpenCodeModel, model_info};

pub(super) const REVERT_METADATA_KEY: &str = "_yacaOpenCodeRevert";

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
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    share: Option<OpenCodeSessionShare>,
    time: OpenCodeSessionTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    permission: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revert: Option<OpenCodeSessionRevert>,
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeSessionTime {
    created: u64,
    updated: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    archived: Option<Number>,
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeSessionShare {
    url: String,
}

#[derive(Clone, Debug, Serialize)]
struct OpenCodeSessionRevert {
    #[serde(rename = "messageID")]
    message_id: String,
    #[serde(rename = "partID", skip_serializing_if = "Option::is_none")]
    part_id: Option<String>,
}

impl OpenCodeSessionInfo {
    pub(super) fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
    }

    pub(super) fn title(&self) -> &str {
        &self.title
    }

    pub(super) fn permission(&self) -> Option<&[Value]> {
        self.permission.as_deref()
    }

    pub(super) fn metadata(&self) -> Option<&Value> {
        self.metadata.as_ref()
    }

    pub(super) fn revert(&self) -> bool {
        self.revert.is_some()
    }
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
    let (metadata, revert) = session_metadata(projection.session.metadata.clone());
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
        agent: projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&meta.agent)
            .to_string(),
        model: model_info(projection.session.model.as_ref().unwrap_or(&meta.model)),
        version: env!("CARGO_PKG_VERSION").to_string(),
        metadata,
        share: projection
            .session
            .share
            .as_ref()
            .map(|url| OpenCodeSessionShare { url: url.clone() }),
        time: OpenCodeSessionTime {
            created: millis(created),
            updated: millis(updated),
            archived: projection.session.archived.clone(),
        },
        permission: projection.session.permission.clone(),
        revert,
    }
}

fn session_metadata(metadata: Option<Value>) -> (Option<Value>, Option<OpenCodeSessionRevert>) {
    match metadata {
        Some(Value::Object(mut object)) => {
            let revert = object
                .remove(REVERT_METADATA_KEY)
                .and_then(revert_from_value);
            let metadata = (!object.is_empty()).then_some(Value::Object(object));
            (metadata, revert)
        }
        other => (other, None),
    }
}

fn revert_from_value(value: Value) -> Option<OpenCodeSessionRevert> {
    let object = value.as_object()?;
    let message_id = object.get("messageID")?.as_str()?.to_string();
    let part_id = object
        .get("partID")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    Some(OpenCodeSessionRevert {
        message_id,
        part_id,
    })
}

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}
