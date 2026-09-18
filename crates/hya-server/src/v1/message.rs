//! `/v1` message domain: transcript reads, part deletion, and the todo
//! projection.

use std::collections::BTreeMap;

use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::get;
use axum::{Json, Router};

use crate::ServerState;
use hya_api::v1 as pb;

use super::V1Error;
use super::convert::message as message_info;
use super::session::parse_session;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/sessions/:id/messages", get(list_messages))
        .route("/v1/sessions/:id/messages/:message", get(get_message))
        .route(
            "/v1/sessions/:id/messages/:message/parts/:part",
            axum::routing::delete(delete_message_part),
        )
        .route("/v1/sessions/:id/todo", get(get_session_todo))
}

async fn list_messages(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ListMessagesResponse>, V1Error> {
    let request: pb::ListMessagesRequest =
        super::query_request(&[("session", id.as_str())], &query)?;
    let session = parse_session(&request.session)?;
    let projection = st.engine.read_projection(session).await?;
    if projection.session.id.is_none() {
        return Err(V1Error::session_not_found(&request.session));
    }
    let messages = projection
        .session
        .messages
        .iter()
        .map(|message| {
            let mut info = message_info(message);
            info.session = session.to_string();
            info
        })
        .collect();
    let (messages, page) = super::catalog::paginate(messages, &request.page);
    Ok(Json(pb::ListMessagesResponse {
        messages,
        page: Some(page),
    }))
}

async fn get_message(
    State(st): State<ServerState>,
    AxumPath((id, message)): AxumPath<(String, String)>,
) -> Result<Json<pb::MessageInfo>, V1Error> {
    let session = parse_session(&id)?;
    let projection = st.engine.read_projection(session).await?;
    let found = projection
        .session
        .messages
        .iter()
        .find(|row| row.id.to_string() == message);
    match found {
        Some(row) => {
            let mut info = message_info(row);
            info.session = session.to_string();
            Ok(Json(info))
        }
        None => Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("message not found: {message}"),
        )),
    }
}

async fn delete_message_part(
    State(st): State<ServerState>,
    AxumPath((id, message, part)): AxumPath<(String, String, String)>,
) -> Result<Json<pb::DeleteMessagePartResponse>, V1Error> {
    let session = parse_session(&id)?;
    let message = message
        .parse::<hya_proto::MessageId>()
        .map_err(|_| V1Error::invalid_argument(format!("invalid message id: {message}")))?;
    let part = part
        .parse::<hya_proto::PartId>()
        .map_err(|_| V1Error::invalid_argument(format!("invalid part id: {part}")))?;
    st.engine
        .delete_part(session, message, part)
        .await
        .map_err(V1Error::from)?;
    Ok(Json(pb::DeleteMessagePartResponse {}))
}

async fn get_session_todo(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::TodoList>, V1Error> {
    let session = parse_session(&id)?;
    let projection = st.engine.read_projection(session).await?;
    if projection.session.id.is_none() {
        return Err(V1Error::session_not_found(&id));
    }
    Ok(Json(todo_list(&projection)))
}

/// Map the projected todo rows onto the wire list.
pub(crate) fn todo_list(projection: &hya_proto::Projection) -> pb::TodoList {
    let Some(todos) = todo_source(projection) else {
        return pb::TodoList { items: Vec::new() };
    };
    let items = todos
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| pb::TodoItem {
                    id: field(row, "id"),
                    content: field(row, "content"),
                    status: match field(row, "status").as_str() {
                        "in_progress" => pb::TodoStatus::InProgress as i32,
                        "completed" => pb::TodoStatus::Completed as i32,
                        _ => pb::TodoStatus::Pending as i32,
                    },
                })
                .collect()
        })
        .unwrap_or_default();
    pb::TodoList { items }
}

fn todo_source(projection: &hya_proto::Projection) -> Option<serde_json::Value> {
    projection.session.metadata.as_ref()?.get("todo").cloned()
}

fn field(row: &serde_json::Value, name: &str) -> String {
    row.get(name)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_default()
}
