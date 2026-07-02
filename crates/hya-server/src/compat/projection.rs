use hya_core::title;
use hya_proto::{AgentName, Envelope, Event, ModelRef, Projection, SessionId, TokenUsage};
use serde::Serialize;
use serde_json::{Number, Value};

pub(super) use super::message_projection::CompatMessage;
use super::message_projection::compat_message;
pub(super) use super::model_ref::{CompatModel, model_info};

pub(super) const REVERT_METADATA_KEY: &str = "_hyaCompatRevert";

#[derive(Clone, Debug, Serialize)]
pub(super) struct CompatSessionInfo {
    id: String,
    slug: String,
    #[serde(rename = "projectID")]
    project_id: String,
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
    directory: String,
    path: String,
    #[serde(rename = "parentID", skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    title: String,
    agent: String,
    model: CompatModel,
    version: String,
    cost: u64,
    tokens: CompatSessionTokens,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    share: Option<CompatSessionShare>,
    time: CompatSessionTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    permission: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revert: Option<CompatSessionRevert>,
    #[serde(skip)]
    empty_unnamed: bool,
}

#[derive(Clone, Debug, Serialize)]
struct CompatSessionTime {
    created: u64,
    updated: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    archived: Option<Number>,
}

#[derive(Clone, Debug, Serialize)]
struct CompatSessionShare {
    url: String,
}

#[derive(Clone, Debug, Default, Serialize)]
struct CompatSessionTokens {
    input: u64,
    output: u64,
    reasoning: u64,
    cache: CompatSessionTokenCache,
}

#[derive(Clone, Debug, Default, Serialize)]
struct CompatSessionTokenCache {
    read: u64,
    write: u64,
}

#[derive(Clone, Debug, Serialize)]
struct CompatSessionRevert {
    #[serde(rename = "messageID")]
    message_id: String,
    #[serde(rename = "partID", skip_serializing_if = "Option::is_none")]
    part_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
}

impl CompatSessionInfo {
    pub(super) fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
    }

    pub(super) fn updated_millis(&self) -> u64 {
        self.time.updated
    }

    pub(super) fn created_millis(&self) -> u64 {
        self.time.created
    }

    pub(super) fn title(&self) -> &str {
        &self.title
    }

    pub(super) fn directory(&self) -> &str {
        &self.directory
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

    pub(super) fn archived(&self) -> bool {
        self.time.archived.is_some()
    }

    pub(super) fn empty_unnamed(&self) -> bool {
        self.empty_unnamed
    }
}

#[derive(Clone, Debug)]
struct SessionCreatedMeta {
    parent: Option<SessionId>,
    agent: AgentName,
    model: ModelRef,
    workdir: String,
}

pub(super) struct CompatSessionSnapshot {
    pub(super) info: CompatSessionInfo,
    pub(super) messages: Vec<CompatMessage>,
}

pub(super) fn snapshot(
    session: SessionId,
    envs: &[Envelope],
    projection: &Projection,
    started_hint: Option<i64>,
) -> Option<CompatSessionSnapshot> {
    let meta = created_meta(envs)?;
    let created = started_hint
        .filter(|ts| *ts > 0)
        .or_else(|| envs.first().map(|env| env.ts_millis))
        .unwrap_or(0);
    let updated = envs.last().map(|env| env.ts_millis).unwrap_or(created);
    let info = session_info(session, projection, &meta, created, updated);
    let message_context = super::message_projection::CompatMessageContext::new(
        projection.session.agent.as_ref().unwrap_or(&meta.agent),
        projection.session.model.as_ref().unwrap_or(&meta.model),
        projection
            .session
            .workdir
            .as_deref()
            .unwrap_or(&meta.workdir),
        envs,
    );
    let mut parent = None;
    let messages = projection
        .session
        .messages
        .iter()
        .map(|message| {
            let out = compat_message(session, message, &message_context, parent);
            parent = Some(message.id);
            out
        })
        .collect();
    Some(CompatSessionSnapshot { info, messages })
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
) -> CompatSessionInfo {
    let id = session.to_string();
    let (metadata, revert) = session_metadata(projection.session.metadata.clone());
    CompatSessionInfo {
        id: id.clone(),
        slug: id,
        project_id: "local".to_string(),
        workspace_id: None,
        directory: projection
            .session
            .workdir
            .clone()
            .unwrap_or_else(|| meta.workdir.clone()),
        path: String::new(),
        parent_id: meta.parent.map(|parent| parent.to_string()),
        title: projection
            .session
            .title
            .clone()
            .unwrap_or_else(|| title::fallback_title(created)),
        agent: projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&meta.agent)
            .to_string(),
        model: model_info(projection.session.model.as_ref().unwrap_or(&meta.model)),
        version: env!("CARGO_PKG_VERSION").to_string(),
        cost: 0,
        tokens: session_tokens(projection),
        metadata,
        share: projection
            .session
            .share
            .as_ref()
            .map(|url| CompatSessionShare { url: url.clone() }),
        time: CompatSessionTime {
            created: millis(created),
            updated: millis(updated),
            archived: projection.session.archived.clone(),
        },
        permission: projection.session.permission.clone(),
        revert,
        empty_unnamed: title::is_empty_unnamed_session(projection),
    }
}

fn session_metadata(metadata: Option<Value>) -> (Option<Value>, Option<CompatSessionRevert>) {
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

fn revert_from_value(value: Value) -> Option<CompatSessionRevert> {
    let object = value.as_object()?;
    let message_id = object.get("messageID")?.as_str()?.to_string();
    let part_id = object
        .get("partID")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    Some(CompatSessionRevert {
        message_id,
        part_id,
        snapshot: None,
        diff: None,
    })
}

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}

fn session_tokens(projection: &Projection) -> CompatSessionTokens {
    let mut total = TokenUsage::default();
    for tokens in projection
        .session
        .messages
        .iter()
        .filter_map(|message| message.tokens)
    {
        total.input = total.input.saturating_add(tokens.input);
        total.output = total.output.saturating_add(tokens.output);
        total.reasoning = total.reasoning.saturating_add(tokens.reasoning);
        total.cache_read = total.cache_read.saturating_add(tokens.cache_read);
        total.cache_write = total.cache_write.saturating_add(tokens.cache_write);
    }
    CompatSessionTokens {
        input: total.input,
        output: total.output,
        reasoning: total.reasoning,
        cache: CompatSessionTokenCache {
            read: total.cache_read,
            write: total.cache_write,
        },
    }
}
