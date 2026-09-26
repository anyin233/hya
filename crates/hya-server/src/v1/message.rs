//! `/v1` message domain: transcript reads, part deletion, and the todo
//! projection.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::get;

use super::Json;

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
    let items = st.engine.todos(session).await;
    Ok(Json(todo_list(&items)))
}

/// Map the engine's todo rows onto the wire list. Item ids are the plane's
/// stable ids; replayed pre-0.36.53 rows carry synthesized `todo-{index}`
/// ids from the engine fold.
pub(crate) fn todo_list(items: &[hya_tool::TodoItem]) -> pb::TodoList {
    pb::TodoList {
        items: items
            .iter()
            .map(|item| pb::TodoItem {
                id: item.id.clone(),
                content: item.content.clone(),
                status: match item.status {
                    hya_tool::TodoStatus::Pending => pb::TodoStatus::Pending as i32,
                    hya_tool::TodoStatus::InProgress => pb::TodoStatus::InProgress as i32,
                    hya_tool::TodoStatus::Blocked => pb::TodoStatus::Blocked as i32,
                    hya_tool::TodoStatus::Completed => pb::TodoStatus::Completed as i32,
                },
            })
            .collect(),
    }
}
