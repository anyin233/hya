use hya_core::CreateSession;
use hya_proto::{AgentName, ModelRef, SessionId};
use serde_json::{Map, Value};

use crate::{ApiError, ServerState};

pub(super) async fn project_replay(
    st: &ServerState,
    directory: &str,
    events: &[Value],
) -> Result<(), ApiError> {
    for event in events {
        match event.get("type").and_then(Value::as_str) {
            Some("session.created") => project_created(st, directory, event).await?,
            Some("session.updated") => project_updated(st, directory, event).await?,
            _ => {}
        }
    }
    Ok(())
}

async fn project_created(st: &ServerState, directory: &str, event: &Value) -> Result<(), ApiError> {
    let Some(session) = session_id(event) else {
        return Ok(());
    };
    let Some(info) = info(event) else {
        return Ok(());
    };
    create_session(st, session, directory, info).await?;
    apply_info(st, session, info).await
}

async fn project_updated(st: &ServerState, directory: &str, event: &Value) -> Result<(), ApiError> {
    let Some(session) = session_id(event) else {
        return Ok(());
    };
    let Some(info) = info(event) else {
        return Ok(());
    };
    if st
        .engine
        .store()
        .read_projection(session)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .session
        .id
        .is_none()
    {
        create_session(st, session, directory, info).await?;
    }
    apply_info(st, session, info).await
}

async fn create_session(
    st: &ServerState,
    session: SessionId,
    directory: &str,
    info: &Map<String, Value>,
) -> Result<(), ApiError> {
    let workdir = info
        .get("directory")
        .and_then(Value::as_str)
        .unwrap_or(directory)
        .to_string();
    st.engine
        .create_with_id(
            Some(session),
            CreateSession {
                parent: info
                    .get("parentID")
                    .and_then(Value::as_str)
                    .and_then(|id| id.parse().ok()),
                agent: info
                    .get("agent")
                    .and_then(Value::as_str)
                    .map(AgentName::new)
                    .unwrap_or_else(|| default_agent(st, &workdir)),
                model: model_ref(info).unwrap_or_else(|| st.agent.model.clone()),
                workdir,
            },
        )
        .await?;
    Ok(())
}

fn default_agent(st: &ServerState, workdir: &str) -> AgentName {
    super::agent_catalog::default_name(std::path::Path::new(workdir), st)
        .map(AgentName::new)
        .unwrap_or_else(|| st.agent.name.clone())
}

async fn apply_info(
    st: &ServerState,
    session: SessionId,
    info: &Map<String, Value>,
) -> Result<(), ApiError> {
    if let Some(title) = info.get("title").and_then(Value::as_str) {
        st.engine.set_title(session, title.to_string()).await?;
    }
    if let Some(directory) = info.get("directory").and_then(Value::as_str) {
        st.engine
            .set_workdir(session, directory.to_string())
            .await?;
    }
    if let Some(metadata) = info.get("metadata").filter(|value| !value.is_null()) {
        st.engine.set_metadata(session, metadata.clone()).await?;
    }
    if let Some(permission) = info.get("permission").and_then(Value::as_array) {
        st.engine
            .set_permission(session, permission.clone())
            .await?;
    }
    if let Some(archived) = info
        .get("time")
        .and_then(|time| time.get("archived"))
        .and_then(Value::as_number)
    {
        st.engine.set_archived(session, archived.clone()).await?;
    }
    Ok(())
}

fn session_id(event: &Value) -> Option<SessionId> {
    event
        .get("data")
        .and_then(|data| data.get("sessionID"))
        .and_then(Value::as_str)
        .or_else(|| event.get("aggregateID").and_then(Value::as_str))
        .and_then(|id| id.parse().ok())
}

fn info(event: &Value) -> Option<&Map<String, Value>> {
    event.get("data")?.get("info")?.as_object()
}

fn model_ref(info: &Map<String, Value>) -> Option<ModelRef> {
    let model = info.get("model")?;
    if let Some(model) = model.as_str() {
        return Some(ModelRef::new(model));
    }
    let object = model.as_object()?;
    let id = object.get("id")?.as_str()?;
    let provider = object.get("providerID").and_then(Value::as_str);
    Some(ModelRef::new(match provider {
        Some(provider) => format!("{provider}/{id}"),
        None => id.to_string(),
    }))
}
