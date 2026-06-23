use std::collections::BTreeMap;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use crate::ServerState;

use super::agent_permission::PermissionRule;
use super::location::{LocationInfo, LocationRef, LocationResponse};
use super::model_ref::model_ref_parts;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/location", get(location))
        .route("/api/agent", get(agent))
        .route("/api/command", get(command))
        .route("/api/skill", get(skill))
}

#[derive(Serialize)]
struct AgentInfo {
    id: String,
    model: AgentModelRef,
    request: RequestInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    mode: String,
    hidden: bool,
    permissions: Vec<PermissionRule>,
}

#[derive(Serialize)]
struct AgentModelRef {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    variant: Option<String>,
}

#[derive(Serialize)]
struct RequestInfo {
    headers: BTreeMap<String, String>,
    body: Value,
}

async fn location(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<LocationInfo> {
    let location = LocationRef::from_request(&query, &headers);
    Json(super::location::info_at(&st, &location))
}

async fn agent(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<LocationResponse<Vec<AgentInfo>>> {
    let model = model_ref_parts(&st.agent.model);
    let location = LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    let build_permissions = super::agent_permission::from_engine(&st.engine);
    Json(super::location::response_at(
        &st,
        &location,
        super::agent_catalog::list(&workdir)
            .into_iter()
            .map(|agent| AgentInfo {
                id: agent.name.clone(),
                model: agent
                    .model
                    .as_deref()
                    .map(agent_model_ref)
                    .map(|mut model| {
                        if let Some(variant) = agent.variant {
                            model.variant = Some(variant);
                        }
                        model
                    })
                    .unwrap_or_else(|| AgentModelRef {
                        id: model.model_id.clone(),
                        provider_id: model.provider_id.clone(),
                        variant: model.variant.clone(),
                    }),
                request: RequestInfo {
                    headers: agent.request_headers,
                    body: json!(agent.request_body),
                },
                system: if agent.name == "build" && agent.prompt.is_none() {
                    Some(st.agent.system_prompt.clone())
                } else {
                    agent.prompt
                },
                description: agent.description,
                mode: agent.mode,
                hidden: agent.hidden,
                permissions: agent_permissions(&agent.name, &build_permissions, agent.permissions),
            })
            .collect(),
    ))
}

fn agent_model_ref(model: &str) -> AgentModelRef {
    if let Some((provider_id, model_id)) = model.split_once('/') {
        return AgentModelRef {
            id: model_id.to_string(),
            provider_id: provider_id.to_string(),
            variant: None,
        };
    }
    AgentModelRef {
        id: model.to_string(),
        provider_id: "yaca".to_string(),
        variant: None,
    }
}

fn agent_permissions(
    name: &str,
    build_permissions: &[PermissionRule],
    configured: Vec<PermissionRule>,
) -> Vec<PermissionRule> {
    let mut permissions = if name == "build" {
        build_permissions.to_vec()
    } else {
        Vec::new()
    };
    permissions.extend(configured);
    permissions
}

async fn command(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<LocationResponse<Vec<super::command_catalog::CommandInfo>>> {
    let location = LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    Json(super::location::response_at(
        &st,
        &location,
        super::command_catalog::list(&workdir),
    ))
}

async fn skill(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<LocationResponse<Vec<super::skill_catalog::SkillInfo>>> {
    let location = LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    Json(super::location::response_at(
        &st,
        &location,
        super::skill_catalog::list(&workdir),
    ))
}
