//! `/v1` interaction domain: the unified permission/question pending plane.

use std::collections::BTreeMap;

use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;

use crate::ServerState;
use hya_api::v1 as pb;
use hya_api::v1::respond_interaction_request::Response;
use hya_proto::SessionId;

use super::V1Error;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/interactions", get(list_interactions))
        .route("/v1/interactions/:id/respond", post(respond_interaction))
}

async fn list_interactions(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ListInteractionsResponse>, V1Error> {
    let request: pb::ListInteractionsRequest = super::query_request(&[], &query)?;
    let mut interactions = Vec::new();
    let want_session = if request.session.is_empty() {
        None
    } else {
        Some(
            request
                .session
                .parse::<SessionId>()
                .map_err(|_| V1Error::invalid_argument("invalid session id"))?,
        )
    };
    let want_type = pb::InteractionType::try_from(request.r#type).ok();

    for view in st.permission_requests.list().await {
        let entry = serde_json::to_value(&view).unwrap_or(Value::Null);
        if let Some(want) = want_session
            && field(&entry, "sessionID") != want.to_string()
            && field(&entry, "session") != want.to_string()
        {
            continue;
        }
        if matches_type(want_type, pb::InteractionType::Permission) {
            interactions.push(pb::Interaction {
                id: field(&entry, "id"),
                session: field(&entry, "sessionID")
                    .parse::<SessionId>()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                r#type: pb::InteractionType::Permission as i32,
                title: format!("{} {}", field(&entry, "action"), field(&entry, "resource")),
                detail: String::new(),
                options: Vec::new(),
                payload: None,
                time_created: None,
            });
        }
    }
    for view in st.question_requests.list().await {
        let entry = serde_json::to_value(&view).unwrap_or(Value::Null);
        let session_id = field(&entry, "sessionID");
        if let Some(want) = want_session
            && session_id != want.to_string()
        {
            continue;
        }
        if matches_type(want_type, pb::InteractionType::Question) {
            interactions.push(pb::Interaction {
                id: field(&entry, "id"),
                session: session_id,
                r#type: pb::InteractionType::Question as i32,
                title: field(&entry, "question"),
                detail: String::new(),
                options: Vec::new(),
                payload: None,
                time_created: None,
            });
        }
    }
    let (interactions, page) = super::catalog::paginate(interactions, &request.page);
    Ok(Json(pb::ListInteractionsResponse {
        interactions,
        page: Some(page),
    }))
}

fn matches_type(want: Option<pb::InteractionType>, kind: pb::InteractionType) -> bool {
    want.is_none_or(|want| want == kind)
}

async fn respond_interaction(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::RespondInteractionRequest>,
) -> Result<Json<pb::RespondInteractionResponse>, V1Error> {
    match request.response {
        Some(Response::Permission(permission)) => {
            let view = st
                .permission_requests
                .list()
                .await
                .into_iter()
                .find(|view| {
                    let entry = serde_json::to_value(view).unwrap_or(Value::Null);
                    field(&entry, "id") == id
                });
            let Some(view) = view else {
                return Err(V1Error::new(
                    hya_api::error::Code::NotFound,
                    format!("interaction not found: {id}"),
                ));
            };
            let entry = serde_json::to_value(&view).unwrap_or(Value::Null);
            let session = field(&entry, "sessionID")
                .parse::<SessionId>()
                .map_err(|_| V1Error::internal("pending permission has no session"))?;
            let reply = if permission.allowed {
                if permission.persist {
                    crate::pending::PermissionReply::Always
                } else {
                    crate::pending::PermissionReply::Once
                }
            } else {
                crate::pending::PermissionReply::Reject
            };
            let applied = st
                .permission_requests
                .reply(session, &id, reply, None)
                .await
                .map_err(V1Error::from)?;
            Ok(Json(pb::RespondInteractionResponse { applied }))
        }
        Some(Response::Question(question)) => {
            let views = st.question_requests.list().await;
            let found = views.iter().find(|view| {
                let entry = serde_json::to_value(view).unwrap_or(Value::Null);
                field(&entry, "id") == id
            });
            let Some(found) = found else {
                return Err(V1Error::new(
                    hya_api::error::Code::NotFound,
                    format!("interaction not found: {id}"),
                ));
            };
            let entry = serde_json::to_value(found).unwrap_or(Value::Null);
            let session = field(&entry, "sessionID")
                .parse::<SessionId>()
                .map_err(|_| V1Error::internal("pending question has no session"))?;
            let applied = if question.rejected {
                st.question_requests.reject(session, &id).await
            } else {
                st.question_requests
                    .reply(session, &id, vec![vec![question.answer]])
                    .await
            };
            Ok(Json(pb::RespondInteractionResponse { applied }))
        }
        None => Err(V1Error::invalid_argument("missing interaction response")),
    }
}

fn field(entry: &Value, name: &str) -> String {
    entry
        .get(name)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_default()
}
