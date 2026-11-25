//! `/v1` interaction domain: the unified permission/question pending plane.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::{get, post};

use super::Json;
use serde_json::{Value, json};

use crate::ServerState;
use crate::pending::{PermissionRequestView, QuestionRequestView};
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
    // Unspecified (0) or an unknown value means "every type".
    let want_type = pb::InteractionType::try_from(request.r#type)
        .ok()
        .filter(|kind| *kind != pb::InteractionType::Unspecified);

    if matches_type(want_type, pb::InteractionType::Permission) {
        interactions.extend(
            st.permission_requests
                .list()
                .await
                .into_iter()
                .filter(|view| want_session.is_none_or(|want| view.session == Some(want)))
                .map(pb::Interaction::from),
        );
    }
    if matches_type(want_type, pb::InteractionType::Question) {
        interactions.extend(
            st.question_requests
                .list()
                .await
                .into_iter()
                .filter(|view| want_session.is_none_or(|want| view.session == want))
                .map(pb::Interaction::from),
        );
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
            let session = st
                .permission_requests
                .session_for(&id)
                .await
                .ok_or_else(|| interaction_not_found(&id))?;
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
            let session = st
                .question_requests
                .session_for(&id)
                .await
                .ok_or_else(|| interaction_not_found(&id))?;
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

fn interaction_not_found(id: &str) -> V1Error {
    V1Error::new(
        hya_api::error::Code::NotFound,
        format!("interaction not found: {id}"),
    )
}

impl From<PermissionRequestView> for pb::Interaction {
    fn from(view: PermissionRequestView) -> Self {
        let title = format!("{} {}", view.action, view.resource)
            .trim()
            .to_owned();
        let mut payload = serde_json::Map::from_iter([
            ("action".into(), Value::String(view.action)),
            ("resource".into(), Value::String(view.resource)),
            ("always".into(), json!(view.always)),
        ]);
        if let Some(message) = view.message_id {
            payload.insert("messageId".into(), Value::String(message.to_string()));
        }
        if let Some(call) = view.call_id {
            payload.insert("callId".into(), Value::String(call.to_string()));
        }
        if let Some(tool) = view.tool {
            if !tool.name.is_empty() {
                payload.insert("tool".into(), Value::String(tool.name));
            }
            payload.insert("input".into(), tool.input);
        }
        Self {
            id: view.id,
            session: view
                .session
                .map(|session| session.to_string())
                .unwrap_or_default(),
            r#type: pb::InteractionType::Permission as i32,
            title,
            payload: Some(super::convert::to_struct(Value::Object(payload))),
            ..Self::default()
        }
    }
}

impl From<QuestionRequestView> for pb::Interaction {
    fn from(view: QuestionRequestView) -> Self {
        let (title, detail, options) = view
            .questions
            .first()
            .map(|first| {
                (
                    first.question.clone(),
                    first.header.clone(),
                    first
                        .options
                        .iter()
                        .map(|option| option.label.clone())
                        .collect(),
                )
            })
            .unwrap_or_default();
        Self {
            id: view.id,
            session: view.session.to_string(),
            r#type: pb::InteractionType::Question as i32,
            title,
            detail,
            options,
            payload: Some(super::convert::to_struct(
                json!({ "questions": view.questions }),
            )),
            time_created: None,
        }
    }
}
